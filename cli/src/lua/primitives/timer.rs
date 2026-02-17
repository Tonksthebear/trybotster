//! Timer primitives for Lua scripts.
//!
//! Provides one-shot and repeating timers that fire Lua callbacks.
//! In production, each timer spawns a tokio task that sends
//! `HubEvent::TimerFired` after the delay. Tests use deadline-based
//! polling via [`poll_timers`] as a fallback.
//!
//! # Usage in Lua
//!
//! ```lua
//! -- One-shot timer: fires once after 1 second
//! local id = timer.after(1, function()
//!     log.info("Timer fired!")
//! end)
//!
//! -- Repeating timer: fires every 0.5 seconds
//! local id = timer.every(0.5, function()
//!     log.info("Tick!")
//! end)
//!
//! -- Cancel a timer
//! timer.cancel(id)
//! ```
//!
//! All durations are in **seconds** (fractional values supported via `f64`).
//!
//! # Error Handling
//!
//! `timer.after` and `timer.every` return a timer ID string.
//! `timer.cancel` returns `true` if the timer was found, `false` otherwise.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use mlua::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::hub::events::HubEvent;

/// A single timer entry in the registry.
struct TimerEntry {
    /// Lua registry key for the callback function.
    callback_key: LuaRegistryKey,
    /// When this timer should next fire (used by test-mode polling).
    fire_at: Instant,
    /// If `Some`, the timer repeats with this interval.
    repeat_interval: Option<Duration>,
    /// Whether this timer has been cancelled.
    cancelled: bool,
    /// Handle for the spawned tokio timer task (production mode).
    ///
    /// `None` in test mode where timers use deadline-based polling via
    /// [`poll_timers`]. Aborted on [`timer.cancel()`] to stop the task.
    task_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Registry of active timers.
///
/// Shared between Lua (for creating/cancelling timers) and the Hub tick
/// loop (for polling and firing callbacks).
pub struct TimerEntries {
    /// Active timers keyed by unique ID.
    entries: Vec<(String, TimerEntry)>,
    /// Counter for generating unique timer IDs.
    next_id: u64,
    /// Event channel for instant timer delivery (production mode).
    ///
    /// When `Some`, `timer.after()` and `timer.every()` spawn tokio tasks
    /// that send [`HubEvent::TimerFired`] instead of relying on deadline
    /// scanning in [`poll_timers`].
    hub_event_tx: Option<UnboundedSender<HubEvent>>,
    /// Tokio runtime handle for spawning timer tasks from sync Lua closures.
    tokio_handle: Option<tokio::runtime::Handle>,
}

impl Default for TimerEntries {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            next_id: 0,
            hub_event_tx: None,
            tokio_handle: None,
        }
    }
}

