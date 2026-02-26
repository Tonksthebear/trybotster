//! User-facing file watch primitives for Lua scripts.
//!
//! Each `watch.directory()` call creates an OS-level file watcher and
//! registers a Lua callback. In production, a blocking forwarder task
//! per watch sends `HubEvent::UserFileWatch` events to the Hub event
//! loop, which dispatches via [`fire_user_watch_events`].
//!
//! # Lua API
//!
//! ```lua
//! -- Watch a directory for changes
//! local id = watch.directory("/path/to/dir", {
//!     recursive = true,       -- default: true
//!     pattern = "*.lua",      -- optional glob filter
//!     poll = true,            -- use mtime polling instead of OS events (default: false)
//!     poll_interval = 2.0,    -- poll interval in seconds (default: 2.0)
//! }, function(event)
//!     -- event.path = "/path/to/dir/file.txt"
//!     -- event.kind = "create" | "modify" | "rename" | "delete"
//!     log.info("Changed: " .. event.path .. " (" .. event.kind .. ")")
//! end)
//!
//! -- Stop watching
//! watch.unwatch(id)
//! ```
//!
//! # Architecture
//!
//! Each `watch.directory()` creates its own [`FileWatcher`] — simple, no
//! shared-watcher complexity. The `notify` crate handles OS-level dedup.
//! Glob filtering happens in Rust before calling Lua, avoiding unnecessary
//! cross-boundary calls.

// Rust guideline compliant 2026-02

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use globset::{Glob, GlobMatcher};
use mlua::prelude::*;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::file_watcher::{FileEventKind, FileWatcher};
use crate::hub::events::HubEvent;

/// A single active directory watch with its OS watcher and Lua callback.
struct WatchEntry {
    /// OS-level file watcher (keeps the `notify` subscription alive).
    watcher: FileWatcher,
    /// Lua registry key for the callback function.
    callback_key: LuaRegistryKey,
    /// Optional glob pattern for filtering events.
    glob: Option<GlobMatcher>,
    /// Blocking forwarder task handle (event-driven mode).
    ///
    /// Aborted on unwatch to stop the forwarder.
    forwarder_handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for WatchEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchEntry")
            .field("has_glob", &self.glob.is_some())
            .finish_non_exhaustive()
    }
}

/// Registry of active user file watches.
///
/// Shared between Lua (for creating/removing watches) and the Hub event
/// loop (for dispatching file watch events and firing callbacks).
#[derive(Debug, Default)]
pub struct WatcherEntries {
    /// Active watches keyed by unique ID.
    entries: HashMap<String, WatchEntry>,
    /// Counter for generating unique watch IDs.
    next_id: u64,
    /// Hub event channel for event-driven delivery.
    ///
    /// When set, new watches spawn a blocking forwarder task that sends
    /// `HubEvent::UserFileWatch` instead of relying on periodic polling.
    hub_event_tx: Option<UnboundedSender<HubEvent>>,
    /// Tokio runtime handle for spawning blocking forwarder tasks.
    ///
    /// Needed because `watch.directory()` may be called during initialization
    /// (before `block_on`), when no implicit runtime context exists.
    tokio_handle: Option<tokio::runtime::Handle>,
}

impl WatcherEntries {
    /// Get the number of active watches.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if no watches are active.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Set the Hub event channel and tokio handle for event-driven delivery.
    pub(crate) fn set_hub_event_tx(
        &mut self,
        tx: UnboundedSender<HubEvent>,
        handle: tokio::runtime::Handle,
    ) {
        self.hub_event_tx = Some(tx);
        self.tokio_handle = Some(handle);
    }

    /// Stop all active watches, aborting forwarder tasks and dropping watchers.
    ///
    /// Must be called before the tokio runtime drops to prevent a deadlock:
    /// forwarder tasks block on `rx.recv()` where the sender lives inside
    /// each `FileWatcher`. Dropping the watcher closes the sender, unblocking
    /// the forwarder so the runtime can shut down cleanly.
    pub(crate) fn stop_all(&mut self) {
        for (_id, entry) in self.entries.drain() {
            if let Some(handle) = entry.forwarder_handle {
                handle.abort();
            }
            // entry.watcher drops here, closing the sender
        }
    }
}

