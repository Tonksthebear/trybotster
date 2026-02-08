//! Timer primitives for Lua scripts.
//!
//! Provides one-shot and repeating timers that fire Lua callbacks.
//! Timers are stored in a shared registry and polled each tick from
//! the Hub loop, similar to file watches.
//!
//! # Design
//!
//! Each `timer.after()` or `timer.every()` creates a `TimerEntry`
//! in the registry. The Hub tick loop calls [`poll_timers`] to check
//! deadlines, fire callbacks, reschedule repeating timers, and remove
//! completed or cancelled entries.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- One-shot timer: fires once after 1000ms
//! local id = timer.after(1000, function()
//!     log.info("Timer fired!")
//! end)
//!
//! -- Repeating timer: fires every 500ms
//! local id = timer.every(500, function()
//!     log.info("Tick!")
//! end)
//!
//! -- Cancel a timer
//! timer.cancel(id)
//! ```
//!
//! # Error Handling
//!
//! `timer.after` and `timer.every` return a timer ID string.
//! `timer.cancel` returns `true` if the timer was found, `false` otherwise.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use mlua::prelude::*;

/// A single timer entry in the registry.
struct TimerEntry {
    /// Lua registry key for the callback function.
    callback_key: LuaRegistryKey,
    /// When this timer should next fire.
    fire_at: Instant,
    /// If `Some`, the timer repeats with this interval.
    repeat_interval: Option<Duration>,
    /// Whether this timer has been cancelled.
    cancelled: bool,
}

/// Registry of active timers.
///
/// Shared between Lua (for creating/cancelling timers) and the Hub tick
/// loop (for polling and firing callbacks).
#[derive(Default)]
pub struct TimerEntries {
    /// Active timers keyed by unique ID.
    entries: Vec<(String, TimerEntry)>,
    /// Counter for generating unique timer IDs.
    next_id: u64,
}

impl std::fmt::Debug for TimerEntries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimerEntries")
            .field("active_count", &self.len())
            .field("next_id", &self.next_id)
            .finish()
    }
}

impl TimerEntries {
    /// Get the number of active (non-cancelled) timers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|(_, e)| !e.cancelled).count()
    }

    /// Check if no active timers exist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Thread-safe handle to the timer registry.
pub type TimerRegistry = Arc<Mutex<TimerEntries>>;

/// Create a new shared timer registry.
#[must_use]
pub fn new_timer_registry() -> TimerRegistry {
    Arc::new(Mutex::new(TimerEntries::default()))
}