impl std::fmt::Debug for TimerEntries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimerEntries")
            .field("active_count", &self.len())
            .field("next_id", &self.next_id)
            .field("event_driven", &self.hub_event_tx.is_some())
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

    /// Inject the event channel and tokio handle for instant timer delivery.
    ///
    /// Called once during Hub initialization. After this, `timer.after()` and
    /// `timer.every()` spawn tokio tasks instead of relying on deadline polling.
    pub(crate) fn set_event_channel(
        &mut self,
        tx: UnboundedSender<HubEvent>,
        handle: tokio::runtime::Handle,
    ) {
        self.hub_event_tx = Some(tx);
        self.tokio_handle = Some(handle);
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
/// - `timer.after(seconds, callback)` -> timer_id (one-shot)
/// - `timer.every(seconds, callback)` -> timer_id (repeating)
/// - `timer.cancel(timer_id)` -> boolean
///
/// Durations are in **seconds** (`f64`), converted via
/// [`Duration::from_secs_f64`].
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

    // timer.after(seconds, callback) -> timer_id
    //
    // Creates a one-shot timer that fires the callback after `seconds` seconds.
    // In production (event channel available), spawns a tokio task that sleeps
    // then sends `HubEvent::TimerFired`. In test mode, uses deadline-based polling.
    let reg = Arc::clone(&registry);
    let after_fn = lua
        .create_function(move |lua, (seconds, callback): (f64, LuaFunction)| {
            let callback_key = lua.create_registry_value(callback).map_err(|e| {
                LuaError::external(format!("timer.after: failed to store callback: {e}"))
            })?;

            let mut entries = reg.lock().expect("TimerEntries mutex poisoned");
            let id = format!("timer_{}", entries.next_id);
            entries.next_id += 1;

            let duration = Duration::from_secs_f64(seconds);

            // Spawn tokio task for instant delivery if event channel is available.
            let task_handle = match (&entries.hub_event_tx, &entries.tokio_handle) {
                (Some(tx), Some(handle)) => {
                    let tx = tx.clone();
                    let timer_id = id.clone();
                    Some(handle.spawn(async move {
                        tokio::time::sleep(duration).await;
                        let _ = tx.send(HubEvent::TimerFired { timer_id });
                    }))
                }
                _ => None,
            };

            entries.entries.push((
                id.clone(),
                TimerEntry {
                    callback_key,
                    fire_at: Instant::now() + duration,
                    repeat_interval: None,
                    cancelled: false,
                    task_handle,
                },
            ));

            Ok(id)
        })
        .map_err(|e| anyhow!("Failed to create timer.after function: {e}"))?;

    timer_table
        .set("after", after_fn)
        .map_err(|e| anyhow!("Failed to set timer.after: {e}"))?;

    // timer.every(seconds, callback) -> timer_id
    //
    // Creates a repeating timer that fires the callback every `seconds` seconds.
    // In production, spawns a looping tokio task. In test mode, uses polling.
    let reg2 = Arc::clone(&registry);
    let every_fn = lua
        .create_function(move |lua, (seconds, callback): (f64, LuaFunction)| {
            let callback_key = lua.create_registry_value(callback).map_err(|e| {
                LuaError::external(format!("timer.every: failed to store callback: {e}"))
            })?;

            let interval = Duration::from_secs_f64(seconds);
            let mut entries = reg2.lock().expect("TimerEntries mutex poisoned");
            let id = format!("timer_{}", entries.next_id);
            entries.next_id += 1;

            // Spawn looping tokio task for instant delivery if event channel
            // is available. The task runs until cancelled via `handle.abort()`.
            let task_handle = match (&entries.hub_event_tx, &entries.tokio_handle) {
                (Some(tx), Some(handle)) => {
                    let tx = tx.clone();
                    let timer_id = id.clone();
                    Some(handle.spawn(async move {
                        loop {
                            tokio::time::sleep(interval).await;
                            if tx.send(HubEvent::TimerFired {
                                timer_id: timer_id.clone(),
                            }).is_err() {
                                break;
                            }
                        }
                    }))
                }
                _ => None,
            };

            entries.entries.push((
                id.clone(),
                TimerEntry {
                    callback_key,
                    fire_at: Instant::now() + interval,
                    repeat_interval: Some(interval),
                    cancelled: false,
                    task_handle,
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
    // Marks a timer as cancelled and aborts its spawned task (if any).
    // Returns true if the timer was found.
    let reg3 = registry;
    let cancel_fn = lua
        .create_function(move |_, timer_id: String| {
            let mut entries = reg3.lock().expect("TimerEntries mutex poisoned");

            for (id, entry) in &mut entries.entries {
                if *id == timer_id && !entry.cancelled {
                    entry.cancelled = true;
                    // Abort the spawned timer task if running (production mode).
                    if let Some(handle) = entry.task_handle.take() {
                        handle.abort();
                    }
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

/// Fire the Lua callback for a single timer event.
///
/// Called from [`handle_hub_event`] for [`HubEvent::TimerFired`] events.
/// Looks up the timer entry by ID, clones the callback key, releases the
/// lock, fires the callback, then cleans up.
///
/// For one-shot timers, the entry is marked cancelled and removed.
/// For repeating timers, the entry stays alive (the looping tokio task
/// will send another `TimerFired` after the next interval).
///
/// # Deadlock Prevention
///
/// The registry lock is released before firing the Lua callback, allowing
/// the callback to call `timer.cancel()` or create new timers.
pub(crate) fn fire_single_timer(lua: &Lua, registry: &TimerRegistry, timer_id: &str) {
    // Phase 1: look up entry under lock, clone callback, handle one-shot.
    let callback_key = {
        let mut entries = registry.lock().expect("TimerEntries mutex poisoned");

        let entry_pos = entries
            .entries
            .iter()
            .position(|(id, e)| id == timer_id && !e.cancelled);

        let Some(pos) = entry_pos else {
            // Timer was cancelled between send and receive — race is benign.
            return;
        };

        let entry = &mut entries.entries[pos].1;

        // Clone the callback for firing outside the lock.
        let callback = match lua.registry_value::<LuaFunction>(&entry.callback_key) {
            Ok(cb) => cb,
            Err(e) => {
                log::warn!("[timer] Failed to retrieve callback for {timer_id}: {e}");
                return;
            }
        };
        let cloned_key = match lua.create_registry_value(callback) {
            Ok(k) => k,
            Err(e) => {
                log::warn!("[timer] Failed to clone callback for {timer_id}: {e}");
                return;
            }
        };

        if entry.repeat_interval.is_none() {
            // One-shot: mark for removal.
            entry.cancelled = true;
        }
        // Repeating timers keep the entry alive; the looping task sends again.

        cloned_key
    };
    // Lock released — callback can safely call timer functions.

    // Phase 2: fire callback.
    let result: LuaResult<()> = (|| {
        let callback: LuaFunction = lua.registry_value(&callback_key)?;
        callback.call::<()>(())?;
        Ok(())
    })();

    if let Err(e) = result {
        log::warn!("[timer] Callback error for {timer_id}: {e}");
    }

    // Phase 3: clean up temporary registry key.
    let _ = lua.remove_registry_value(callback_key);

    // Phase 4: remove cancelled entries and clean up their registry keys.
    {
        let mut entries = registry.lock().expect("TimerEntries mutex poisoned");
        let removed: Vec<_> = entries.entries.drain_filter_compat();
        for (_, entry) in removed {
            let _ = lua.remove_registry_value(entry.callback_key);
        }
    }
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
                return timer.after(1.0, function()
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
                return timer.every(0.5, function()
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
            Some(Duration::from_secs_f64(0.5))
        );
    }

    #[test]
    fn test_cancel_existing_timer() {
        let lua = Lua::new();
        let registry = new_timer_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.load(
            r#"
            my_timer_id = timer.after(10, function() end)
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

        // Create a timer with 0s delay (fires immediately)
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

        // Create a repeating timer with 0s interval
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

        // Create a timer far in the future (60 seconds)
        lua.load(
            r#"
            timer.after(60, function()
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
            .load("return timer.after(10, function() end)")
            .eval()
            .expect("timer 1");

        let id2: String = lua
            .load("return timer.after(10, function() end)")
            .eval()
            .expect("timer 2");

        let id3: String = lua
            .load("return timer.every(10, function() end)")
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
            timer.after(10, function() end)
            local id = timer.after(10, function() end)
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