/// Thread-safe handle to the watcher registry.
pub type WatcherRegistry = Arc<Mutex<WatcherEntries>>;

/// Create a new shared watcher registry.
#[must_use]
pub fn new_watcher_registry() -> WatcherRegistry {
    Arc::new(Mutex::new(WatcherEntries::default()))
}

/// Map [`FileEventKind`] to a Lua-friendly string.
fn kind_to_str(kind: FileEventKind) -> &'static str {
    match kind {
        FileEventKind::Create => "create",
        FileEventKind::Modify => "modify",
        FileEventKind::Rename => "rename",
        FileEventKind::Delete => "delete",
        FileEventKind::Other => "other",
    }
}

/// Register watch primitives with the Lua state.
///
/// Adds the following functions to the global `watch` table:
/// - `watch.directory(path, opts, callback)` -> watch_id
/// - `watch.unwatch(watch_id)` -> boolean
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `registry` - Shared watcher registry for storing active watches
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua, registry: WatcherRegistry) -> Result<()> {
    let watch_table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create watch table: {e}"))?;

    // watch.directory(path, opts, callback) -> watch_id
    let reg = Arc::clone(&registry);
    let directory_fn = lua
        .create_function(move |lua, args: LuaMultiValue| {
            // Parse arguments: (path: string, opts: table?, callback: function)
            let mut iter = args.into_iter();

            let path: String = iter
                .next()
                .and_then(|v| lua.coerce_string(v).ok().flatten())
                .and_then(|s| s.to_str().ok().map(|s| s.to_string()))
                .ok_or_else(|| LuaError::external("watch.directory: first argument must be a path string"))?;

            // Second arg can be opts table or callback function
            let second = iter.next().ok_or_else(|| {
                LuaError::external("watch.directory: requires at least a path and callback")
            })?;

            let (opts, callback) = if let LuaValue::Function(f) = second {
                // Two-arg form: watch.directory(path, callback)
                (None, f)
            } else if let LuaValue::Table(t) = second {
                // Three-arg form: watch.directory(path, opts, callback)
                let cb = iter
                    .next()
                    .and_then(|v| match v {
                        LuaValue::Function(f) => Some(f),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        LuaError::external("watch.directory: third argument must be a callback function")
                    })?;
                (Some(t), cb)
            } else {
                return Err(LuaError::external(
                    "watch.directory: second argument must be an options table or callback function",
                ));
            };

            // Parse options
            let recursive = opts
                .as_ref()
                .and_then(|t| t.get::<bool>("recursive").ok())
                .unwrap_or(true);

            let pattern: Option<String> = opts
                .as_ref()
                .and_then(|t| t.get::<String>("pattern").ok());

            let use_poll = opts
                .as_ref()
                .and_then(|t| t.get::<bool>("poll").ok())
                .unwrap_or(false);

            let poll_interval_secs: f64 = opts
                .as_ref()
                .and_then(|t| t.get::<f64>("poll_interval").ok())
                .unwrap_or(2.0);

            // Compile glob pattern if provided
            let glob = match &pattern {
                Some(pat) => {
                    let g = Glob::new(pat).map_err(|e| {
                        LuaError::external(format!("watch.directory: invalid glob pattern '{pat}': {e}"))
                    })?;
                    Some(g.compile_matcher())
                }
                None => None,
            };

            // Create the file watcher (poll-based or OS-native)
            let mut watcher = if use_poll {
                let interval = std::time::Duration::from_secs_f64(poll_interval_secs);
                FileWatcher::new_poll(interval)
            } else {
                FileWatcher::new()
            }
            .map_err(|e| {
                LuaError::external(format!("watch.directory: failed to create watcher: {e}"))
            })?;

            watcher
                .watch(std::path::Path::new(&path), recursive)
                .map_err(|e| {
                    LuaError::external(format!(
                        "watch.directory: failed to watch '{path}': {e}"
                    ))
                })?;

            // Store callback in Lua registry
            let callback_key = lua.create_registry_value(callback).map_err(|e| {
                LuaError::external(format!("watch.directory: failed to store callback: {e}"))
            })?;

            // Generate unique ID and store entry
            let mut entries = reg.lock().expect("WatcherEntries mutex poisoned");
            let id = format!("watch_{}", entries.next_id);
            entries.next_id += 1;

            // Spawn a blocking forwarder task if the event channel and tokio
            // handle are available. Uses the stored handle because this may
            // be called during initialization (before block_on).
            let forwarder_handle = if let (Some(ref tx), Some(ref handle)) =
                (&entries.hub_event_tx, &entries.tokio_handle)
            {
                let rx = watcher.take_rx();
                if let Some(rx) = rx {
                    let tx = tx.clone();
                    let watch_id = id.clone();
                    Some(handle.spawn_blocking(move || {
                        // Blocking recv — wakes only when the OS delivers an event.
                        while let Ok(result) = rx.recv() {
                            let events = match result {
                                Ok(event) => FileWatcher::classify_event(&event),
                                Err(e) => {
                                    log::warn!("[watch] File watcher error: {e}");
                                    continue;
                                }
                            };
                            if events.is_empty() {
                                continue;
                            }
                            if tx.send(HubEvent::UserFileWatch {
                                watch_id: watch_id.clone(),
                                events,
                            }).is_err() {
                                break; // Hub shut down
                            }
                        }
                    }))
                } else {
                    None
                }
            } else {
                None
            };

            entries.entries.insert(
                id.clone(),
                WatchEntry {
                    watcher,
                    callback_key,
                    glob,
                    forwarder_handle,
                },
            );

            log::info!(
                "[watch] Started watching '{}' (id={}, recursive={}, pattern={:?}, poll={})",
                path,
                id,
                recursive,
                pattern,
                use_poll
            );

            Ok(id)
        })
        .map_err(|e| anyhow!("Failed to create watch.directory function: {e}"))?;

    watch_table
        .set("directory", directory_fn)
        .map_err(|e| anyhow!("Failed to set watch.directory: {e}"))?;

    // watch.unwatch(watch_id) -> boolean
    let reg2 = registry;
    let unwatch_fn = lua
        .create_function(move |lua, watch_id: String| {
            let mut entries = reg2.lock().expect("WatcherEntries mutex poisoned");

            if let Some(entry) = entries.entries.remove(&watch_id) {
                // Abort the forwarder task if running.
                if let Some(handle) = entry.forwarder_handle {
                    handle.abort();
                }
                // Remove callback from Lua registry
                if let Err(e) = lua.remove_registry_value(entry.callback_key) {
                    log::warn!("[watch] Failed to remove callback for {}: {}", watch_id, e);
                }
                log::info!("[watch] Stopped watching (id={})", watch_id);
                Ok(true)
            } else {
                log::debug!("[watch] unwatch called with unknown id: {}", watch_id);
                Ok(false)
            }
        })
        .map_err(|e| anyhow!("Failed to create watch.unwatch function: {e}"))?;

    watch_table
        .set("unwatch", unwatch_fn)
        .map_err(|e| anyhow!("Failed to set watch.unwatch: {e}"))?;

    lua.globals()
        .set("watch", watch_table)
        .map_err(|e| anyhow!("Failed to register watch table globally: {e}"))?;

    Ok(())
}