/// Register timer primitives with the Lua state.
///
/// Adds the following functions to the global `timer` table:
/// - `timer.after(ms, callback)` -> timer_id (one-shot)
/// - `timer.every(ms, callback)` -> timer_id (repeating)
/// - `timer.cancel(timer_id)` -> boolean
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `registry` - Shared timer registry for storing active timers
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua, registry: TimerRegistry) -> Result<()> {
    let timer_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create timer table: {e}"))?;

    // timer.after(ms, callback) -> timer_id
    //
    // Creates a one-shot timer that fires the callback after `ms` milliseconds.
    let reg = Arc::clone(&registry);
    let after_fn = lua
        .create_function(move |lua, (ms, callback): (u64, LuaFunction)| {
            let callback_key = lua.create_registry_value(callback).map_err(|e| {
                LuaError::external(format!("timer.after: failed to store callback: {e}"))
            })?;

            let mut entries = reg.lock().expect("TimerEntries mutex poisoned");
            let id = format!("timer_{}", entries.next_id);
            entries.next_id += 1;

            entries.entries.push((
                id.clone(),
                TimerEntry {
                    callback_key,
                    fire_at: Instant::now() + Duration::from_millis(ms),
                    repeat_interval: None,
                    cancelled: false,
                },
            ));

            Ok(id)
        })
        .map_err(|e| anyhow!("Failed to create timer.after function: {e}"))?;

    timer_table
        .set("after", after_fn)
        .map_err(|e| anyhow!("Failed to set timer.after: {e}"))?;

    // timer.every(ms, callback) -> timer_id
    //
    // Creates a repeating timer that fires the callback every `ms` milliseconds.
    let reg2 = Arc::clone(&registry);
    let every_fn = lua
        .create_function(move |lua, (ms, callback): (u64, LuaFunction)| {
            let callback_key = lua.create_registry_value(callback).map_err(|e| {
                LuaError::external(format!("timer.every: failed to store callback: {e}"))
            })?;

            let interval = Duration::from_millis(ms);
            let mut entries = reg2.lock().expect("TimerEntries mutex poisoned");
            let id = format!("timer_{}", entries.next_id);
            entries.next_id += 1;

            entries.entries.push((
                id.clone(),
                TimerEntry {
                    callback_key,
                    fire_at: Instant::now() + interval,
                    repeat_interval: Some(interval),
                    cancelled: false,
                },
            ));

            Ok(id)
        })
        .map_err(|e| anyhow!("Failed to create timer.every function: {e}"))?;

    timer_table
        .set("every", every_fn)
        .map_err(|e| anyhow!("Failed to set timer.every: {e}"))?;

    // timer.cancel(timer_id) -> boolean
    //
    // Marks a timer as cancelled. Returns true if the timer was found.
    let reg3 = registry;
    let cancel_fn = lua
        .create_function(move |_, timer_id: String| {
            let mut entries = reg3.lock().expect("TimerEntries mutex poisoned");

            for (id, entry) in &mut entries.entries {
                if *id == timer_id && !entry.cancelled {
                    entry.cancelled = true;
                    return Ok(true);
                }
            }

            Ok(false)
        })
        .map_err(|e| anyhow!("Failed to create timer.cancel function: {e}"))?;

    timer_table
        .set("cancel", cancel_fn)
        .map_err(|e| anyhow!("Failed to set timer.cancel: {e}"))?;

    lua.globals()
        .set("timer", timer_table)
        .map_err(|e| anyhow!("Failed to register timer table globally: {e}"))?;

    Ok(())
}

/// Poll all timers, fire callbacks for elapsed timers, and clean up.
///
/// Called from the Hub tick loop each tick. For each timer:
/// - Skip cancelled timers (remove them)
/// - Check if `fire_at` has elapsed
/// - Fire the callback
/// - For one-shot timers: remove after firing
/// - For repeating timers: reschedule `fire_at` to the next interval
///
/// # Deadlock Prevention
///
/// Fired timer IDs and callback keys are collected under the lock,
/// then the lock is released before calling Lua. This allows callbacks
/// to call `timer.cancel()` or create new timers without deadlocking.
///
/// # Returns
///
/// The number of timer callbacks fired.
pub fn poll_timers(lua: &Lua, registry: &TimerRegistry) -> usize {
    let now = Instant::now();

    // Phase 1: collect fired timers and clean up under the lock.
    let fired_keys: Vec<LuaRegistryKey> = {
        let mut entries = registry.lock().expect("TimerEntries mutex poisoned");

        // Collect callback keys for timers that should fire
        let mut fired = Vec::new();

        for (_, entry) in &mut entries.entries {
            if entry.cancelled {
                continue;
            }

            if now >= entry.fire_at {
                // Clone the callback key for firing outside the lock
                if let Ok(callback) = lua.registry_value::<LuaFunction>(&entry.callback_key) {
                    if let Ok(key) = lua.create_registry_value(callback) {
                        fired.push(key);
                    }
                }

                if let Some(interval) = entry.repeat_interval {
                    // Reschedule repeating timer
                    entry.fire_at = now + interval;
                } else {
                    // Mark one-shot timer for removal
                    entry.cancelled = true;
                }
            }
        }

        // Remove cancelled/completed entries and clean up their registry keys
        let removed: Vec<_> = entries
            .entries
            .drain_filter_compat()
            .into_iter()
            .map(|(_, entry)| entry.callback_key)
            .collect();

        for key in removed {
            let _ = lua.remove_registry_value(key);
        }

        fired
    };
    // Lock released here — callbacks can safely call timer functions.

    // Phase 2: fire callbacks without holding the lock.
    let count = fired_keys.len();

    for key in &fired_keys {
        let result: LuaResult<()> = (|| {
            let callback: LuaFunction = lua.registry_value(key)?;
            callback.call::<()>(())?;
            Ok(())
        })();

        if let Err(e) = result {
            log::warn!("[timer] Callback error: {e}");
        }
    }

    // Phase 3: clean up temporary registry keys.
    for key in fired_keys {
        let _ = lua.remove_registry_value(key);
    }

    count
}

