//! Shutdown watchdog — guarantees the process exits after SIGTERM/SIGINT/SIGHUP
//! even when the event loop is wedged.
//!
//! # Why this exists
//!
//! Hub signal handling goes: signal-hook flips `SHUTDOWN_FLAG` → event loop
//! checks the flag between `tokio::select!` arms → loop exits → `Hub::shutdown`
//! runs → process exits.
//!
//! If a handler never returns (a Lua hook that loops forever, a primitive that
//! blocks on a poisoned mutex), the flag never gets checked and SIGTERM is
//! silently ignored. Only `kill -9` recovers.
//!
//! The watchdog is the last-ditch escape hatch: a dedicated OS thread that polls
//! the flag, starts a grace timer when it flips, and calls `process::exit` if the
//! normal path hasn't terminated by then. Normal shutdown wins the race on a
//! healthy hub — the process exits and takes the watchdog with it.
//!
//! # Scope
//!
//! This is a mitigation, not a fix. The event loop can still wedge — this just
//! ensures the operator can stop it without `kill -9`. The underlying hook
//! timeout enforcement (see `cli/lua/hub/hooks.lua`) is the real fix.
//
// Rust guideline compliant 2026-04

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::thread;
use std::time::{Duration, Instant};

/// Grace window between `SHUTDOWN_FLAG` flipping and forced exit.
///
/// A healthy hub finishes `Hub::shutdown` in well under a second. 15s is generous
/// enough to tolerate slow cleanup (WebRTC teardown, PTY kills, Lua shutdown
/// hooks) while still providing a prompt exit when the event loop is wedged.
const GRACE_PERIOD: Duration = Duration::from_secs(15);

/// How often the watchdog polls `SHUTDOWN_FLAG`. Coarse enough that CPU overhead
/// is negligible, fine enough that shutdown-detection latency is invisible.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Ensures the watchdog thread is spawned at most once even if multiple code
/// paths register the same flag (headless, TUI, attach).
static SPAWN_ONCE: Once = Once::new();

/// Spawn the shutdown watchdog.
///
/// Idempotent: repeated calls are no-ops. The watchdog thread is named
/// `shutdown-watchdog` and detaches; the OS reclaims it on process exit.
///
/// # Example
///
/// ```ignore
/// use botster::shutdown;
/// use signal_hook::flag;
/// use signal_hook::consts::signal::{SIGINT, SIGTERM};
///
/// flag::register(SIGINT, Arc::clone(&SHUTDOWN_FLAG))?;
/// flag::register(SIGTERM, Arc::clone(&SHUTDOWN_FLAG))?;
/// shutdown::spawn(Arc::clone(&SHUTDOWN_FLAG));
/// ```
pub fn spawn(shutdown_flag: Arc<AtomicBool>) {
    SPAWN_ONCE.call_once(|| {
        let result = thread::Builder::new()
            .name("shutdown-watchdog".to_string())
            .spawn(move || {
                run(&shutdown_flag, GRACE_PERIOD, || std::process::exit(1));
            });

        if let Err(e) = result {
            // Spawning the watchdog failed; log and continue. The process is
            // still runnable without it — we just lose the forced-exit backstop.
            log::warn!("Failed to spawn shutdown watchdog thread: {e}");
        }
    });
}

/// Core watchdog loop.
///
/// Blocks until `shutdown_flag` becomes true, waits for `grace` to elapse, then
/// invokes `on_expire`. The production entry point passes `std::process::exit`;
/// tests pass a no-side-effect handler so they can verify timing.
fn run<F>(shutdown_flag: &AtomicBool, grace: Duration, on_expire: F)
where
    F: FnOnce(),
{
    while !shutdown_flag.load(Ordering::SeqCst) {
        thread::sleep(POLL_INTERVAL);
    }

    log::warn!(
        "Shutdown signal received; watchdog will force exit in {grace:?} if graceful shutdown stalls"
    );

    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        thread::sleep(POLL_INTERVAL);
    }

    // Reaching here means normal shutdown stalled — the event loop is wedged
    // (e.g., deadlocked Lua hook). Exit abruptly so the operator doesn't have
    // to reach for `kill -9`.
    log::error!(
        "Graceful shutdown did not complete within {grace:?}; forcing process exit (watchdog)"
    );
    on_expire();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end timing: flag flips → watchdog waits grace → on_expire fires.
    #[test]
    fn run_waits_for_flag_then_invokes_on_expire_after_grace() {
        let flag = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));

        let flag_for_thread = Arc::clone(&flag);
        let fired_for_thread = Arc::clone(&fired);
        let grace = Duration::from_millis(200);

        let handle = thread::spawn(move || {
            run(&flag_for_thread, grace, move || {
                fired_for_thread.store(true, Ordering::SeqCst);
            });
        });

        // Before the flag flips, on_expire must not have fired.
        thread::sleep(Duration::from_millis(50));
        assert!(
            !fired.load(Ordering::SeqCst),
            "on_expire fired before flag was set"
        );

        // Flip the flag; grace period starts.
        let start = Instant::now();
        flag.store(true, Ordering::SeqCst);

        // Within the grace window it must still not have fired. Observe near
        // the start of the window to avoid racing the boundary.
        thread::sleep(grace / 4);
        assert!(
            !fired.load(Ordering::SeqCst),
            "on_expire fired before grace period elapsed"
        );

        handle.join().expect("watchdog thread completes");

        let elapsed = start.elapsed();
        assert!(
            fired.load(Ordering::SeqCst),
            "on_expire must fire after grace expires"
        );
        assert!(
            elapsed >= grace,
            "on_expire fired before the grace window closed ({elapsed:?} < {grace:?})"
        );
        // Loose upper bound: grace plus a generous polling slop. Catches
        // runaway sleeps without being flaky on loaded CI.
        assert!(
            elapsed < grace + Duration::from_millis(500),
            "on_expire fired too late ({elapsed:?})"
        );
    }

    /// When the flag stays false, the watchdog sits in its poll loop and
    /// on_expire never fires.
    #[test]
    fn run_does_not_fire_while_flag_is_unset() {
        // We cannot let `run` return without flipping the flag, so this test
        // runs the loop in a detached thread and observes that `fired` stays
        // false for longer than any reasonable polling cycle.
        let flag = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));

        let flag_for_thread = Arc::clone(&flag);
        let fired_for_thread = Arc::clone(&fired);

        thread::Builder::new()
            .name("watchdog-unset-probe".to_string())
            .spawn(move || {
                run(&flag_for_thread, Duration::from_millis(100), move || {
                    fired_for_thread.store(true, Ordering::SeqCst);
                });
            })
            .expect("spawn probe");

        // Several poll intervals without flipping the flag.
        thread::sleep(POLL_INTERVAL * 5);
        assert!(
            !fired.load(Ordering::SeqCst),
            "on_expire must not fire while flag is unset"
        );
        // Thread continues to poll forever; `Arc` drop does not wake it. That
        // is acceptable — the process would terminate in real use before this
        // ever mattered.
    }

    /// Repeated `spawn` calls must not start multiple watchdog threads.
    #[test]
    fn spawn_is_idempotent() {
        let flag = Arc::new(AtomicBool::new(false));
        spawn(Arc::clone(&flag));
        spawn(Arc::clone(&flag));
        spawn(flag);
        // Nothing to assert directly — the SPAWN_ONCE gate is internal. The
        // test passes if no panic occurs and the test binary does not exit
        // unexpectedly (force-exit would terminate the whole test harness).
        thread::sleep(Duration::from_millis(50));
    }
}