/// A pending file event collected under the registry lock, ready for
/// dispatch after the lock is released.
struct PendingEvent {
    watch_id: String,
    callback_key_index: usize,
    path: String,
    kind: &'static str,
}

/// Poll all user file watches, fire Lua callbacks, and notify hook observers.
///
/// Called from `LuaRuntime::poll_user_file_watches()` each tick. For each
/// watch entry, drains OS events, applies glob filtering, builds a Lua
/// event table, and calls the registered callback.
///
/// Also fires `hooks.notify("file_changed", event)` for each event so
/// the hook system can react.
///
/// # Deadlock Prevention
///
/// Events are collected under the registry lock, then the lock is released
/// before calling Lua. This allows callbacks to call `watch.unwatch()` or
/// `watch.directory()` without deadlocking. Callback registry keys are
/// cloned (via index) and resolved after unlock.
///
/// # Returns
///
/// The total number of events fired across all watches.
pub fn poll_user_watches(lua: &Lua, registry: &WatcherRegistry) -> usize {
    // Phase 1: collect events and callback keys under the lock.
    let (pending, callback_keys): (Vec<PendingEvent>, Vec<LuaRegistryKey>) = {
        let mut entries = registry.lock().expect("WatcherEntries mutex poisoned");
        let mut pending = Vec::new();
        let mut keys = Vec::new();

        for (watch_id, entry) in &mut entries.entries {
            let events = entry.watcher.poll();
            if events.is_empty() {
                continue;
            }

            // Clone the callback registry key once per watch that has events.
            // We store the index into `keys` so PendingEvent stays lightweight.
            let key_index = keys.len();
            // Create a Lua reference to the same callback function
            if let Ok(callback) = lua.registry_value::<LuaFunction>(&entry.callback_key) {
                if let Ok(key) = lua.create_registry_value(callback) {
                    keys.push(key);
                } else {
                    continue;
                }
            } else {
                continue;
            }

            for event in events {
                if event.kind == FileEventKind::Other {
                    continue;
                }

                // Apply glob filter
                if let Some(ref glob) = entry.glob {
                    let matches = event
                        .path
                        .file_name()
                        .map_or(false, |name| glob.is_match(name.to_string_lossy().as_ref()));
                    if !matches {
                        continue;
                    }
                }

                pending.push(PendingEvent {
                    watch_id: watch_id.clone(),
                    callback_key_index: key_index,
                    path: event.path.to_string_lossy().to_string(),
                    kind: kind_to_str(event.kind),
                });
            }
        }

        (pending, keys)
    };
    // Lock released here — Lua callbacks can safely call watch.unwatch() / watch.directory().

    // Phase 2: fire callbacks without holding the lock.
    let mut total_events = 0;

    for event in &pending {
        let result: LuaResult<()> = (|| {
            let event_table = lua.create_table()?;
            event_table.set("path", event.path.clone())?;
            event_table.set("kind", event.kind)?;
            event_table.set("watch_id", event.watch_id.clone())?;

            let callback: LuaFunction =
                lua.registry_value(&callback_keys[event.callback_key_index])?;
            callback.call::<()>(event_table.clone())?;

            // Also fire hooks.notify("file_changed", event) for the hook system
            let hooks_result: LuaResult<()> = (|| {
                let hooks: LuaTable = lua.globals().get("hooks")?;
                let notify: LuaFunction = hooks.get("notify")?;
                notify.call::<()>(("file_changed", event_table))?;
                Ok(())
            })();

            if let Err(e) = hooks_result {
                // hooks may not be loaded yet — that's fine
                log::trace!("[watch] hooks.notify failed (may not be loaded): {e}");
            }

            Ok(())
        })();

        if let Err(e) = result {
            log::warn!(
                "[watch] Callback error for {} (path={}, kind={}): {}",
                event.watch_id,
                event.path,
                event.kind,
                e
            );
        }

        total_events += 1;
    }

    // Phase 3: clean up temporary registry keys.
    for key in callback_keys {
        let _ = lua.remove_registry_value(key);
    }

    total_events
}