/// Helper trait to emulate `Vec::drain_filter` on stable Rust.
trait DrainFilterCompat<T> {
    fn drain_filter_compat(&mut self) -> Vec<T>;
}

impl DrainFilterCompat<(String, TimerEntry)> for Vec<(String, TimerEntry)> {
    fn drain_filter_compat(&mut self) -> Vec<(String, TimerEntry)> {
        let mut removed = Vec::new();
        let mut i = 0;
        while i < self.len() {
            if self[i].1.cancelled {
                removed.push(self.remove(i));
            } else {
                i += 1;
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_timer_registry() {
        let registry = new_timer_registry();
        let entries = registry.lock().expect("mutex");
        assert!(entries.is_empty());
        assert_eq!(entries.next_id, 0);
    }

    #[test]
    fn test_register_creates_timer_table() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, registry).expect("Should register timer primitives");

        let timer_table: LuaTable = lua
            .globals()
            .get("timer")
            .expect("timer table should exist");
        assert!(timer_table.contains_key("after").expect("key check"));
        assert!(timer_table.contains_key("every").expect("key check"));
        assert!(timer_table.contains_key("cancel").expect("key check"));
    }

    #[test]
    fn test_after_creates_oneshot_timer() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        let id: String = lua
            .load(
                r#"
                return timer.after(1000, function()
                end)
            "#,
            )
            .eval()
            .expect("timer.after should succeed");

        assert!(id.starts_with("timer_"));

        let entries = registry.lock().expect("mutex");
        assert_eq!(entries.entries.len(), 1);
        assert!(entries.entries[0].1.repeat_interval.is_none());
    }

    #[test]
    fn test_every_creates_repeating_timer() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        let id: String = lua
            .load(
                r#"
                return timer.every(500, function()
                end)
            "#,
            )
            .eval()
            .expect("timer.every should succeed");

        assert!(id.starts_with("timer_"));

