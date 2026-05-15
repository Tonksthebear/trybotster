//! Hub event loop implementation.
//!
//! Contains the headless run loop for Hub operations. TUI mode is now
//! handled by [`crate::clients::tui::run_with_hub`] to maintain proper layer separation.
//!
//! # Architecture
//!
//! ## Event-Driven Design
//!
//! Both headless and TUI modes use `tokio::select!` to wait for events
//! instead of polling with `thread::sleep`. Hub is `!Send` (Lua VM), so
//! we use `Runtime::block_on()` which runs the future on the calling thread.
//!
//! ## Headless Mode (`run_headless_loop`)
//!
//! For CI/daemon use without a terminal. Hub processes commands and events
//! without any TUI rendering.
//!
//! ## TUI Mode
//!
//! See [`crate::clients::tui::run_with_hub`] - the TUI module coordinates with Hub
//! via channels, with the Hub event loop also using `select!`.

// Rust guideline compliant 2026-02

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::hub::Hub;

/// Poll infrequently so signal-hook shutdown atomics are observed even when
/// the hub is otherwise idle.
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Run the Hub event loop without TUI (headless mode).
///
/// Fully event-driven via `tokio::select!`. Direct channel receivers
/// (PTY input, WebRTC signals, stream frames, worktree results) wake
/// the loop instantly. The unified `HubEvent` channel delivers all
/// background events (HTTP, WebSocket, timers, ActionCable, WebRTC,
/// PTY notifications, file watches, cleanup ticks) with zero latency.
/// A small timeout backstop is still required for signal-hook's atomic
/// shutdown flags because they do not wake `tokio::select!` on their own.
///
/// # Arguments
///
/// * `hub` - The Hub instance to run
/// * `shutdown_flag` - Atomic flag for external shutdown requests (signals)
///
/// # Errors
///
/// Returns an error if the event loop encounters an unrecoverable failure.
pub fn run_headless_loop(hub: &mut Hub, shutdown_flag: &AtomicBool) -> Result<()> {
    log::info!("Hub event loop starting (headless, event-driven)");

    run_event_loop(hub, shutdown_flag, None)?;

    log::info!("Hub headless event loop exiting");
    Ok(())
}

/// Core event loop shared by headless and TUI modes.
///
/// Extracts channel receivers from Hub for `tokio::select!` and drives
/// the async loop via `Runtime::block_on()`. Hub is `!Send` (Lua VM), but
/// `block_on` runs the future on the calling thread — no Send required.
///
/// # Arguments
///
/// * `hub` - The Hub instance
/// * `shutdown_flag` - External shutdown signal (Ctrl+C)
/// * `tui_shutdown` - Optional TUI-initiated shutdown flag
///
/// # Errors
///
/// Returns an error if the event loop encounters an unrecoverable failure.
pub(crate) fn run_event_loop(
    hub: &mut Hub,
    shutdown_flag: &AtomicBool,
    tui_shutdown: Option<&AtomicBool>,
) -> Result<()> {
    hub.webrtc
        .start_queue_forwarders(&hub.tokio_runtime, hub.hub_event_tx.clone());
    let mut worktree_result_rx = hub.worktree_result_rx.take();
    let mut tui_request_rx = hub.tui_request_rx.take();
    let mut hub_event_high_priority_rx = hub.hub_event_high_priority_rx.take();
    let mut hub_event_rx = hub.hub_event_rx.take();

    // Spawn a cleanup interval task that sends CleanupTick every 5 seconds.
    // This replaces the periodic timer in the select! loop, allowing the
    // event loop to sleep fully between real events.
    let cleanup_tx = hub.hub_event_tx.clone();
    let cleanup_handle = hub.tokio_runtime.handle().spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if cleanup_tx
                .send(super::events::HubEvent::CleanupTick)
                .is_err()
            {
                break; // Hub shut down
            }
        }
    });

    // Clone the runtime handle before entering async context.
    // block_on() drives the tokio reactor on the current (main) thread.
    // Hub is !Send (Lua VM), but block_on doesn't require Send — the future
    // executes entirely on this thread. Spawned tasks run on worker threads.
    let rt_handle = hub.tokio_runtime.handle().clone();
    rt_handle.block_on(async {
        loop {
            // select! with biased: check high-priority channels first
            tokio::select! {
                biased;

                // TUI requests (keyboard input, Lua messages)
                Some(req) = async {
                    match tui_request_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    hub.handle_tui_request(req);
                }

                // Worktree creation results
                Some(result) = async {
                    match worktree_result_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    hub.handle_worktree_result(result);
                }

                // Latency-sensitive client/control events.
                Some(event) = async {
                    match hub_event_high_priority_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    dispatch_hub_event(hub, event);
                }

                // Unified event bus (all events including cleanup ticks)
                Some(event) = async {
                    match hub_event_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    dispatch_hub_event(hub, event);
                }

                // signal-hook only flips atomics; it does not wake this select loop.
                _ = tokio::time::sleep(SHUTDOWN_POLL_INTERVAL) => {}
            }

            // Fairness drain: biased select above keeps hub_event last.
            // Under sustained load on higher-priority arms, hub events
            // (cleanup ticks, lifecycle, shutdown propagation from hub.quit)
            // can go unserved. Drain
            // a small bounded batch inline after each select iteration so
            // hub events make progress even when a high-volume arm is hot.
            //
            // A tight bound keeps iteration latency predictable for the
            // keystroke fast path; 4 is small enough to be invisible and
            // large enough that a backlog clears quickly across iterations.
            const FAIRNESS_DRAIN_LIMIT: usize = 4;
            if let Some(ref mut rx) = hub_event_high_priority_rx {
                for _ in 0..FAIRNESS_DRAIN_LIMIT {
                    match rx.try_recv() {
                        Ok(event) => {
                            dispatch_hub_event(hub, event);
                        }
                        Err(_) => break,
                    }
                }
            }
            if let Some(ref mut rx) = hub_event_rx {
                for _ in 0..FAIRNESS_DRAIN_LIMIT {
                    match rx.try_recv() {
                        Ok(event) => {
                            dispatch_hub_event(hub, event);
                        }
                        Err(_) => break,
                    }
                }
            }

            // Check shutdown conditions
            if hub.quit || shutdown_flag.load(Ordering::SeqCst) {
                break;
            }
            if let Some(flag) = tui_shutdown {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
            }
        }
    });

    // Stop the cleanup interval task.
    cleanup_handle.abort();

    hub.worktree_result_rx = worktree_result_rx;
    hub.tui_request_rx = tui_request_rx;
    hub.hub_event_high_priority_rx = hub_event_high_priority_rx;
    hub.hub_event_rx = hub_event_rx;

    Ok(())
}

fn dispatch_hub_event(hub: &mut Hub, event: super::events::HubEvent) {
    let kind = event.kind();
    let bytes = event.approx_size_bytes();
    hub.hub_event_metrics.record_dequeue(kind, bytes);
    hub.hub_event_tx.mark_dequeued(&event);
    let started_at = Instant::now();
    hub.handle_hub_event(event);
    hub.hub_event_metrics
        .record_handler_time(kind, started_at.elapsed());
}