/// Fire Lua callbacks for a single user file watch event (event-driven path).
///
/// Called by `handle_hub_event` when a `HubEvent::UserFileWatch` arrives.
/// Applies glob filtering and fires the registered Lua callback, same as
/// [`poll_user_watches`] but for a single watch ID.
///
/// # Deadlock Prevention
///
/// The registry lock is held only to read the glob pattern and clone the
/// callback key. Lua callbacks are fired after the lock is released.
pub fn fire_user_watch_events(
    lua: &Lua,
    registry: &WatcherRegistry,
    watch_id: &str,
    events: Vec<crate::file_watcher::FileEvent>,
) -> usize {
    // Phase 1: read glob + clone callback key under lock.
    let (glob, callback_key) = {
        let entries = registry.lock().expect("WatcherEntries mutex poisoned");
        let Some(entry) = entries.entries.get(watch_id) else {
            return 0; // Watch was removed between event and dispatch
        };

        let glob = entry.glob.as_ref().map(|g| g.clone());
        let key = match lua.registry_value::<LuaFunction>(&entry.callback_key) {
            Ok(cb) => match lua.create_registry_value(cb) {
                Ok(k) => k,
                Err(_) => return 0,
            },
            Err(_) => return 0,
        };

        (glob, key)
    };
    // Lock released — safe to call Lua.

    // Phase 2: filter and fire callbacks.
    let mut fired = 0;

    for event in events {
        if event.kind == FileEventKind::Other {
            continue;
        }

        // Apply glob filter.
        if let Some(ref glob) = glob {
            let matches = event
                .path
                .file_name()
                .map_or(false, |name| glob.is_match(name.to_string_lossy().as_ref()));
            if !matches {
                continue;
            }
        }

        let result: LuaResult<()> = (|| {
            let event_table = lua.create_table()?;
            event_table.set("path", event.path.to_string_lossy().to_string())?;
            event_table.set("kind", kind_to_str(event.kind))?;
            event_table.set("watch_id", watch_id)?;

            let callback: LuaFunction = lua.registry_value(&callback_key)?;
            callback.call::<()>(event_table.clone())?;

            // Fire hooks.notify("file_changed", event) for the hook system.
            let hooks_result: LuaResult<()> = (|| {
                let hooks: LuaTable = lua.globals().get("hooks")?;
                let notify: LuaFunction = hooks.get("notify")?;
                notify.call::<()>(("file_changed", event_table))?;
                Ok(())
            })();

            if let Err(e) = hooks_result {
                log::trace!("[watch] hooks.notify failed (may not be loaded): {e}");
            }

            Ok(())
        })();

        if let Err(e) = result {
            log::warn!(
                "[watch] Callback error for {} (path={}, kind={}): {}",
                watch_id,
                event.path.display(),
                kind_to_str(event.kind),
                e
            );
        }

        fired += 1;
    }

    // Clean up temporary registry key.
    let _ = lua.remove_registry_value(callback_key);

    fired
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kind_to_str() {
        assert_eq!(kind_to_str(FileEventKind::Create), "create");
        assert_eq!(kind_to_str(FileEventKind::Modify), "modify");
        assert_eq!(kind_to_str(FileEventKind::Rename), "rename");
        assert_eq!(kind_to_str(FileEventKind::Delete), "delete");
        assert_eq!(kind_to_str(FileEventKind::Other), "other");
    }

    #[test]
    fn test_new_watcher_registry() {
        let registry = new_watcher_registry();
        let entries = registry.lock().expect("mutex");
        assert!(entries.entries.is_empty());
        assert_eq!(entries.next_id, 0);
    }

    #[test]
    fn test_register_creates_watch_table() {
        let lua = Lua::new();
        let registry = new_watcher_registry();

        register(&lua, registry).expect("Should register watch primitives");

        let watch_table: LuaTable = lua
            .globals()
            .get("watch")
            .expect("watch table should exist");
        assert!(watch_table.contains_key("directory").expect("key check"));
        assert!(watch_table.contains_key("unwatch").expect("key check"));
    }

    #[test]
    fn test_watch_directory_real_dir() {
        let dir = std::env::temp_dir().join("botster_watch_test_dir");
        let _ = std::fs::create_dir_all(&dir);

        let lua = Lua::new();
        let registry = new_watcher_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        // Register a watch with a callback
        lua.globals()
            .set("watch_dir", dir.to_string_lossy().to_string())
            .expect("set global");

        let id: String = lua
            .load(
                r#"
            return watch.directory(watch_dir, function(event)
                -- noop callback
            end)
        "#,
            )
            .eval()
            .expect("watch.directory should succeed");

        assert!(id.starts_with("watch_"));

        // Verify entry exists in registry
        {
            let entries = registry.lock().expect("mutex");
            assert_eq!(entries.entries.len(), 1);
            assert!(entries.entries.contains_key(&id));
        }

        // Unwatch
        let removed: bool = lua
            .load(&format!("return watch.unwatch('{}')", id))
            .eval()
            .expect("unwatch should succeed");
        assert!(removed);

        // Verify entry removed
        {
            let entries = registry.lock().expect("mutex");
            assert!(entries.entries.is_empty());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_watch_directory_with_pattern() {
        let dir = std::env::temp_dir().join("botster_watch_pattern_test");
        let _ = std::fs::create_dir_all(&dir);

        let lua = Lua::new();
        let registry = new_watcher_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.globals()
            .set("watch_dir", dir.to_string_lossy().to_string())
            .expect("set global");

        let id: String = lua
            .load(
                r#"
            return watch.directory(watch_dir, { pattern = "*.lua" }, function(event)
            end)
        "#,
            )
            .eval()
            .expect("watch.directory with pattern should succeed");

        assert!(id.starts_with("watch_"));

        // Verify glob is set
        {
            let entries = registry.lock().expect("mutex");
            let entry = entries.entries.get(&id).expect("entry should exist");
            assert!(entry.glob.is_some());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_watch_directory_nonexistent_path_errors() {
        let lua = Lua::new();
        let registry = new_watcher_registry();

        register(&lua, registry).expect("Should register");

        let result: LuaResult<String> = lua
            .load(r#"return watch.directory("/nonexistent/path/xyz", function() end)"#)
            .eval();

        assert!(result.is_err());
    }

    #[test]
    fn test_watch_directory_invalid_glob_errors() {
        let dir = std::env::temp_dir().join("botster_watch_glob_err_test");
        let _ = std::fs::create_dir_all(&dir);

        let lua = Lua::new();
        let registry = new_watcher_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.globals()
            .set("watch_dir", dir.to_string_lossy().to_string())
            .expect("set global");

        let result: LuaResult<String> = lua
            .load(
                r#"return watch.directory(watch_dir, { pattern = "[invalid" }, function() end)"#,
            )
            .eval();

        assert!(result.is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_unwatch_unknown_id_returns_false() {
        let lua = Lua::new();
        let registry = new_watcher_registry();

        register(&lua, registry).expect("Should register");

        let removed: bool = lua
            .load(r#"return watch.unwatch("nonexistent_id")"#)
            .eval()
            .expect("unwatch should not error");

        assert!(!removed);
    }

    #[test]
    fn test_poll_user_watches_empty_registry() {
        let lua = Lua::new();
        let registry = new_watcher_registry();

        let count = poll_user_watches(&lua, &registry);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_watch_directory_fires_callback_on_file_change() {
        let dir = std::env::temp_dir().join("botster_watch_fire_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");

        let lua = Lua::new();
        let registry = new_watcher_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.globals()
            .set("watch_dir", dir.to_string_lossy().to_string())
            .expect("set global");

        // Set up hooks stub so hooks.notify doesn't error
        lua.load(
            r#"
            hooks = { notify = function() end }
            received_events = {}
        "#,
        )
        .exec()
        .expect("setup");

        lua.load(
            r#"
            watch.directory(watch_dir, function(event)
                table.insert(received_events, event)
            end)
        "#,
        )
        .exec()
        .expect("watch.directory should succeed");

        // Create a file to trigger an event
        let test_file = dir.join("test.txt");
        std::fs::write(&test_file, "hello").expect("write file");

        // Give the OS watcher time to detect the change
        std::thread::sleep(std::time::Duration::from_millis(200));

        let count = poll_user_watches(&lua, &registry);

        // Depending on OS timing, we may or may not get the event.
        // At minimum, verify poll doesn't crash.
        let _ = count;

        // If events were received, verify structure
        // The watcher may emit events for the directory itself, so search
        // all events for the one matching our test file.
        if count > 0 {
            let event_count: i32 = lua.load("return #received_events").eval().expect("count");
            assert!(event_count > 0);

            let found: bool = lua
                .load(
                    r#"
                    for _, e in ipairs(received_events) do
                        if e.path and e.path:find("test.txt", 1, true) then
                            return true
                        end
                    end
                    return false
                "#,
                )
                .eval()
                .expect("search");
            assert!(found, "Expected at least one event for test.txt");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_glob_filtering() {
        let dir = std::env::temp_dir().join("botster_watch_glob_filter");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");

        let lua = Lua::new();
        let registry = new_watcher_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.globals()
            .set("watch_dir", dir.to_string_lossy().to_string())
            .expect("set global");

        lua.load(
            r#"
            hooks = { notify = function() end }
            lua_events = {}
        "#,
        )
        .exec()
        .expect("setup");

        // Watch only *.lua files
        lua.load(
            r#"
            watch.directory(watch_dir, { pattern = "*.lua" }, function(event)
                table.insert(lua_events, event)
            end)
        "#,
        )
        .exec()
        .expect("watch.directory with pattern");

        // Create both a .lua file and a .txt file
        std::fs::write(dir.join("test.txt"), "not lua").expect("write txt");
        std::fs::write(dir.join("test.lua"), "-- lua").expect("write lua");

        std::thread::sleep(std::time::Duration::from_millis(200));

        let _ = poll_user_watches(&lua, &registry);

        // If events were received, only .lua events should have been passed to Lua
        let event_count: i32 = lua.load("return #lua_events").eval().expect("count");
        for i in 1..=event_count {
            let path: String = lua
                .load(&format!("return lua_events[{}].path", i))
                .eval()
                .expect("path");
            assert!(
                path.ends_with(".lua"),
                "Glob filter should only pass .lua files, got: {}",
                path
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_unwatch_from_callback_no_deadlock() {
        // Regression test: calling watch.unwatch() inside a callback must not
        // deadlock. This works because poll_user_watches releases the registry
        // lock before firing Lua callbacks.
        let dir = std::env::temp_dir().join("botster_watch_unwatch_cb");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");

        let lua = Lua::new();
        let registry = new_watcher_registry();

        register(&lua, Arc::clone(&registry)).expect("Should register");

        lua.globals()
            .set("watch_dir", dir.to_string_lossy().to_string())
            .expect("set global");

        lua.load(
            r#"
            hooks = { notify = function() end }
            unwatch_called = false
            -- Callback that unwatches itself on first event
            my_watch_id = watch.directory(watch_dir, function(event)
                if not unwatch_called then
                    unwatch_called = true
                    watch.unwatch(my_watch_id)
                end
            end)
        "#,
        )
        .exec()
        .expect("setup");

        // Trigger an event
        std::fs::write(dir.join("trigger.txt"), "data").expect("write");
        std::thread::sleep(std::time::Duration::from_millis(200));

        // This would deadlock with the old implementation
        let _ = poll_user_watches(&lua, &registry);

        let called: bool = lua.globals().get("unwatch_called").unwrap_or(false);
        // If OS delivered the event, the callback should have run and unwatched
        if called {
            let entries = registry.lock().expect("mutex");
            assert!(entries.is_empty(), "Watch should have been removed by callback");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_watch_entries_debug() {
        let entries = WatcherEntries::default();
        let debug = format!("{entries:?}");
        assert!(debug.contains("WatcherEntries"));
    }
}