        let entries = registry.lock().expect("mutex");
        assert_eq!(entries.entries.len(), 1);
        assert_eq!(
            entries.entries[0].1.repeat_interval,
            Some(Duration::from_millis(500))
        );
    }

    #[test]
    fn test_cancel_existing_timer() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.load(
            r#"
            my_timer_id = timer.after(10000, function() end)
        "#,
        )
        .exec()
        .expect("create timer");

        let cancelled: bool = lua
            .load(r#"return timer.cancel(my_timer_id)"#)
            .eval()
            .expect("cancel should succeed");

        assert!(cancelled);

        // Verify timer is marked cancelled
        let entries = registry.lock().expect("mutex");
        assert!(entries.entries[0].1.cancelled);
    }

    #[test]
    fn test_cancel_nonexistent_returns_false() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, registry).expect("Should register");

        let cancelled: bool = lua
            .load(r#"return timer.cancel("nonexistent_id")"#)
            .eval()
            .expect("cancel should not error");

        assert!(!cancelled);
    }

    #[test]
    fn test_poll_timers_empty_registry() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        let count = poll_timers(&lua, &registry);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_poll_fires_elapsed_oneshot() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        // Set up a counter
        lua.load("fired_count = 0")
            .exec()
            .expect("setup counter");

        // Create a timer with 0ms delay (fires immediately)
        lua.load(
            r#"
            timer.after(0, function()
                fired_count = fired_count + 1
            end)
        "#,
        )
        .exec()
        .expect("create timer");

        // Small sleep to ensure the instant has passed
        std::thread::sleep(Duration::from_millis(5));

        let count = poll_timers(&lua, &registry);
        assert_eq!(count, 1);

        let fired: i32 = lua
            .load("return fired_count")
            .eval()
            .expect("read counter");
        assert_eq!(fired, 1);

        // One-shot should be removed after firing
        let entries = registry.lock().expect("mutex");
        assert!(entries.entries.is_empty(), "One-shot timer should be removed after firing");
    }

    #[test]
    fn test_poll_fires_repeating_and_reschedules() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.load("repeat_count = 0")
            .exec()
            .expect("setup counter");

        // Create a repeating timer with 0ms interval
        lua.load(
            r#"
            timer.every(0, function()
                repeat_count = repeat_count + 1
            end)
        "#,
        )
        .exec()
        .expect("create timer");

        std::thread::sleep(Duration::from_millis(5));

        // First poll should fire
        let count = poll_timers(&lua, &registry);
        assert_eq!(count, 1);

        // Repeating timer should still be in the registry
        {
            let entries = registry.lock().expect("mutex");
            assert_eq!(entries.entries.len(), 1, "Repeating timer should still exist");
            assert!(!entries.entries[0].1.cancelled);
        }

        std::thread::sleep(Duration::from_millis(5));

        // Second poll should fire again
        let count = poll_timers(&lua, &registry);
        assert_eq!(count, 1);

        let fired: i32 = lua
            .load("return repeat_count")
            .eval()
            .expect("read counter");
        assert_eq!(fired, 2);
    }

    #[test]
    fn test_poll_skips_cancelled_timers() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.load("should_not_fire = false")
            .exec()
            .expect("setup");

        // Create and immediately cancel
        lua.load(
            r#"
            local id = timer.after(0, function()
                should_not_fire = true
            end)
            timer.cancel(id)
        "#,
        )
        .exec()
        .expect("create and cancel");

        std::thread::sleep(Duration::from_millis(5));

        let count = poll_timers(&lua, &registry);
        assert_eq!(count, 0);

        let fired: bool = lua
            .load("return should_not_fire")
            .eval()
            .expect("read flag");
        assert!(!fired, "Cancelled timer should not fire");
    }

    #[test]
    fn test_poll_does_not_fire_future_timers() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.load("future_fired = false")
            .exec()
            .expect("setup");

        // Create a timer far in the future
        lua.load(
            r#"
            timer.after(60000, function()
                future_fired = true
            end)
        "#,
        )
        .exec()
        .expect("create timer");

        let count = poll_timers(&lua, &registry);
        assert_eq!(count, 0);

        let fired: bool = lua
            .load("return future_fired")
            .eval()
            .expect("read flag");
        assert!(!fired, "Future timer should not fire yet");

        // Timer should still be in registry
        let entries = registry.lock().expect("mutex");
        assert_eq!(entries.entries.len(), 1);
    }

    #[test]
    fn test_unique_timer_ids() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        let id1: String = lua
            .load("return timer.after(10000, function() end)")
            .eval()
            .expect("timer 1");

        let id2: String = lua
            .load("return timer.after(10000, function() end)")
            .eval()
            .expect("timer 2");

        let id3: String = lua
            .load("return timer.every(10000, function() end)")
            .eval()
            .expect("timer 3");

        assert_ne!(id1, id2);
        assert_ne!(id2, id3);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_timer_entries_len_excludes_cancelled() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.load(
            r#"
            timer.after(10000, function() end)
            local id = timer.after(10000, function() end)
            timer.cancel(id)
        "#,
        )
        .exec()
        .expect("create timers");

        let entries = registry.lock().expect("mutex");
        assert_eq!(entries.len(), 1, "len() should exclude cancelled timers");
        assert!(!entries.is_empty());
    }

    #[test]
    fn test_cancel_from_callback_no_deadlock() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.load("cancel_worked = false").exec().expect("setup");

        // Create a timer that cancels another timer from its callback
        lua.load(
            r#"
            local target_id = timer.after(0, function() end)
            timer.after(0, function()
                cancel_worked = timer.cancel(target_id)
            end)
        "#,
        )
        .exec()
        .expect("create timers");

        std::thread::sleep(Duration::from_millis(5));

        // This would deadlock if we held the lock during callbacks
        let _ = poll_timers(&lua, &registry);

        // The cancel-from-callback may or may not succeed depending on
        // which timer fires first, but it must not deadlock
    }
}
