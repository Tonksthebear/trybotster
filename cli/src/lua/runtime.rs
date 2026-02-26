//! Lua runtime management.
//!
//! Provides the `LuaRuntime` struct which owns and manages the Lua interpreter
//! state. Handles script loading, function invocation, and error handling based
//! on environment configuration.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use mlua::{IntoLuaMulti, Lua};

use crate::hub::handle_cache::HandleCache;

use super::primitives;
use super::primitives::events::SharedEventCallbacks;
use super::primitives::pty::PtyOutputContext;
use super::primitives::socket::registry_keys as socket_registry_keys;
use super::primitives::tui::registry_keys as tui_registry_keys;
use super::primitives::http::HttpAsyncRegistry;
use super::primitives::timer::TimerRegistry;
use super::primitives::watch::WatcherRegistry;
use super::primitives::webrtc::registry_keys;
use super::primitives::websocket::WebSocketRegistry;
use super::primitives::HubEventSender;

/// Lua scripting runtime for the botster hub.
///
/// Owns the Lua interpreter state and provides methods for loading scripts
/// and calling Lua functions. Thread safety depends on usage context - the
/// Lua state is not `Send` or `Sync` by default.
///
/// # Environment Variables
///
/// - `BOTSTER_LUA_PATH` - Override default script path
/// - `BOTSTER_LUA_STRICT` - If "1", errors panic instead of logging
///
/// # Hot-Reload
///
/// Hot-reload of core Lua modules and plugins is handled entirely by
/// `handlers/module_watcher.lua` using the `watch.directory()` primitive.
pub struct LuaRuntime {
    /// The Lua interpreter state.
    lua: Lua,
    /// Base path for loading Lua scripts.
    base_path: PathBuf,
    /// Whether to panic on Lua errors (strict mode).
    strict: bool,
    /// Shared sender for Lua primitives to deliver events to the Hub event loop.
    ///
    /// Initially `None`, filled by `set_hub_event_tx()` before plugins execute.
    /// Captured by closures in WebRTC, TUI, PTY, Hub, connection, and worktree
    /// primitives so they can send events directly without intermediate queues.
    hub_event_sender: HubEventSender,
    /// Event callbacks registered by Lua scripts.
    event_callbacks: SharedEventCallbacks,
    /// Registry of active user file watches (for `watch.directory()`).
    watcher_registry: WatcherRegistry,
    /// Registry of active timers (for `timer.after()` and `timer.every()`).
    timer_registry: TimerRegistry,
    /// Registry of async HTTP requests (for `http.request()`).
    http_registry: HttpAsyncRegistry,
    /// Registry of WebSocket connections (for `websocket.connect()`).
    websocket_registry: WebSocketRegistry,
    /// Registry of ActionCable channel callbacks (for `action_cable.subscribe()`).
    ac_callback_registry: primitives::ActionCableCallbackRegistry,
    /// Registry of hub client connection callbacks (for `hub_client.on_message()`).
    hub_client_callback_registry: primitives::HubClientCallbackRegistry,
    /// Pending blocking requests for `hub_client.request()`.
    hub_client_pending_requests: primitives::HubClientPendingRequests,
    /// Direct frame write channels for `hub_client.request()` (bypasses event loop).
    hub_client_frame_senders: primitives::HubClientFrameSenders,
    /// Cached compiled function for PTY output interceptor calls.
    pty_hook_fn: Option<mlua::RegistryKey>,
    /// Cached reusable context table for PTY output interceptor calls.
    pty_hook_ctx: Option<mlua::RegistryKey>,
    /// Whether any agent has a pending notification to clear on PTY input.
    ///
    /// Set `true` by `notify_pty_notification` when a notification fires.
    /// Checked by `notify_pty_input` on every keystroke — when `false`
    /// (99.9% of the time), the Lua call is skipped entirely.
    /// Cleared when the last notification is dismissed.
    pty_input_listening: bool,
}

impl std::fmt::Debug for LuaRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let event_cb_count = self.event_callbacks.lock().map(|c| c.callback_count()).unwrap_or(0);
        let watch_count = self.watcher_registry.lock().map(|w| w.len()).unwrap_or(0);
        let timer_count = self.timer_registry.lock().map(|t| t.len()).unwrap_or(0);
        let (http_pending, http_in_flight) = self
            .http_registry
            .lock()
            .map(|h| (h.pending_count(), h.in_flight_count()))
            .unwrap_or((0, 0));
        let hub_event_tx_active = self.hub_event_sender.lock()
            .map(|g| g.is_some())
            .unwrap_or(false);
        f.debug_struct("LuaRuntime")
            .field("base_path", &self.base_path)
            .field("strict", &self.strict)
            .field("hub_event_tx_active", &hub_event_tx_active)
            .field("event_callback_count", &event_cb_count)
            .field("active_watches", &watch_count)
            .field("active_timers", &timer_count)
            .field("http_pending", &http_pending)
            .field("http_in_flight", &http_in_flight)
            .field("websocket_connections",
                &self.websocket_registry.lock().map(|r| r.connection_count()).unwrap_or(0))
            .finish_non_exhaustive()
    }
}

impl LuaRuntime {
    /// Create a new Lua runtime with primitives registered.
    ///
    /// Initializes the Lua state and registers all primitive functions
    /// (currently just the `log` table).
    ///
    /// # Environment Variables
    ///
    /// - `BOTSTER_LUA_PATH` - Override default script path (default: `~/.botster/lua`)
    /// - `BOTSTER_LUA_STRICT` - If "1", Lua errors panic; otherwise they're logged
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Lua state creation fails
    /// - Primitive registration fails
    pub fn new() -> Result<Self> {
        let lua = Lua::new();

        // Determine base path for Lua scripts
        let base_path = Self::resolve_base_path();

        // Check strict mode
        let strict = std::env::var("BOTSTER_LUA_STRICT")
            .map(|v| v == "1")
            .unwrap_or(false);

        // Create shared event sender for Lua primitives (initially None,
        // filled by set_hub_event_tx() before plugins execute)
        let hub_event_sender = primitives::new_hub_event_sender();

        // Create event callback storage
        let event_callbacks = primitives::new_event_callbacks();

        // Create watcher registry for watch.directory()
        let watcher_registry = primitives::new_watcher_registry();

        // Create timer registry for timer.after() / timer.every()
        let timer_registry = primitives::new_timer_registry();

        // Create HTTP async registry for http.request()
        let http_registry = primitives::new_http_registry();

        // Create WebSocket connection registry for websocket.connect()
        let websocket_registry = primitives::new_websocket_registry();

        // Create ActionCable callback registry for action_cable.subscribe()
        let ac_callback_registry = primitives::new_ac_callback_registry();

        // Create hub client callback registry for hub_client.on_message()
        let hub_client_callback_registry = primitives::new_hub_client_callback_registry();

        // Create hub client pending requests map for hub_client.request()
        let hub_client_pending_requests = primitives::new_hub_client_pending_requests();

        // Create hub client frame senders map for hub_client.request() direct writes
        let hub_client_frame_senders = primitives::new_hub_client_frame_senders();

        // Register all primitives
        primitives::register_all(&lua).context("Failed to register Lua primitives")?;

        // Register self-update primitives with the shared event sender
        primitives::register_update(&lua, Arc::clone(&hub_event_sender))
            .context("Failed to register update primitives")?;

        // Register push notification primitives with the shared event sender
        primitives::register_push(&lua, Arc::clone(&hub_event_sender))
            .context("Failed to register push primitives")?;

        // Register WebRTC primitives with the shared event sender
        primitives::register_webrtc(&lua, Arc::clone(&hub_event_sender))
            .context("Failed to register WebRTC primitives")?;

        // Register TUI primitives with the shared event sender
        primitives::register_tui(&lua, Arc::clone(&hub_event_sender))
            .context("Failed to register TUI primitives")?;

        // Register socket IPC primitives with the shared event sender
        primitives::register_socket(&lua, Arc::clone(&hub_event_sender))
            .context("Failed to register socket primitives")?;

        // Register PTY primitives with the shared event sender
        primitives::register_pty(&lua, Arc::clone(&hub_event_sender))
            .context("Failed to register PTY primitives")?;

        // Register event primitives with the callback storage
        primitives::register_events(&lua, Arc::clone(&event_callbacks))
            .context("Failed to register event primitives")?;

        // Register watch primitives with the watcher registry
        primitives::register_watch(&lua, Arc::clone(&watcher_registry))
            .context("Failed to register watch primitives")?;

        // Register timer primitives with the timer registry
        primitives::register_timer(&lua, Arc::clone(&timer_registry))
            .context("Failed to register timer primitives")?;

        // Register HTTP primitives with the async registry
        primitives::register_http(&lua, Arc::clone(&http_registry))
            .context("Failed to register HTTP primitives")?;

        // Register WebSocket primitives with the connection registry
        primitives::register_websocket(&lua, Arc::clone(&websocket_registry))
            .context("Failed to register WebSocket primitives")?;

        // Register ActionCable primitives with the shared event sender and callback registry
        primitives::register_action_cable(
            &lua,
            Arc::clone(&hub_event_sender),
            Arc::clone(&ac_callback_registry),
        ).context("Failed to register ActionCable primitives")?;

        // Register hub client primitives with the shared event sender and registries
        primitives::register_hub_client(
            &lua,
            Arc::clone(&hub_event_sender),
            Arc::clone(&hub_client_callback_registry),
            Arc::clone(&hub_client_pending_requests),
            Arc::clone(&hub_client_frame_senders),
        ).context("Failed to register hub client primitives")?;

        // Note: Hub, connection, and worktree primitives are registered later via
        // register_hub_primitives() because they need a HandleCache reference from Hub

        // Configure package.path to include the base path for require()
        Self::setup_package_path(&lua, &base_path)?;

        log::debug!(
            "Lua runtime created (base_path={}, strict={})",
            base_path.display(),
            strict
        );

        Ok(Self {
            lua,
            base_path,
            strict,
            hub_event_sender,
            event_callbacks,
            watcher_registry,
            timer_registry,
            http_registry,
            websocket_registry,
            ac_callback_registry,
            hub_client_callback_registry,
            hub_client_pending_requests,
            hub_client_frame_senders,
            pty_hook_fn: None,
            pty_hook_ctx: None,
            pty_input_listening: false,
        })
    }

    /// Configure Lua package.path to include the base path and subdirectories.
    ///
    /// This allows:
    /// - `require("hub.hooks")` to find `{base_path}/hub/hooks.lua`
    /// - `require("lib.client")` to find `{base_path}/lib/client.lua`
    /// - `require("handlers.webrtc")` to find `{base_path}/handlers/webrtc.lua`
    fn setup_package_path(lua: &Lua, base_path: &Path) -> Result<()> {
        let package: mlua::Table = lua
            .globals()
            .get("package")
            .map_err(|e| anyhow!("Failed to get package table: {e}"))?;

        let current_path: String = package
            .get("path")
            .map_err(|e| anyhow!("Failed to get package.path: {e}"))?;

        // Add search paths for our module structure:
        // - {base}/?.lua - top-level modules
        // - {base}/?/init.lua - package init files
        // - {base}/lib/?.lua - library modules (client, utils)
        // - {base}/handlers/?.lua - handler modules (webrtc)
        // - {base}/hub/?.lua - hub modules (state, hooks, loader)
        // - {base}/plugins/?.lua - user plugins
        // - {base}/plugins/?/init.lua - user plugin packages
        let new_path = format!(
            "{path}/?.lua;{path}/?/init.lua;{path}/lib/?.lua;{path}/handlers/?.lua;{path}/hub/?.lua;{path}/plugins/?.lua;{path}/plugins/?/init.lua;{current}",
            path = base_path.display(),
            current = current_path
        );

        package
            .set("path", new_path)
            .map_err(|e| anyhow!("Failed to set package.path: {e}"))?;

        Ok(())
    }

    /// Prepend a directory to Lua `package.path`, giving it priority over existing paths.
    ///
    /// Used to layer override directories onto the search path. Called paths
    /// are searched before any previously configured paths, enabling the chain:
    /// project root (highest) > userspace > default.
    ///
    /// # Arguments
    ///
    /// * `additional_path` - Path to add to package.path
    ///
    /// # Errors
    ///
    /// Returns an error if the package table cannot be accessed or modified.
    pub fn update_package_path(&self, additional_path: &Path) -> Result<()> {
        Self::update_package_path_internal(&self.lua, additional_path)
    }

    /// Internal implementation of update_package_path.
    fn update_package_path_internal(lua: &Lua, additional_path: &Path) -> Result<()> {
        let package: mlua::Table = lua
            .globals()
            .get("package")
            .map_err(|e| anyhow!("Failed to get package table: {e}"))?;

        let current_path: String = package
            .get("path")
            .map_err(|e| anyhow!("Failed to get package.path: {e}"))?;

        // Prepend so this path takes priority over paths already in package.path
        let new_path = format!(
            "{path}/?.lua;{path}/?/init.lua;{path}/lib/?.lua;{path}/handlers/?.lua;{path}/hub/?.lua;{path}/plugins/?.lua;{path}/plugins/?/init.lua;{current}",
            path = additional_path.display(),
            current = current_path
        );

        package
            .set("path", new_path)
            .map_err(|e| anyhow!("Failed to set package.path: {e}"))?;

        Ok(())
    }

    /// Resolve the base path for Lua scripts.
    ///
    /// Priority:
    /// 1. `BOTSTER_LUA_PATH` environment variable
    /// 2. `~/.botster/lua` (default)
    fn resolve_base_path() -> PathBuf {
        if let Ok(path) = std::env::var("BOTSTER_LUA_PATH") {
            return PathBuf::from(path);
        }

        // Default: ~/.botster/lua
        dirs::home_dir()
            .map(|home| home.join(".botster").join("lua"))
            .unwrap_or_else(|| PathBuf::from(".botster/lua"))
    }

    /// Get the base path for Lua scripts.
    #[must_use]
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Set the base path for Lua scripts.
    ///
    /// Updates the base path used for file watching. Call this when loading
    /// from an alternate directory (e.g., embedded Lua files during development).
    pub fn set_base_path(&mut self, path: PathBuf) {
        self.base_path = path;
    }

    /// Load and execute a Lua file relative to the base path.
    ///
    /// The file path is resolved relative to the configured base path
    /// (from `BOTSTER_LUA_PATH` or `~/.botster/lua`).
    ///
    /// # Arguments
    ///
    /// * `relative_path` - Path relative to base path (e.g., `hub/init.lua`)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file cannot be read
    /// - The Lua code has syntax errors
    /// - The Lua code throws an error during execution
    ///
    /// In strict mode (`BOTSTER_LUA_STRICT=1`), errors propagate up.
    /// In non-strict mode, errors are logged and `Ok(())` is returned.
    pub fn load_file(&self, relative_path: &Path) -> Result<()> {
        let full_path = self.base_path.join(relative_path);

        match self.load_file_internal(&full_path) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict {
                    Err(e)
                } else {
                    log::warn!("Lua file error ({}): {}", full_path.display(), e);
                    Ok(())
                }
            }
        }
    }

    /// Internal file loading that always returns errors.
    fn load_file_internal(&self, path: &Path) -> Result<()> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read Lua file: {}", path.display()))?;

        self.lua
            .load(&source)
            .set_name(path.to_string_lossy())
            .exec()
            .map_err(|e| anyhow!("Failed to execute Lua file {}: {}", path.display(), e))?;

        log::debug!("Loaded Lua file: {}", path.display());
        Ok(())
    }

    /// Load and execute a Lua file from an absolute path.
    ///
    /// Unlike `load_file`, this does not prepend the base path.
    ///
    /// # Arguments
    ///
    /// * `path` - Absolute path to the Lua file
    ///
    /// # Errors
    ///
    /// Same as `load_file`.
    pub fn load_file_absolute(&self, path: &Path) -> Result<()> {
        match self.load_file_internal(path) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict {
                    Err(e)
                } else {
                    log::warn!("Lua file error ({}): {}", path.display(), e);
                    Ok(())
                }
            }
        }
    }

    /// Load and execute Lua code from a string.
    ///
    /// Used for loading embedded Lua files in release builds.
    ///
    /// # Arguments
    ///
    /// * `name` - Name for error messages (e.g., "hub/init.lua")
    /// * `source` - The Lua source code
    ///
    /// # Errors
    ///
    /// Returns an error if the Lua code fails to parse or execute.
    pub fn load_string(&self, name: &str, source: &str) -> Result<()> {
        match self.load_string_internal(name, source) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict {
                    Err(e)
                } else {
                    log::warn!("Lua error ({}): {}", name, e);
                    Ok(())
                }
            }
        }
    }

    /// Internal string loading that always returns errors.
    fn load_string_internal(&self, name: &str, source: &str) -> Result<()> {
        self.lua
            .load(source)
            .set_name(name)
            .exec()
            .map_err(|e| anyhow!("Failed to execute Lua {}: {}", name, e))?;

        log::debug!("Loaded Lua: {}", name);
        Ok(())
    }

    /// Load embedded Lua files as a fallback module source.
    ///
    /// Installs a custom searcher (appended last in `package.searchers`)
    /// and loads `hub/init.lua`, whose `require()` calls resolve modules
    /// lazily from embedded content when not found on the filesystem.
    ///
    /// # Errors
    ///
    /// Returns an error if any embedded file fails to load.
    pub fn load_embedded(&self) -> Result<()> {
        use super::embedded;

        let files = embedded::all();
        if files.is_empty() {
            log::warn!("No embedded Lua files found");
            return Ok(());
        }

        log::info!("Loading {} embedded Lua file(s)", files.len());

        // Install embedded searcher as fallback (last in package.searchers).
        // Filesystem paths (project root, userspace) are checked first.
        self.install_embedded_searcher()?;

        // Now load hub/init.lua — its require() calls trigger the embedded
        // searcher, which lazily loads each module and its transitive deps.
        if let Some(init_content) = embedded::get("hub/init.lua") {
            self.load_string("hub/init.lua", init_content)?;
        } else {
            return Err(anyhow!("Missing embedded hub/init.lua"));
        }

        log::info!("Embedded Lua loaded successfully");
        Ok(())
    }

    /// Install a custom Lua searcher for embedded modules as a fallback.
    ///
    /// Appends a searcher to `package.searchers` that maps module names
    /// (e.g., `"lib.agent"`) to embedded source files (e.g., `"lib/agent.lua"`).
    /// When `require("lib.agent")` is called and no filesystem match is found,
    /// this searcher returns a loader function that compiles and executes the
    /// embedded source.
    ///
    /// The searcher is appended (last position) so filesystem paths take
    /// priority. This enables the override chain:
    /// 1. Project root (`{repo}/.botster/lua/`) — highest priority
    /// 2. Userspace (`~/.botster/lua/`) — user overrides
    /// 3. Embedded (this searcher) — fallback/base
    fn install_embedded_searcher(&self) -> Result<()> {
        use super::embedded;

        let lua = &self.lua;

        // Build a Lua table mapping module names to their source
        let embedded_modules: mlua::Table = lua.create_table()
            .map_err(|e| anyhow!("Failed to create embedded modules table: {e}"))?;

        for (path, content) in embedded::all() {
            // Skip ui/ (loaded by TUI separately) and hub/init.lua (loaded explicitly)
            if path.starts_with("ui/") || *path == "hub/init.lua" {
                continue;
            }
            let module_name = path.trim_end_matches(".lua").replace('/', ".");
            embedded_modules
                .set(module_name.as_str(), *content)
                .map_err(|e| anyhow!("Failed to populate embedded table: {e}"))?;
        }

        // Set the table as a global so the searcher closure can access it
        lua.globals()
            .set("_EMBEDDED_MODULES", embedded_modules)
            .map_err(|e| anyhow!("Failed to set _EMBEDDED_MODULES: {e}"))?;

        // Install the searcher via Lua code — append to package.searchers
        // so filesystem paths are checked first (embedded is the fallback).
        lua.load(
            r#"
            local embedded = _EMBEDDED_MODULES
            -- Append our searcher so filesystem paths (userspace, project root) win
            table.insert(package.searchers, function(module_name)
                local source = embedded[module_name]
                if source then
                    local fn, err = load(source, "=" .. module_name:gsub("%.", "/") .. ".lua")
                    if fn then
                        return fn
                    else
                        return "\n\tembedded load error: " .. (err or "unknown")
                    end
                end
                return "\n\tno embedded module '" .. module_name .. "'"
            end)
            -- Clean up the global reference (searcher captured it via upvalue)
            _EMBEDDED_MODULES = nil
            "#,
        )
        .set_name("embedded_searcher_setup")
        .exec()
        .map_err(|e| anyhow!("Failed to install embedded searcher: {e}"))?;

        log::debug!("Installed embedded module searcher");
        Ok(())
    }

    /// Call a global Lua function.
    ///
    /// Looks up a function in the global table and invokes it with the
    /// provided arguments.
    ///
    /// # Arguments
    ///
    /// * `name` - Name of the global function to call
    /// * `args` - Arguments to pass to the function
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The function doesn't exist
    /// - The arguments cannot be converted to Lua values
    /// - The function throws an error
    ///
    /// In non-strict mode, errors are logged and `Ok(())` is returned.
    pub fn call_function<A>(&self, name: &str, args: A) -> Result<()>
    where
        A: IntoLuaMulti,
    {
        match self.call_function_internal(name, args) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict {
                    Err(e)
                } else {
                    log::warn!("Lua function error ({}): {}", name, e);
                    Ok(())
                }
            }
        }
    }

    /// Internal function call that always returns errors.
    fn call_function_internal<A>(&self, name: &str, args: A) -> Result<()>
    where
        A: IntoLuaMulti,
    {
        let globals = self.lua.globals();
        let func: mlua::Function = globals
            .get(name)
            .map_err(|e| anyhow!("Lua function not found '{}': {}", name, e))?;

        func.call::<()>(args)
            .map_err(|e| anyhow!("Lua function '{}' failed: {}", name, e))?;

        Ok(())
    }

    /// Check if a global function exists.
    ///
    /// # Arguments
    ///
    /// * `name` - Name of the global to check
    ///
    /// # Returns
    ///
    /// `true` if a function with the given name exists in globals.
    #[must_use]
    pub fn has_function(&self, name: &str) -> bool {
        self.lua
            .globals()
            .get::<mlua::Function>(name)
            .is_ok()
    }

    /// Get a reference to the underlying Lua state.
    ///
    /// Use with caution - direct manipulation bypasses error handling
    /// and strict mode semantics.
    #[must_use]
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    // =========================================================================
    // Watcher Management
    // =========================================================================

    /// Stop all blocking watcher tasks (user `watch.directory()` watches).
    ///
    /// Must be called before the tokio runtime drops to prevent a deadlock.
    /// Each watcher's `spawn_blocking` forwarder blocks on `rx.recv()` — the
    /// sender lives inside the `FileWatcher`. Aborting the forwarder and
    /// dropping the watcher closes the channel, allowing the runtime's
    /// blocking pool to shut down cleanly.
    pub fn stop_all_watchers(&mut self) {
        self.watcher_registry
            .lock()
            .expect("WatcherEntries mutex poisoned")
            .stop_all();
    }

    /// Poll user file watches via periodic drain (test-only fallback).
    ///
    /// Production uses `HubEvent::UserFileWatch` from blocking forwarder tasks.
    #[cfg(test)]
    pub fn poll_user_file_watches(&self) -> usize {
        use super::primitives::watch;
        watch::poll_user_watches(&self.lua, &self.watcher_registry)
    }

    /// Fire Lua callbacks for a user file watch event (event-driven path).
    ///
    /// Called from `handle_hub_event()` for `HubEvent::UserFileWatch` events.
    pub(crate) fn fire_user_file_watch(
        &self,
        watch_id: &str,
        events: Vec<crate::file_watcher::FileEvent>,
    ) -> usize {
        use super::primitives::watch;
        watch::fire_user_watch_events(&self.lua, &self.watcher_registry, watch_id, events)
    }

    /// Poll timers via deadline scanning (test-only fallback).
    ///
    /// Production uses `HubEvent::TimerFired` from spawned tokio tasks.
    #[cfg(test)]
    pub fn poll_timers(&self) -> usize {
        use super::primitives::timer;
        timer::poll_timers(&self.lua, &self.timer_registry)
    }

    /// Poll HTTP responses via shared vec (test-only fallback).
    ///
    /// Production uses `HubEvent::HttpResponse` from background threads.
    #[cfg(test)]
    pub fn poll_http_responses(&self) -> usize {
        use super::primitives::http;
        http::poll_http_responses(&self.lua, &self.http_registry)
    }

    // =========================================================================
    // Hook System Integration
    // =========================================================================

    /// Check if any hooks are registered for an event.
    ///
    /// This provides a fast-path optimization for Rust code: if no hooks
    /// are registered for an event, there's no need to prepare arguments
    /// and call into Lua.
    ///
    /// # Arguments
    ///
    /// * `event_name` - The event name to check (e.g., "pty_output", "message_received")
    ///
    /// # Returns
    ///
    /// `true` if at least one enabled hook is registered for the event.
    /// Check if any observers are registered for an event.
    ///
    /// Observers are async/safe - they receive notifications but cannot
    /// block or transform data.
    #[must_use]
    pub fn has_observers(&self, event_name: &str) -> bool {
        let result: mlua::Result<bool> = (|| {
            let hooks: mlua::Table = self.lua.globals().get("hooks")?;
            let has_fn: mlua::Function = hooks.get("has_observers")?;
            has_fn.call::<bool>(event_name)
        })();
        result.unwrap_or(false)
    }

    /// Check if any interceptors are registered for an event.
    ///
    /// Interceptors are sync/blocking - they can transform or drop data
    /// but run in the critical path.
    #[must_use]
    pub fn has_interceptors(&self, event_name: &str) -> bool {
        let result: mlua::Result<bool> = (|| {
            let hooks: mlua::Table = self.lua.globals().get("hooks")?;
            let has_fn: mlua::Function = hooks.get("has_interceptors")?;
            has_fn.call::<bool>(event_name)
        })();
        result.unwrap_or(false)
    }

    /// Check if any hooks (observers or interceptors) are registered.
    ///
    /// # Example
    ///
    /// ```ignore
    /// if lua.has_hooks("pty_output") {
    ///     // Hooks exist - need to involve Lua
    /// } else {
    ///     // Fast path: no hooks, skip Lua entirely
    /// }
    /// ```
    #[must_use]
    pub fn has_hooks(&self, event_name: &str) -> bool {
        self.has_observers(event_name) || self.has_interceptors(event_name)
    }

    /// Notify observers of an event (fire-and-forget).
    ///
    /// Observers are called asynchronously and cannot affect data flow.
    /// Errors are logged but don't stop notification to other observers.
    ///
    /// # Returns
    ///
    /// Number of observers notified.
    pub fn notify_observers(&self, event_name: &str, data: &str) -> usize {
        let result: mlua::Result<usize> = (|| {
            let hooks: mlua::Table = self.lua.globals().get("hooks")?;
            let notify: mlua::Function = hooks.get("notify")?;
            notify.call::<usize>((event_name, data))
        })();

        match result {
            Ok(count) => count,
            Err(e) => {
                log::error!("Observer notification failed for '{}': {}", event_name, e);
                0
            }
        }
    }

    /// Call interceptor chain for an event with string data.
    ///
    /// Interceptors can transform or drop data. They run synchronously
    /// in the critical path - use sparingly.
    ///
    /// # Returns
    ///
    /// The transformed data, or `None` if any interceptor returned `nil` (drop).
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(output) = lua.call_interceptors("pty_output", raw_output) {
    ///     // Use transformed output
    /// } else {
    ///     // Interceptor returned nil, drop this output
    /// }
    /// ```
    pub fn call_interceptors(&self, event_name: &str, data: &str) -> Option<String> {
        let result: mlua::Result<Option<String>> = (|| {
            let hooks: mlua::Table = self.lua.globals().get("hooks")?;
            let call: mlua::Function = hooks.get("call")?;
            call.call::<Option<String>>((event_name, data))
        })();

        match result {
            Ok(value) => value,
            Err(e) => {
                log::error!("Interceptor chain failed for '{}': {}", event_name, e);
                // On error, return original data (don't drop)
                Some(data.to_string())
            }
        }
    }

    // =========================================================================
    // WebRTC Callback Methods
    // =========================================================================

    /// Call the on_peer_connected callback if registered.
    ///
    /// Called by Hub when a WebRTC peer connection is established.
    ///
    /// # Arguments
    ///
    /// * `peer_id` - The unique identifier of the connected peer
    ///
    /// # Errors
    ///
    /// In strict mode, returns errors from Lua callback execution.
    /// In non-strict mode, logs errors and returns Ok.
    pub fn call_peer_connected(&self, peer_id: &str) -> Result<()> {
        match self.call_peer_connected_internal(peer_id) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict {
                    Err(e)
                } else {
                    log::warn!("Lua peer_connected callback error: {}", e);
                    Ok(())
                }
            }
        }
    }

    fn call_peer_connected_internal(&self, peer_id: &str) -> Result<()> {
        let key_result: mlua::Result<mlua::RegistryKey> =
            self.lua.named_registry_value(registry_keys::ON_PEER_CONNECTED);

        if let Ok(key) = key_result {
            let callback: mlua::Function = self.lua.registry_value(&key)
                .map_err(|e| anyhow!("Failed to get peer_connected callback: {e}"))?;

            callback.call::<()>(peer_id)
                .map_err(|e| anyhow!("peer_connected callback failed: {e}"))?;
        }

        Ok(())
    }

    /// Call the on_peer_disconnected callback if registered.
    ///
    /// Called by Hub when a WebRTC peer connection is closed.
    ///
    /// # Arguments
    ///
    /// * `peer_id` - The unique identifier of the disconnected peer
    ///
    /// # Errors
    ///
    /// In strict mode, returns errors from Lua callback execution.
    /// In non-strict mode, logs errors and returns Ok.
    pub fn call_peer_disconnected(&self, peer_id: &str) -> Result<()> {
        match self.call_peer_disconnected_internal(peer_id) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict {
                    Err(e)
                } else {
                    log::warn!("Lua peer_disconnected callback error: {}", e);
                    Ok(())
                }
            }
        }
    }

    fn call_peer_disconnected_internal(&self, peer_id: &str) -> Result<()> {
        let key_result: mlua::Result<mlua::RegistryKey> =
            self.lua.named_registry_value(registry_keys::ON_PEER_DISCONNECTED);

        if let Ok(key) = key_result {
            let callback: mlua::Function = self.lua.registry_value(&key)
                .map_err(|e| anyhow!("Failed to get peer_disconnected callback: {e}"))?;

            callback.call::<()>(peer_id)
                .map_err(|e| anyhow!("peer_disconnected callback failed: {e}"))?;
        }

        Ok(())
    }

    /// Call the on_message callback with a JSON value.
    ///
    /// Called by Hub when a WebRTC message is received from a peer.
    /// The JSON value is converted to a Lua table using mlua's serialize feature.
    ///
    /// # Arguments
    ///
    /// * `peer_id` - The unique identifier of the sending peer
    /// * `message` - The JSON message to pass to Lua
    ///
    /// # Errors
    ///
    /// In strict mode, returns errors from Lua callback execution.
    /// In non-strict mode, logs errors and returns Ok.
    pub fn call_webrtc_message(&self, peer_id: &str, message: serde_json::Value) -> Result<()> {
        match self.call_webrtc_message_internal(peer_id, message) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict {
                    Err(e)
                } else {
                    log::warn!("Lua webrtc_message callback error: {}", e);
                    Ok(())
                }
            }
        }
    }

    fn call_webrtc_message_internal(&self, peer_id: &str, message: serde_json::Value) -> Result<()> {
        let key_result: mlua::Result<mlua::RegistryKey> =
            self.lua.named_registry_value(registry_keys::ON_MESSAGE);

        if let Ok(key) = key_result {
            let callback: mlua::Function = self.lua.registry_value(&key)
                .map_err(|e| anyhow!("Failed to get webrtc_message callback: {e}"))?;

            // Convert JSON to Lua value, mapping null → nil (not userdata)
            let lua_value = crate::lua::primitives::json::json_to_lua(&self.lua, &message)
                .map_err(|e| anyhow!("Failed to convert JSON to Lua value: {e}"))?;

            callback.call::<()>((peer_id, lua_value))
                .map_err(|e| anyhow!("webrtc_message callback failed: {e}"))?;
        }

        Ok(())
    }

    /// Check if any WebRTC callbacks are registered.
    ///
    /// Returns true if at least one of:
    /// - on_peer_connected
    /// - on_peer_disconnected
    /// - on_message
    ///
    /// is registered. Hub can use this as a fast-path check before
    /// preparing arguments for Lua calls.
    #[must_use]
    pub fn has_webrtc_callbacks(&self) -> bool {
        // Check each callback by attempting to retrieve the registry key.
        // The key is stored via set_named_registry_value, so we retrieve it
        // and then try to get the actual function from the registry.
        let has_connected = self.has_callback_registered(registry_keys::ON_PEER_CONNECTED);
        let has_disconnected = self.has_callback_registered(registry_keys::ON_PEER_DISCONNECTED);
        let has_message = self.has_callback_registered(registry_keys::ON_MESSAGE);

        has_connected || has_disconnected || has_message
    }

    /// Check if a specific callback is registered in the named registry.
    fn has_callback_registered(&self, key_name: &str) -> bool {
        // Try to get the registry key, then verify we can retrieve the function from it
        if let Ok(key) = self.lua.named_registry_value::<mlua::RegistryKey>(key_name) {
            // If we got a key, verify it points to a valid function
            self.lua.registry_value::<mlua::Function>(&key).is_ok()
        } else {
            false
        }
    }

    // =========================================================================
    // TUI Callback Methods
    // =========================================================================

    /// Call the TUI on_connected callback if registered.
    ///
    /// Called by Hub when the TUI is ready to receive messages.
    ///
    /// # Errors
    ///
    /// In strict mode, returns errors from Lua callback execution.
    /// In non-strict mode, logs errors and returns Ok.
    pub fn call_tui_connected(&self) -> Result<()> {
        match self.call_tui_connected_internal() {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict {
                    Err(e)
                } else {
                    log::warn!("Lua tui_connected callback error: {}", e);
                    Ok(())
                }
            }
        }
    }

    fn call_tui_connected_internal(&self) -> Result<()> {
        let key_result: mlua::Result<mlua::RegistryKey> = self
            .lua
            .named_registry_value(tui_registry_keys::ON_CONNECTED);

        if let Ok(key) = key_result {
            let callback: mlua::Function = self
                .lua
                .registry_value(&key)
                .map_err(|e| anyhow!("Failed to get tui_connected callback: {e}"))?;

            callback
                .call::<()>(())
                .map_err(|e| anyhow!("tui_connected callback failed: {e}"))?;
        }

        Ok(())
    }

    /// Call the TUI on_disconnected callback if registered.
    ///
    /// Called by Hub when the TUI is shutting down.
    ///
    /// # Errors
    ///
    /// In strict mode, returns errors from Lua callback execution.
    /// In non-strict mode, logs errors and returns Ok.
    pub fn call_tui_disconnected(&self) -> Result<()> {
        match self.call_tui_disconnected_internal() {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict {
                    Err(e)
                } else {
                    log::warn!("Lua tui_disconnected callback error: {}", e);
                    Ok(())
                }
            }
        }
    }

    fn call_tui_disconnected_internal(&self) -> Result<()> {
        let key_result: mlua::Result<mlua::RegistryKey> = self
            .lua
            .named_registry_value(tui_registry_keys::ON_DISCONNECTED);

        if let Ok(key) = key_result {
            let callback: mlua::Function = self
                .lua
                .registry_value(&key)
                .map_err(|e| anyhow!("Failed to get tui_disconnected callback: {e}"))?;

            callback
                .call::<()>(())
                .map_err(|e| anyhow!("tui_disconnected callback failed: {e}"))?;
        }

        Ok(())
    }

    /// Call the TUI on_message callback with a JSON value.
    ///
    /// Called by Hub when a message is received from the TUI.
    /// The JSON value is converted to a Lua table using mlua's serialize feature.
    ///
    /// # Arguments
    ///
    /// * `message` - The JSON message to pass to Lua
    ///
    /// # Errors
    ///
    /// In strict mode, returns errors from Lua callback execution.
    /// In non-strict mode, logs errors and returns Ok.
    pub fn call_tui_message(&self, message: serde_json::Value) -> Result<()> {
        match self.call_tui_message_internal(message) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict {
                    Err(e)
                } else {
                    log::warn!("Lua tui_message callback error: {}", e);
                    Ok(())
                }
            }
        }
    }

    fn call_tui_message_internal(&self, message: serde_json::Value) -> Result<()> {
        let key_result: mlua::Result<mlua::RegistryKey> = self
            .lua
            .named_registry_value(tui_registry_keys::ON_MESSAGE);

        if let Ok(key) = key_result {
            let callback: mlua::Function = self
                .lua
                .registry_value(&key)
                .map_err(|e| anyhow!("Failed to get tui_message callback: {e}"))?;

            let lua_value = crate::lua::primitives::json::json_to_lua(&self.lua, &message)
                .map_err(|e| anyhow!("Failed to convert JSON to Lua value: {e}"))?;

            callback
                .call::<()>(lua_value)
                .map_err(|e| anyhow!("tui_message callback failed: {e}"))?;
        }

        Ok(())
    }

    /// Check if any TUI callbacks are registered.
    ///
    /// Returns true if at least one of on_connected, on_disconnected,
    /// or on_message is registered.
    #[must_use]
    pub fn has_tui_callbacks(&self) -> bool {
        let has_connected = self.has_callback_registered(tui_registry_keys::ON_CONNECTED);
        let has_disconnected = self.has_callback_registered(tui_registry_keys::ON_DISCONNECTED);
        let has_message = self.has_callback_registered(tui_registry_keys::ON_MESSAGE);

        has_connected || has_disconnected || has_message
    }

    // =========================================================================
    // Socket Client Callbacks
    // =========================================================================

    /// Call the socket on_client_connected callback if registered.
    pub fn call_socket_client_connected(&self, client_id: &str) -> Result<()> {
        match self.call_socket_client_connected_internal(client_id) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict { Err(e) } else {
                    log::warn!("Lua socket_client_connected callback error: {}", e);
                    Ok(())
                }
            }
        }
    }

    fn call_socket_client_connected_internal(&self, client_id: &str) -> Result<()> {
        let key_result: mlua::Result<mlua::RegistryKey> =
            self.lua.named_registry_value(socket_registry_keys::ON_CLIENT_CONNECTED);
        if let Ok(key) = key_result {
            let callback: mlua::Function = self.lua.registry_value(&key)
                .map_err(|e| anyhow!("Failed to get socket_client_connected callback: {e}"))?;
            callback.call::<()>(client_id)
                .map_err(|e| anyhow!("socket_client_connected callback failed: {e}"))?;
        }
        Ok(())
    }

    /// Call the socket on_client_disconnected callback if registered.
    pub fn call_socket_client_disconnected(&self, client_id: &str) -> Result<()> {
        match self.call_socket_client_disconnected_internal(client_id) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict { Err(e) } else {
                    log::warn!("Lua socket_client_disconnected callback error: {}", e);
                    Ok(())
                }
            }
        }
    }

    fn call_socket_client_disconnected_internal(&self, client_id: &str) -> Result<()> {
        let key_result: mlua::Result<mlua::RegistryKey> =
            self.lua.named_registry_value(socket_registry_keys::ON_CLIENT_DISCONNECTED);
        if let Ok(key) = key_result {
            let callback: mlua::Function = self.lua.registry_value(&key)
                .map_err(|e| anyhow!("Failed to get socket_client_disconnected callback: {e}"))?;
            callback.call::<()>(client_id)
                .map_err(|e| anyhow!("socket_client_disconnected callback failed: {e}"))?;
        }
        Ok(())
    }

    /// Call the socket on_message callback with a client_id and JSON value.
    pub fn call_socket_message(&self, client_id: &str, message: serde_json::Value) -> Result<()> {
        match self.call_socket_message_internal(client_id, message) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.strict { Err(e) } else {
                    log::warn!("Lua socket_message callback error: {}", e);
                    Ok(())
                }
            }
        }
    }

    fn call_socket_message_internal(&self, client_id: &str, message: serde_json::Value) -> Result<()> {
        let key_result: mlua::Result<mlua::RegistryKey> =
            self.lua.named_registry_value(socket_registry_keys::ON_MESSAGE);
        if let Ok(key) = key_result {
            let callback: mlua::Function = self.lua.registry_value(&key)
                .map_err(|e| anyhow!("Failed to get socket_message callback: {e}"))?;
            let lua_value = crate::lua::primitives::json::json_to_lua(&self.lua, &message)
                .map_err(|e| anyhow!("Failed to convert JSON to Lua value: {e}"))?;
            callback.call::<()>((client_id, lua_value))
                .map_err(|e| anyhow!("socket_message callback failed: {e}"))?;
        }
        Ok(())
    }

    // =========================================================================
    // PTY Operations
    // =========================================================================

    /// Notify PTY output observers (fire-and-forget).
    ///
    /// Observers are called asynchronously and cannot affect data flow.
    /// Use this for logging, metrics, side effects.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Context containing agent_index, pty_index, peer_id
    /// * `data` - Raw PTY output bytes
    ///
    /// # Returns
    ///
    /// Number of observers notified.
    pub fn notify_pty_output_observers(
        &self,
        ctx: &PtyOutputContext,
        data: &[u8],
    ) -> usize {
        let result: mlua::Result<usize> = (|| {
            let ctx_table = self.lua.create_table()?;
            ctx_table.set("agent_index", ctx.agent_index)?;
            ctx_table.set("pty_index", ctx.pty_index)?;
            ctx_table.set("peer_id", ctx.peer_id.clone())?;

            let data_str = self.lua.create_string(data)?;

            let hooks: mlua::Table = self.lua.globals().get("hooks")?;
            let notify: mlua::Function = hooks.get("notify")?;
            notify.call::<usize>(("pty_output", ctx_table, data_str))
        })();

        match result {
            Ok(count) => count,
            Err(e) => {
                log::warn!("PTY output observer notification failed: {}", e);
                0
            }
        }
    }

    /// Call PTY output interceptors with context and data.
    ///
    /// Interceptors can transform or drop data. They run synchronously
    /// in the critical path - only use when transformation is needed.
    ///
    /// # Arguments
    ///
    /// * `ctx` - Context containing agent_index, pty_index, peer_id
    /// * `data` - Raw PTY output bytes
    ///
    /// # Returns
    ///
    /// - `Ok(Some(data))` - Transformed data to send
    /// - `Ok(None)` - Interceptor returned nil, drop this output
    /// - `Err(_)` - Error (Hub should send original data)
    ///
    /// # Usage
    ///
    /// ```ignore
    /// // Check for interceptors first (observers don't block)
    /// let final_data = if lua.has_interceptors("pty_output") {
    ///     match lua.call_pty_output_interceptors(&ctx, &data) {
    ///         Ok(Some(transformed)) => transformed,
    ///         Ok(None) => return, // drop
    ///         Err(_) => data, // fallback
    ///     }
    /// } else {
    ///     data
    /// };
    /// send(final_data);
    ///
    /// // Notify observers separately (async, never blocks)
    /// if lua.has_observers("pty_output") {
    ///     lua.notify_pty_output_observers(&ctx, &final_data);
    /// }
    /// ```
    pub fn call_pty_output_interceptors(
        &mut self,
        ctx: &PtyOutputContext,
        data: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        // Lazily initialize cached function and reusable context table
        if self.pty_hook_fn.is_none() {
            let f: mlua::Function = self.lua.load(
                r#"
                return function(ctx, data)
                    return hooks.call("pty_output", ctx, data)
                end
                "#
            ).eval()
                .map_err(|e| anyhow!("Failed to create PTY hook wrapper: {e}"))?;
            let fn_key = self.lua.create_registry_value(f)
                .map_err(|e| anyhow!("Failed to cache PTY hook function: {e}"))?;
            self.pty_hook_fn = Some(fn_key);

            let ctx_table = self.lua.create_table()
                .map_err(|e| anyhow!("Failed to create context table: {e}"))?;
            let ctx_key = self.lua.create_registry_value(ctx_table)
                .map_err(|e| anyhow!("Failed to cache PTY context table: {e}"))?;
            self.pty_hook_ctx = Some(ctx_key);
        }

        // Reuse cached context table — update fields in place
        let ctx_table: mlua::Table = self.lua.registry_value(self.pty_hook_ctx.as_ref().unwrap())
            .map_err(|e| anyhow!("Failed to retrieve cached PTY context table: {e}"))?;

        ctx_table.set("agent_index", ctx.agent_index)
            .map_err(|e| anyhow!("Failed to set agent_index: {e}"))?;
        ctx_table.set("pty_index", ctx.pty_index)
            .map_err(|e| anyhow!("Failed to set pty_index: {e}"))?;
        ctx_table.set("peer_id", ctx.peer_id.clone())
            .map_err(|e| anyhow!("Failed to set peer_id: {e}"))?;

        // Convert data to Lua string (binary-safe)
        let data_str = self.lua.create_string(data)
            .map_err(|e| anyhow!("Failed to create data string: {e}"))?;

        let func: mlua::Function = self.lua.registry_value(self.pty_hook_fn.as_ref().unwrap())
            .map_err(|e| anyhow!("Failed to retrieve cached PTY hook function: {e}"))?;

        let result: mlua::Result<Option<mlua::String>> = func.call((ctx_table, data_str));

        match result {
            Ok(Some(transformed)) => {
                Ok(Some(transformed.as_bytes().to_vec()))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(anyhow!("PTY output hook error: {e}")),
        }
    }

    // =========================================================================
    // Hub State Primitives
    // =========================================================================

    /// Register Hub state and connection primitives with the HandleCache.
    ///
    /// Call this after runtime creation to enable Hub state queries and
    /// connection URL access in Lua. Hub calls this during setup to wire
    /// the HandleCache.
    ///
    /// # Arguments
    ///
    /// * `handle_cache` - Thread-safe cache of agent handles and connection URL
    /// * `worktree_base` - Base directory for worktree storage
    /// * `server_id` - Server-assigned hub ID (set after registration)
    /// * `shared_state` - Shared hub state for agent queries
    ///
    /// # Errors
    ///
    /// Returns an error if registration fails.
    pub fn register_hub_primitives(
        &self,
        handle_cache: Arc<HandleCache>,
        worktree_base: PathBuf,
        hub_identifier: String,
        server_id: primitives::SharedServerId,
        shared_state: Arc<std::sync::RwLock<crate::hub::state::HubState>>,
        broker_connection: crate::broker::SharedBrokerConnection,
    ) -> Result<()> {
        primitives::register_hub(
            &self.lua,
            Arc::clone(&self.hub_event_sender),
            Arc::clone(&handle_cache),
            hub_identifier,
            server_id,
            shared_state,
            broker_connection,
        )
        .context("Failed to register Hub primitives")?;

        primitives::register_connection(
            &self.lua,
            Arc::clone(&self.hub_event_sender),
            Arc::clone(&handle_cache),
        )
        .context("Failed to register connection primitives")?;

        primitives::register_worktree(
            &self.lua,
            Arc::clone(&self.hub_event_sender),
            handle_cache,
            worktree_base,
        )
        .context("Failed to register worktree primitives")?;

        Ok(())
    }


    /// Poll WebSocket events via shared vec (test-only fallback).
    ///
    /// Production uses `HubEvent::WebSocketEvent` from background threads.
    #[cfg(test)]
    pub fn poll_websocket_events(&self) -> usize {
        primitives::websocket::poll_websocket_events(
            &self.lua,
            &self.websocket_registry,
        )
    }

    /// Set up a test event channel for the `HubEventSender`.
    ///
    /// Creates an unbounded channel, fills the shared `HubEventSender` so that
    /// Lua closures can send events, and returns the receiver for assertions.
    #[cfg(test)]
    pub(crate) fn setup_test_event_channel(&self) -> tokio::sync::mpsc::UnboundedReceiver<crate::hub::events::HubEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *self.hub_event_sender.lock().expect("HubEventSender mutex poisoned") = Some(tx);
        rx
    }

    /// Inject the Hub event channel sender into primitive registries.
    ///
    /// Called once during Hub initialization after the `LuaRuntime` is created.
    /// Enables Lua closures and background threads to send events directly to
    /// the Hub event loop.
    ///
    /// This fills the shared `HubEventSender` (captured by WebRTC, TUI, PTY,
    /// Hub, connection, and worktree closures) and configures the HTTP, WebSocket,
    /// timer, and watcher registries.
    pub(crate) fn set_hub_event_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>,
        tokio_handle: tokio::runtime::Handle,
    ) {
        // Fill the shared HubEventSender for all 6 event-driven Lua primitives
        *self.hub_event_sender.lock().expect("HubEventSender mutex poisoned") = Some(tx.clone());

        self.http_registry
            .lock()
            .expect("HttpAsyncEntries mutex poisoned")
            .set_hub_event_tx(tx.clone());
        self.websocket_registry
            .lock()
            .expect("WebSocketRegistry mutex poisoned")
            .set_hub_event_tx(tx.clone());
        self.timer_registry
            .lock()
            .expect("TimerEntries mutex poisoned")
            .set_event_channel(tx.clone(), tokio_handle.clone());
        self.watcher_registry
            .lock()
            .expect("WatcherEntries mutex poisoned")
            .set_hub_event_tx(tx.clone(), tokio_handle.clone());
    }

    /// Fire the Lua callback for a single completed HTTP response.
    ///
    /// Called from `handle_hub_event()` for `HubEvent::HttpResponse` events.
    pub(crate) fn fire_http_callback(
        &self,
        response: primitives::http::CompletedHttpResponse,
    ) {
        primitives::http::fire_single_http_callback(
            &self.lua,
            &self.http_registry,
            response,
        );
    }

    /// Fire the Lua callback for a single WebSocket event.
    ///
    /// Called from `handle_hub_event()` for `HubEvent::WebSocketEvent` events.
    pub(crate) fn fire_websocket_event(
        &self,
        event: primitives::websocket::WsEvent,
    ) {
        primitives::websocket::fire_single_websocket_event(
            &self.lua,
            &self.websocket_registry,
            event,
        );
    }

    /// Fire the Lua callback for a single timer event.
    ///
    /// Called from `handle_hub_event()` for `HubEvent::TimerFired` events.
    /// Looks up the callback in the timer registry, fires it, and cleans up
    /// one-shot entries.
    pub(crate) fn fire_timer_callback(&self, timer_id: &str) {
        primitives::timer::fire_single_timer(
            &self.lua,
            &self.timer_registry,
            timer_id,
        );
    }

    /// Get a reference to the inner Lua state.
    ///
    /// Used by Hub for ActionCable channel polling where direct Lua access
    /// is needed for callback dispatch.
    pub fn lua_ref(&self) -> &Lua {
        &self.lua
    }

    /// Get a reference to the ActionCable callback registry.
    ///
    /// Used by Hub to look up channel callbacks when firing AC messages
    /// and to clean up entries on unsubscribe/close.
    pub fn ac_callback_registry(&self) -> &primitives::ActionCableCallbackRegistry {
        &self.ac_callback_registry
    }

    /// Get a reference to the hub client callback registry.
    ///
    /// Used by Hub to look up connection callbacks when firing hub client messages
    /// and to clean up entries on close/disconnect.
    pub fn hub_client_callback_registry(&self) -> &primitives::HubClientCallbackRegistry {
        &self.hub_client_callback_registry
    }

    /// Get a reference to the hub client pending requests map.
    ///
    /// Used by Hub to deliver responses to blocking `hub_client.request()` calls.
    pub fn hub_client_pending_requests(&self) -> &primitives::HubClientPendingRequests {
        &self.hub_client_pending_requests
    }

    /// Get a reference to the hub client frame senders map.
    ///
    /// Used by Hub to register/deregister direct write channels per connection.
    /// `hub_client.request()` writes frames here to bypass the blocked event loop.
    pub fn hub_client_frame_senders(&self) -> &primitives::HubClientFrameSenders {
        &self.hub_client_frame_senders
    }

    // =========================================================================
    // Event System
    // =========================================================================

    /// Check if any event callbacks are registered for an event.
    ///
    /// Hub can use this as a fast-path check before preparing arguments
    /// for Lua event calls.
    ///
    /// # Arguments
    ///
    /// * `event_name` - The event name to check (e.g., "agent_created")
    #[must_use]
    pub fn has_event_callbacks(&self, event_name: &str) -> bool {
        self.event_callbacks
            .lock()
            .map(|cbs| cbs.has_callbacks(event_name))
            .unwrap_or(false)
    }

    /// Fire an event to all registered Lua callbacks.
    ///
    /// Iterates through all callbacks registered for the event and invokes
    /// each one. Errors in individual callbacks are logged but don't prevent
    /// other callbacks from being called.
    ///
    /// # Arguments
    ///
    /// * `event` - Event name
    /// * `args_fn` - Closure that builds the Lua arguments
    ///
    /// # Returns
    ///
    /// Ok if all callbacks completed (regardless of individual errors).
    ///
    /// # Example
    ///
    /// ```ignore
    /// lua.fire_event("agent_created", |lua| {
    ///     let t = lua.create_table().map_err(|e| anyhow!("{}", e))?;
    ///     t.set("id", agent_id).map_err(|e| anyhow!("{}", e))?;
    ///     Ok(mlua::Value::Table(t))
    /// })?;
    /// ```
    pub fn fire_event<F>(&self, event: &str, args_fn: F) -> Result<()>
    where
        F: Fn(&Lua) -> Result<mlua::Value>,
    {
        // Collect callbacks and their functions in one lock acquisition
        let callbacks_to_call: Vec<mlua::Function> = {
            let callbacks = self.event_callbacks.lock()
                .expect("Event callbacks mutex poisoned");
            callbacks
                .get_callbacks(event)
                .iter()
                .filter_map(|key| self.lua.registry_value::<mlua::Function>(key).ok())
                .collect()
        };
        // Lock released here

        if callbacks_to_call.is_empty() {
            return Ok(());
        }

        // Build args once
        let args = args_fn(&self.lua)?;

        // Call callbacks without holding the lock
        for callback in callbacks_to_call {
            if let Err(e) = callback.call::<()>(args.clone()) {
                log::error!("Event callback error for '{}': {}", event, e);
            }
        }

        Ok(())
    }

    /// Fire the "connection_code_ready" event with URL and QR ASCII art.
    ///
    /// Called by Hub when a connection URL is generated or regenerated.
    /// Generates ASCII art QR code lines for universal terminal display.
    ///
    /// # Arguments
    ///
    /// * `url` - The connection URL
    pub fn fire_connection_code_ready(&self, url: &str) -> Result<()> {
        if !self.has_event_callbacks("connection_code_ready") {
            return Ok(());
        }

        // Generate ASCII QR (generous max size - clients will re-render if needed)
        let qr_lines = crate::tui::generate_qr_code_lines(url, 200, 100);

        let url = url.to_string();

        self.fire_event("connection_code_ready", |lua| {
            let t = lua.create_table().map_err(|e| anyhow!("create_table: {e}"))?;
            t.set("url", url.clone()).map_err(|e| anyhow!("set url: {e}"))?;

            // Convert Vec<String> to Lua array
            let qr_array = lua.create_table().map_err(|e| anyhow!("create qr_array: {e}"))?;
            for (i, line) in qr_lines.iter().enumerate() {
                qr_array.set(i + 1, line.clone()).map_err(|e| anyhow!("set qr line: {e}"))?;
            }
            t.set("qr_ascii", qr_array).map_err(|e| anyhow!("set qr_ascii: {e}"))?;

            Ok(mlua::Value::Table(t))
        })
    }

    /// Fire the "connection_code_error" event.
    ///
    /// Called by Hub when connection URL generation fails.
    pub fn fire_connection_code_error(&self, error: &str) -> Result<()> {
        if !self.has_event_callbacks("connection_code_error") {
            return Ok(());
        }

        let error = error.to_string();
        self.fire_event("connection_code_error", |lua| {
            let s = lua.create_string(&error).map_err(|e| anyhow!("create_string: {e}"))?;
            Ok(mlua::Value::String(s))
        })
    }

    /// Fire an event with a JSON value as the Lua argument.
    ///
    /// Converts `value` to a Lua table/value via serde and dispatches
    /// to all registered callbacks for `event`. No-ops when no callbacks
    /// are registered for the event name.
    pub fn fire_json_event(&self, event: &str, value: &serde_json::Value) -> Result<()> {
        if !self.has_event_callbacks(event) {
            return Ok(());
        }

        let value = value.clone();

        self.fire_event(event, |lua| {
            crate::lua::primitives::json::json_to_lua(lua, &value)
                .map_err(|e| anyhow!("json_to_lua: {e}"))
        })
    }

    /// Fire the "command_message" event with the full message payload.
    ///
    /// Called by Hub when a command channel message should be handled by Lua.
    /// Lua handlers (e.g., `handlers/agents.lua`) listen for this event and
    /// route `create_agent`, `delete_agent`, etc. to their respective handlers.
    pub fn fire_command_message(&self, message: &serde_json::Value) -> Result<()> {
        self.fire_json_event("command_message", message)
    }

    /// Notify observers of a PTY notification event.
    ///
    /// Fires the `_pty_notification_raw` hook with a table containing:
    /// - `type`: "osc9" or "osc777"
    /// - `message`: notification message (osc9)
    /// - `title`/`body`: notification fields (osc777)
    /// - `agent_key`: agent identifier
    /// - `session_name`: PTY session name
    ///
    /// Lua enriches this with `already_notified` (from the Agent model)
    /// and re-dispatches as the public `pty_notification` event.
    pub fn notify_pty_notification(
        &mut self,
        agent_key: &str,
        session_name: &str,
        notification: &crate::agent::AgentNotification,
    ) {
        use crate::agent::AgentNotification;

        let result: mlua::Result<()> = (|| {
            let data = self.lua.create_table()?;
            data.set("agent_key", agent_key)?;
            data.set("session_name", session_name)?;

            match notification {
                AgentNotification::Osc9(msg) => {
                    data.set("type", "osc9")?;
                    data.set("message", msg.clone())?;
                }
                AgentNotification::Osc777 { title, body } => {
                    data.set("type", "osc777")?;
                    data.set("title", title.clone())?;
                    data.set("body", body.clone())?;
                }
            }

            let hooks: mlua::Table = self.lua.globals().get("hooks")?;
            let notify: mlua::Function = hooks.get("notify")?;
            notify.call::<mlua::Value>(("_pty_notification_raw", data))?;
            Ok(())
        })();

        if let Err(e) = result {
            log::warn!("PTY notification hook failed: {}", e);
        }

        // Arm the PTY input listener so keystrokes clear the notification.
        self.pty_input_listening = true;
    }

    /// Update per-client focus state in Lua's `pty_clients` module.
    ///
    /// Called when focus-in (`\x1b[I`) or focus-out (`\x1b[O`) sequences
    /// are detected in PTY input. The `peer_id` identifies the client
    /// ("tui" for TUI, browser identity key for WebRTC).
    pub fn set_pty_focused(
        &self,
        agent_index: usize,
        pty_index: usize,
        peer_id: &str,
        focused: bool,
    ) {
        let result: mlua::Result<()> = (|| {
            let func: mlua::Function = self.lua.globals().get("_set_pty_focused")?;
            func.call::<()>((agent_index, pty_index, peer_id, focused))?;
            Ok(())
        })();

        if let Err(e) = result {
            log::warn!("set_pty_focused failed: {}", e);
        }
    }

    /// Clear agent notification on PTY input if any are pending.
    ///
    /// Gated by `pty_input_listening` — when no notifications are pending
    /// (99.9% of keystrokes), this is a single bool check with no Lua call.
    /// When armed, calls `_on_pty_input(agent_index)` in Lua which clears
    /// the notification and returns whether any agents still have pending
    /// notifications. Disarms when none remain.
    ///
    /// Called from both browser (WebRTC) and TUI PTY input paths —
    /// the single convergence point for all input sources.
    pub fn notify_pty_input(&mut self, agent_index: usize) {
        if !self.pty_input_listening {
            return;
        }

        let result: mlua::Result<bool> = (|| {
            let func: mlua::Function = self.lua.globals().get("_on_pty_input")?;
            func.call::<bool>(agent_index)
        })();

        match result {
            Ok(keep_listening) => self.pty_input_listening = keep_listening,
            Err(e) => {
                log::warn!("PTY input notification clear failed: {}", e);
                self.pty_input_listening = false;
            }
        }
    }

    /// Dispatch a PTY OSC metadata event to the appropriate Lua hook.
    ///
    /// Maps `PtyEvent` variants to distinct hook names:
    /// - `TitleChanged` → `"pty_title_changed"` with `{ agent_key, session_name, title }`
    /// - `CwdChanged` → `"pty_cwd_changed"` with `{ agent_key, session_name, cwd }`
    /// - `PromptMark` → `"pty_prompt"` with `{ agent_key, session_name, mark, exit_code?, command? }`
    ///
    /// Other PtyEvent variants are ignored (they use different dispatch paths).
    pub fn notify_pty_osc_event(
        &self,
        agent_key: &str,
        session_name: &str,
        event: &crate::agent::pty::PtyEvent,
    ) {
        use crate::agent::pty::{PromptMark, PtyEvent};

        let result: mlua::Result<()> = (|| {
            let data = self.lua.create_table()?;
            data.set("agent_key", agent_key)?;
            data.set("session_name", session_name)?;

            let hook_name = match event {
                PtyEvent::TitleChanged(title) => {
                    data.set("title", title.as_str())?;
                    "pty_title_changed"
                }
                PtyEvent::CwdChanged(cwd) => {
                    data.set("cwd", cwd.as_str())?;
                    "pty_cwd_changed"
                }
                PtyEvent::PromptMark(mark) => {
                    match mark {
                        PromptMark::PromptStart => {
                            data.set("mark", "prompt_start")?;
                        }
                        PromptMark::CommandStart => {
                            data.set("mark", "command_start")?;
                        }
                        PromptMark::CommandExecuted(cmd) => {
                            data.set("mark", "command_executed")?;
                            if let Some(c) = cmd {
                                data.set("command", c.as_str())?;
                            }
                        }
                        PromptMark::CommandFinished(code) => {
                            data.set("mark", "command_finished")?;
                            if let Some(c) = code {
                                data.set("exit_code", *c)?;
                            }
                        }
                    }
                    "pty_prompt"
                }
                PtyEvent::CursorVisibilityChanged(visible) => {
                    data.set("visible", *visible)?;
                    "pty_cursor_visibility"
                }
                _ => return Ok(()), // Other PtyEvent variants use different paths
            };

            let hooks: mlua::Table = self.lua.globals().get("hooks")?;
            let notify: mlua::Function = hooks.get("notify")?;
            notify.call::<mlua::Value>((hook_name, data))?;
            Ok(())
        })();

        if let Err(e) = result {
            log::warn!("PTY OSC event hook failed: {}", e);
        }
    }

    /// Fire the "shutdown" event.
    ///
    /// Called by Hub when shutting down.
    pub fn fire_shutdown(&self) -> Result<()> {
        if !self.has_event_callbacks("shutdown") {
            return Ok(());
        }

        self.fire_event("shutdown", |_lua| Ok(mlua::Value::Nil))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::events::HubEvent;
    use crate::lua::primitives::webrtc::WebRtcSendRequest;
    use crate::lua::primitives::tui::TuiSendRequest;
    use crate::lua::primitives::pty::PtyRequest;

    #[test]
    fn test_runtime_creation() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        assert!(!runtime.strict);
    }

    #[test]
    fn test_base_path_env_override() {
        // Test that BOTSTER_LUA_PATH env var overrides the default
        // We test this by setting an env var and checking the result
        // (env vars can race between tests but this tests the logic, not the default)
        std::env::set_var("BOTSTER_LUA_PATH", "/test/override/path");
        let path = LuaRuntime::resolve_base_path();
        assert_eq!(path, PathBuf::from("/test/override/path"));
        std::env::remove_var("BOTSTER_LUA_PATH");
    }

    #[test]
    fn test_has_function_false_for_nonexistent() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        assert!(!runtime.has_function("nonexistent_function"));
    }

    #[test]
    fn test_log_primitive_registered() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        // The 'log' table should exist after primitives are registered
        let globals = runtime.lua().globals();
        let log_table: mlua::Result<mlua::Table> = globals.get("log");
        assert!(log_table.is_ok(), "log table should be registered");
    }

    #[test]
    fn test_package_path_configured() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let package: mlua::Table = runtime.lua().globals().get("package").unwrap();
        let path: String = package.get("path").unwrap();

        // Should contain the base path pattern
        let base_path = runtime.base_path().display().to_string();
        assert!(
            path.contains(&base_path),
            "package.path should contain base_path"
        );
    }

    #[test]
    fn test_update_package_path_adds_directory() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        // Get original package.path
        let package: mlua::Table = runtime.lua().globals().get("package").unwrap();
        let original_path: String = package.get("path").unwrap();

        // Update with a new directory
        let additional_path = PathBuf::from("/additional/lua/path");
        runtime.update_package_path(&additional_path).expect("Should update package.path");

        // Get updated package.path
        let updated_path: String = package.get("path").unwrap();

        // Should contain the new path and still have the original content
        assert!(
            updated_path.contains("/additional/lua/path"),
            "Updated package.path should contain the additional path"
        );
        assert!(
            updated_path.ends_with(&original_path),
            "Updated package.path should preserve original path at the end"
        );
    }

    #[test]
    fn test_has_hooks_false_when_no_hooks() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        // Load hooks module inline for testing
        runtime.lua().load(r#"
            hooks = {
                has_observers = function(event_name)
                    return false
                end,
                has_interceptors = function(event_name)
                    return false
                end
            }
        "#).exec().unwrap();

        assert!(!runtime.has_hooks("nonexistent_event"));
    }

    #[test]
    fn test_has_hooks_true_when_hooks_exist() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        // Load hooks module with observer/interceptor API
        runtime.lua().load(r#"
            hooks = {
                has_observers = function(event_name)
                    return event_name == "test_event"
                end,
                has_interceptors = function(event_name)
                    return false
                end
            }
        "#).exec().unwrap();

        assert!(runtime.has_hooks("test_event"));
        assert!(!runtime.has_hooks("other_event"));
    }

    #[test]
    fn test_call_interceptors_returns_transformed_data() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        // Load hooks module that transforms data
        runtime.lua().load(r#"
            hooks = {
                call = function(event_name, data)
                    return data .. "_transformed"
                end
            }
        "#).exec().unwrap();

        let result = runtime.call_interceptors("test_event", "hello");
        assert_eq!(result, Some("hello_transformed".to_string()));
    }

    #[test]
    fn test_call_interceptors_returns_none_on_drop() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        // Load hooks module that drops data
        runtime.lua().load(r#"
            hooks = {
                call = function(event_name, data)
                    return nil
                end
            }
        "#).exec().unwrap();

        let result = runtime.call_interceptors("test_event", "hello");
        assert_eq!(result, None);
    }

    // =========================================================================
    // WebRTC Callback Tests
    // =========================================================================

    #[test]
    fn test_webrtc_table_registered() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let globals = runtime.lua().globals();
        let webrtc_table: mlua::Result<mlua::Table> = globals.get("webrtc");
        assert!(webrtc_table.is_ok(), "webrtc table should be registered");
    }

    #[test]
    fn test_has_webrtc_callbacks_false_initially() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        assert!(!runtime.has_webrtc_callbacks());
    }

    #[test]
    fn test_has_webrtc_callbacks_true_after_registration() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            webrtc.on_message(function(peer_id, msg) end)
        "#).exec().unwrap();

        assert!(runtime.has_webrtc_callbacks());
    }

    #[test]
    fn test_call_peer_connected_invokes_callback() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            connected_peer = nil
            webrtc.on_peer_connected(function(peer_id)
                connected_peer = peer_id
            end)
        "#).exec().unwrap();

        runtime.call_peer_connected("test-peer-123").expect("Should call callback");

        let result: String = runtime.lua().globals().get("connected_peer").unwrap();
        assert_eq!(result, "test-peer-123");
    }

    #[test]
    fn test_call_peer_disconnected_invokes_callback() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            disconnected_peer = nil
            webrtc.on_peer_disconnected(function(peer_id)
                disconnected_peer = peer_id
            end)
        "#).exec().unwrap();

        runtime.call_peer_disconnected("test-peer-456").expect("Should call callback");

        let result: String = runtime.lua().globals().get("disconnected_peer").unwrap();
        assert_eq!(result, "test-peer-456");
    }

    #[test]
    fn test_call_webrtc_message_invokes_callback_with_json() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            received_peer = nil
            received_type = nil
            received_value = nil
            webrtc.on_message(function(peer_id, msg)
                received_peer = peer_id
                received_type = msg.type
                received_value = msg.value
            end)
        "#).exec().unwrap();

        let msg = serde_json::json!({
            "type": "test_message",
            "value": 42
        });

        runtime.call_webrtc_message("peer-789", msg).expect("Should call callback");

        let peer: String = runtime.lua().globals().get("received_peer").unwrap();
        let msg_type: String = runtime.lua().globals().get("received_type").unwrap();
        let value: i64 = runtime.lua().globals().get("received_value").unwrap();

        assert_eq!(peer, "peer-789");
        assert_eq!(msg_type, "test_message");
        assert_eq!(value, 42);
    }

    #[test]
    fn test_webrtc_send_delivers_events() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let mut rx = runtime.setup_test_event_channel();

        runtime.lua().load(r#"
            webrtc.send("peer-1", { type = "hello" })
            webrtc.send("peer-2", { type = "world" })
        "#).exec().unwrap();

        let event1 = rx.try_recv().expect("Should receive first event");
        let event2 = rx.try_recv().expect("Should receive second event");
        assert!(rx.try_recv().is_err(), "No more events expected");

        match event1 {
            HubEvent::WebRtcSend(WebRtcSendRequest::Json { peer_id, .. }) => {
                assert_eq!(peer_id, "peer-1");
            }
            _ => panic!("Expected WebRtcSend Json event"),
        }

        match event2 {
            HubEvent::WebRtcSend(WebRtcSendRequest::Json { peer_id, .. }) => {
                assert_eq!(peer_id, "peer-2");
            }
            _ => panic!("Expected WebRtcSend Json event"),
        }
    }

    #[test]
    fn test_webrtc_callback_sends_event() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let mut rx = runtime.setup_test_event_channel();

        runtime.lua().load(r#"
            webrtc.on_message(function(peer_id, msg)
                if msg.type == "ping" then
                    webrtc.send(peer_id, { type = "pong" })
                end
            end)
        "#).exec().unwrap();

        let ping = serde_json::json!({ "type": "ping" });
        runtime.call_webrtc_message("peer-echo", ping).expect("Should call callback");

        let event = rx.try_recv().expect("Should receive event");
        assert!(rx.try_recv().is_err(), "No more events expected");

        match event {
            HubEvent::WebRtcSend(WebRtcSendRequest::Json { peer_id, data }) => {
                assert_eq!(peer_id, "peer-echo");
                assert_eq!(data["type"], "pong");
            }
            _ => panic!("Expected WebRtcSend Json event"),
        }
    }

    // =========================================================================
    // PTY Primitive Tests
    // =========================================================================

    #[test]
    fn test_hub_table_registered() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let globals = runtime.lua().globals();
        let hub_table: mlua::Result<mlua::Table> = globals.get("hub");
        assert!(hub_table.is_ok(), "hub table should be registered");
    }

    #[test]
    fn test_create_pty_forwarder_function_exists() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let globals = runtime.lua().globals();
        let webrtc: mlua::Table = globals.get("webrtc").expect("webrtc should exist");
        let create_fn: mlua::Result<mlua::Function> = webrtc.get("create_pty_forwarder");
        assert!(create_fn.is_ok(), "create_pty_forwarder should exist");
    }

    #[test]
    fn test_pty_events_empty_initially() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let mut rx = runtime.setup_test_event_channel();
        assert!(rx.try_recv().is_err(), "No events expected initially");
    }

    #[test]
    fn test_pty_write_sends_event() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let mut rx = runtime.setup_test_event_channel();

        runtime.lua().load(r#"
            hub.write_pty(0, 0, "hello")
        "#).exec().unwrap();

        let event = rx.try_recv().expect("Should receive event");
        assert!(rx.try_recv().is_err(), "No more events expected");

        match event {
            HubEvent::LuaPtyRequest(PtyRequest::WritePty { agent_index, pty_index, data }) => {
                assert_eq!(agent_index, 0);
                assert_eq!(pty_index, 0);
                assert_eq!(data, b"hello");
            }
            _ => panic!("Expected LuaPtyRequest WritePty event"),
        }
    }

    #[test]
    fn test_pty_resize_sends_event() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let mut rx = runtime.setup_test_event_channel();

        runtime.lua().load(r#"
            hub.resize_pty(1, 0, 50, 100)
        "#).exec().unwrap();

        let event = rx.try_recv().expect("Should receive event");
        assert!(rx.try_recv().is_err(), "No more events expected");

        match event {
            HubEvent::LuaPtyRequest(PtyRequest::ResizePty { agent_index, pty_index, rows, cols }) => {
                assert_eq!(agent_index, 1);
                assert_eq!(pty_index, 0);
                assert_eq!(rows, 50);
                assert_eq!(cols, 100);
            }
            _ => panic!("Expected LuaPtyRequest ResizePty event"),
        }
    }

    #[test]
    fn test_create_forwarder_sends_event() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let mut rx = runtime.setup_test_event_channel();

        runtime.lua().load(r#"
            forwarder = webrtc.create_pty_forwarder({
                peer_id = "test-browser",
                agent_index = 0,
                pty_index = 0,
                subscription_id = "sub_1_test",
            })
        "#).exec().unwrap();

        let event = rx.try_recv().expect("Should receive event");
        assert!(rx.try_recv().is_err(), "No more events expected");

        match event {
            HubEvent::LuaPtyRequest(PtyRequest::CreateForwarder(req)) => {
                assert_eq!(req.peer_id, "test-browser");
                assert_eq!(req.agent_index, 0);
                assert_eq!(req.pty_index, 0);
                assert_eq!(req.subscription_id, "sub_1_test");
            }
            _ => panic!("Expected LuaPtyRequest CreateForwarder event"),
        }
    }

    #[test]
    fn test_forwarder_handle_methods() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            forwarder = webrtc.create_pty_forwarder({
                peer_id = "browser-xyz",
                agent_index = 2,
                pty_index = 1,
                subscription_id = "sub_2_test",
            })
        "#).exec().unwrap();

        let id: String = runtime.lua().load("return forwarder:id()").eval().unwrap();
        assert_eq!(id, "browser-xyz:2:1");

        let active: bool = runtime.lua().load("return forwarder:is_active()").eval().unwrap();
        assert!(active);

        runtime.lua().load("forwarder:stop()").exec().unwrap();

        let active_after: bool = runtime.lua().load("return forwarder:is_active()").eval().unwrap();
        assert!(!active_after);
    }

    #[test]
    fn test_call_pty_output_interceptors_passthrough() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");

        // Set up hooks that pass through unchanged
        runtime.lua().load(r#"
            hooks = {
                call = function(event_name, ctx, data)
                    return data
                end
            }
        "#).exec().unwrap();

        let ctx = PtyOutputContext {
            agent_index: 0,
            pty_index: 0,
            peer_id: "test-peer".to_string(),
        };

        let result = runtime.call_pty_output_interceptors(&ctx, b"hello world").unwrap();
        assert_eq!(result, Some(b"hello world".to_vec()));
    }

    #[test]
    fn test_call_pty_output_interceptors_transform() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");

        // Set up hooks that transform data
        runtime.lua().load(r#"
            hooks = {
                call = function(event_name, ctx, data)
                    return data .. " transformed"
                end
            }
        "#).exec().unwrap();

        let ctx = PtyOutputContext {
            agent_index: 0,
            pty_index: 0,
            peer_id: "test-peer".to_string(),
        };

        let result = runtime.call_pty_output_interceptors(&ctx, b"hello").unwrap();
        assert_eq!(result, Some(b"hello transformed".to_vec()));
    }

    #[test]
    fn test_call_pty_output_interceptors_drop() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");

        // Set up hooks that drop data
        runtime.lua().load(r#"
            hooks = {
                call = function(event_name, ctx, data)
                    return nil
                end
            }
        "#).exec().unwrap();

        let ctx = PtyOutputContext {
            agent_index: 0,
            pty_index: 0,
            peer_id: "test-peer".to_string(),
        };

        let result = runtime.call_pty_output_interceptors(&ctx, b"hello").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_call_pty_output_interceptors_receives_context() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");

        // Set up hooks that use context
        runtime.lua().load(r#"
            received_ctx = nil
            hooks = {
                call = function(event_name, ctx, data)
                    received_ctx = ctx
                    return data
                end
            }
        "#).exec().unwrap();

        let ctx = PtyOutputContext {
            agent_index: 3,
            pty_index: 1,
            peer_id: "context-test-peer".to_string(),
        };

        runtime.call_pty_output_interceptors(&ctx, b"test").unwrap();

        let agent_idx: usize = runtime.lua().load("return received_ctx.agent_index").eval().unwrap();
        let pty_idx: usize = runtime.lua().load("return received_ctx.pty_index").eval().unwrap();
        let peer_id: String = runtime.lua().load("return received_ctx.peer_id").eval().unwrap();

        assert_eq!(agent_idx, 3);
        assert_eq!(pty_idx, 1);
        assert_eq!(peer_id, "context-test-peer");
    }

    // =========================================================================
    // Event System Tests
    // =========================================================================

    #[test]
    fn test_events_table_registered() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let globals = runtime.lua().globals();
        let events_table: mlua::Result<mlua::Table> = globals.get("events");
        assert!(events_table.is_ok(), "events table should be registered");
    }

    #[test]
    fn test_has_event_callbacks_false_initially() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        assert!(!runtime.has_event_callbacks("agent_created"));
        assert!(!runtime.has_event_callbacks("agent_deleted"));
        assert!(!runtime.has_event_callbacks("shutdown"));
    }

    #[test]
    fn test_has_event_callbacks_true_after_registration() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            events.on("agent_created", function(info) end)
        "#).exec().unwrap();

        assert!(runtime.has_event_callbacks("agent_created"));
        assert!(!runtime.has_event_callbacks("agent_deleted"));
    }

    #[test]
    fn test_fire_event_invokes_callback() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            received_value = nil
            events.on("test_event", function(value)
                received_value = value
            end)
        "#).exec().unwrap();

        runtime.fire_event("test_event", |lua| {
            Ok(mlua::Value::String(lua.create_string("hello").unwrap()))
        }).expect("Should fire event");

        let received: String = runtime.lua().globals().get("received_value").unwrap();
        assert_eq!(received, "hello");
    }

    #[test]
    fn test_fire_event_invokes_multiple_callbacks() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            call_count = 0
            events.on("test_event", function() call_count = call_count + 1 end)
            events.on("test_event", function() call_count = call_count + 1 end)
            events.on("test_event", function() call_count = call_count + 1 end)
        "#).exec().unwrap();

        runtime.fire_event("test_event", |_lua| Ok(mlua::Value::Nil)).unwrap();

        let count: i32 = runtime.lua().globals().get("call_count").unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_fire_shutdown_invokes_callback() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            shutdown_called = false
            events.on("shutdown", function()
                shutdown_called = true
            end)
        "#).exec().unwrap();

        runtime.fire_shutdown().expect("Should fire event");

        let called: bool = runtime.lua().globals().get("shutdown_called").unwrap();
        assert!(called);
    }

    #[test]
    fn test_hub_events_empty_initially() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let mut rx = runtime.setup_test_event_channel();
        assert!(rx.try_recv().is_err(), "No events expected initially");
    }

    // =========================================================================
    // TUI Callback Tests
    // =========================================================================

    #[test]
    fn test_tui_table_registered() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let globals = runtime.lua().globals();
        let tui_table: mlua::Result<mlua::Table> = globals.get("tui");
        assert!(tui_table.is_ok(), "tui table should be registered");
    }

    #[test]
    fn test_has_tui_callbacks_false_initially() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        assert!(!runtime.has_tui_callbacks());
    }

    #[test]
    fn test_has_tui_callbacks_true_after_registration() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            tui.on_message(function(msg) end)
        "#).exec().unwrap();

        assert!(runtime.has_tui_callbacks());
    }

    #[test]
    fn test_call_tui_connected_invokes_callback() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            tui_connected = false
            tui.on_connected(function()
                tui_connected = true
            end)
        "#).exec().unwrap();

        runtime.call_tui_connected().expect("Should call callback");

        let result: bool = runtime.lua().globals().get("tui_connected").unwrap();
        assert!(result);
    }

    #[test]
    fn test_call_tui_disconnected_invokes_callback() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            tui_disconnected = false
            tui.on_disconnected(function()
                tui_disconnected = true
            end)
        "#).exec().unwrap();

        runtime.call_tui_disconnected().expect("Should call callback");

        let result: bool = runtime.lua().globals().get("tui_disconnected").unwrap();
        assert!(result);
    }

    #[test]
    fn test_call_tui_message_invokes_callback_with_json() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            received_tui_type = nil
            received_tui_value = nil
            tui.on_message(function(msg)
                received_tui_type = msg.type
                received_tui_value = msg.value
            end)
        "#).exec().unwrap();

        let msg = serde_json::json!({
            "type": "resize",
            "value": 80
        });

        runtime.call_tui_message(msg).expect("Should call callback");

        let msg_type: String = runtime.lua().globals().get("received_tui_type").unwrap();
        let value: i64 = runtime.lua().globals().get("received_tui_value").unwrap();

        assert_eq!(msg_type, "resize");
        assert_eq!(value, 80);
    }

    #[test]
    fn test_tui_send_delivers_events() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let mut rx = runtime.setup_test_event_channel();

        runtime.lua().load(r#"
            tui.send({ type = "agent_list" })
            tui.send({ type = "status" })
        "#).exec().unwrap();

        let event1 = rx.try_recv().expect("Should receive first event");
        let event2 = rx.try_recv().expect("Should receive second event");
        assert!(rx.try_recv().is_err(), "No more events expected");

        match event1 {
            HubEvent::TuiSend(TuiSendRequest::Json { data }) => {
                assert_eq!(data["type"], "agent_list");
            }
            _ => panic!("Expected TuiSend Json event"),
        }

        match event2 {
            HubEvent::TuiSend(TuiSendRequest::Json { data }) => {
                assert_eq!(data["type"], "status");
            }
            _ => panic!("Expected TuiSend Json event"),
        }
    }

    #[test]
    fn test_tui_callback_sends_event() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let mut rx = runtime.setup_test_event_channel();

        runtime.lua().load(r#"
            tui.on_message(function(msg)
                if msg.type == "list_agents" then
                    tui.send({ type = "agent_list", count = 0 })
                end
            end)
        "#).exec().unwrap();

        let msg = serde_json::json!({ "type": "list_agents" });
        runtime.call_tui_message(msg).expect("Should call callback");

        let event = rx.try_recv().expect("Should receive event");
        assert!(rx.try_recv().is_err(), "No more events expected");

        match event {
            HubEvent::TuiSend(TuiSendRequest::Json { data }) => {
                assert_eq!(data["type"], "agent_list");
                assert_eq!(data["count"], 0);
            }
            _ => panic!("Expected TuiSend Json event"),
        }
    }

    /// Verifies that pre-seeding package.loaded makes modules available via require().
    /// This simulates what load_embedded() does in release builds.
    #[test]
    fn test_package_loaded_preseeding_enables_require() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let lua = runtime.lua();

        // Simulate what load_embedded does: eval a module and put it in package.loaded
        let module: mlua::Value = lua
            .load(r#"
                local M = {}
                M.value = 42
                function M.get_name() return "test_module" end
                return M
            "#)
            .eval()
            .expect("module should eval");

        let package_loaded: mlua::Table = lua
            .globals()
            .get::<mlua::Table>("package")
            .and_then(|pkg| pkg.get::<mlua::Table>("loaded"))
            .expect("package.loaded should exist");

        package_loaded
            .set("hub.test_module", module)
            .expect("should seed package.loaded");

        // Now require() should find it without filesystem
        let result: i32 = lua
            .load(r#"return require("hub.test_module").value"#)
            .eval()
            .expect("require should resolve from package.loaded");
        assert_eq!(result, 42);

        let name: String = lua
            .load(r#"return require("hub.test_module").get_name()"#)
            .eval()
            .expect("require should return full module");
        assert_eq!(name, "test_module");
    }

    /// Verifies that pre-seeded modules can require each other (dependency chains).
    #[test]
    fn test_package_loaded_preseeding_cross_module_require() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let lua = runtime.lua();

        let package_loaded: mlua::Table = lua
            .globals()
            .get::<mlua::Table>("package")
            .and_then(|pkg| pkg.get::<mlua::Table>("loaded"))
            .expect("package.loaded should exist");

        // Seed "hub.state" first
        let state: mlua::Value = lua
            .load(r#"
                local M = {}
                M.agents = {}
                return M
            "#)
            .eval()
            .unwrap();
        package_loaded.set("hub.state", state).unwrap();

        // Seed "hub.hooks" that depends on nothing
        let hooks: mlua::Value = lua
            .load(r#"
                local M = {}
                function M.notify(event) end
                return M
            "#)
            .eval()
            .unwrap();
        package_loaded.set("hub.hooks", hooks).unwrap();

        // Now a script that requires both should work
        let result: bool = lua
            .load(r#"
                local state = require("hub.state")
                local hooks = require("hub.hooks")
                return type(state.agents) == "table" and type(hooks.notify) == "function"
            "#)
            .eval()
            .expect("cross-module require should work");
        assert!(result, "Both modules should resolve from package.loaded");
    }

    /// Verifies the embedded searcher resolves cross-dependent modules lazily.
    ///
    /// Simulates the real dependency graph where handlers require lib modules,
    /// and lib modules require hub modules. The searcher must resolve these
    /// on-demand regardless of iteration order.
    #[test]
    fn test_embedded_searcher_resolves_cross_dependencies() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let lua = runtime.lua();

        // Build a fake _EMBEDDED_MODULES table with cross-tier dependencies:
        // handlers.agents requires lib.agent, lib.agent requires hub.state
        lua.load(
            r#"
            _EMBEDDED_MODULES = {
                ["hub.state"] = 'local M = {}; M.agents = {}; return M',
                ["hub.hooks"] = 'local M = {}; function M.notify() end; return M',
                ["lib.agent"] = [[
                    local state = require("hub.state")
                    local hooks = require("hub.hooks")
                    local M = {}
                    M.state_ref = state
                    return M
                ]],
                ["lib.config_resolver"] = 'local M = {}; return M',
                ["handlers.agents"] = [[
                    local Agent = require("lib.agent")
                    local ConfigResolver = require("lib.config_resolver")
                    local M = {}
                    M.agent_ref = Agent
                    return M
                ]],
            }

            -- Install the same searcher logic as install_embedded_searcher (appended as fallback)
            local embedded = _EMBEDDED_MODULES
            table.insert(package.searchers, function(module_name)
                local source = embedded[module_name]
                if source then
                    local fn, err = load(source, "=" .. module_name:gsub("%.", "/") .. ".lua")
                    if fn then
                        return fn
                    else
                        return "\n\tembedded load error: " .. (err or "unknown")
                    end
                end
                return "\n\tno embedded module '" .. module_name .. "'"
            end)
            _EMBEDDED_MODULES = nil
            "#,
        )
        .exec()
        .expect("Searcher setup should succeed");

        // Now require handlers.agents — this triggers the full dependency chain:
        // handlers.agents → lib.agent → hub.state, hub.hooks
        // handlers.agents → lib.config_resolver
        let result: bool = lua
            .load(
                r#"
                local agents_handler = require("handlers.agents")
                local agent_lib = require("lib.agent")
                local state = require("hub.state")
                -- Verify the chain resolved correctly
                return agents_handler.agent_ref == agent_lib
                   and agent_lib.state_ref == state
                   and type(state.agents) == "table"
                "#,
            )
            .eval()
            .expect("Cross-dependency require chain should resolve");
        assert!(
            result,
            "Embedded searcher should resolve transitive dependencies lazily"
        );
    }

    /// Verifies the embedded searcher is appended (last position) so filesystem
    /// paths take priority. This is critical for the override chain:
    /// project root > userspace > embedded.
    #[test]
    fn test_embedded_searcher_is_last_in_package_searchers() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let lua = runtime.lua();

        // Count searchers before installing embedded
        let count_before: usize = lua
            .load("return #package.searchers")
            .eval()
            .expect("Should get searcher count");

        // Set up a fake embedded module and install the searcher
        lua.load(
            r#"
            _EMBEDDED_MODULES = {
                ["test.module"] = 'return { name = "embedded" }',
            }
            local embedded = _EMBEDDED_MODULES
            table.insert(package.searchers, function(module_name)
                local source = embedded[module_name]
                if source then
                    local fn, err = load(source, "=" .. module_name:gsub("%.", "/") .. ".lua")
                    if fn then return fn
                    else return "\n\tembedded load error: " .. (err or "unknown") end
                end
                return "\n\tno embedded module '" .. module_name .. "'"
            end)
            _EMBEDDED_MODULES = nil
            "#,
        )
        .exec()
        .expect("Searcher setup should succeed");

        // Verify searcher was appended (last position)
        let count_after: usize = lua
            .load("return #package.searchers")
            .eval()
            .expect("Should get searcher count");

        assert_eq!(
            count_after,
            count_before + 1,
            "Should have exactly one more searcher"
        );

        // The embedded searcher should be the LAST one
        let is_last: bool = lua
            .load(
                r#"
                local last = package.searchers[#package.searchers]
                -- Call it with our test module to verify it's our searcher
                local result = last("test.module")
                return type(result) == "function"
                "#,
            )
            .eval()
            .expect("Should check last searcher");

        assert!(
            is_last,
            "The last searcher should be our embedded searcher that finds test.module"
        );
    }

    /// Verifies that filesystem paths take priority over the embedded searcher.
    ///
    /// When a module exists both in embedded and on the filesystem (via
    /// `package.path`), the filesystem version should win because the embedded
    /// searcher is appended last.
    #[test]
    fn test_filesystem_overrides_embedded_searcher() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let lua = runtime.lua();

        // Create a temp directory with a filesystem module
        let tmp = std::env::temp_dir().join("botster_lua_priority_test");
        let _ = std::fs::create_dir_all(&tmp);
        std::fs::write(
            tmp.join("priority_test.lua"),
            r#"return { source = "filesystem" }"#,
        )
        .expect("Should write test file");

        // Add the temp dir to package.path (simulating userspace/project root)
        let prepend_path = format!("{}/?.lua", tmp.display());
        lua.load(&format!(
            r#"package.path = "{}" .. ";" .. package.path"#,
            prepend_path,
        ))
        .exec()
        .expect("Should update package.path");

        // Install embedded searcher with the SAME module name but different content
        lua.load(
            r#"
            _EMBEDDED_MODULES = {
                ["priority_test"] = 'return { source = "embedded" }',
            }
            local embedded = _EMBEDDED_MODULES
            table.insert(package.searchers, function(module_name)
                local source = embedded[module_name]
                if source then
                    local fn, err = load(source, "=" .. module_name:gsub("%.", "/") .. ".lua")
                    if fn then return fn
                    else return "\n\tembedded load error: " .. (err or "unknown") end
                end
                return "\n\tno embedded module '" .. module_name .. "'"
            end)
            _EMBEDDED_MODULES = nil
            "#,
        )
        .exec()
        .expect("Searcher setup should succeed");

        // require should find the FILESYSTEM version (not embedded)
        let source: String = lua
            .load(r#"return require("priority_test").source"#)
            .eval()
            .expect("Should require priority_test");

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(
            source, "filesystem",
            "Filesystem module should override embedded (embedded searcher is fallback)"
        );
    }

    // =========================================================================
    // Delivery Pipeline Tests — NULL Userdata & Event Routing
    // =========================================================================

    /// Verifies `fire_json_event` converts JSON null to Lua nil, not userdata.
    ///
    /// `lua.to_value()` maps JSON null to `Value::NULL` (light-userdata),
    /// which is truthy in Lua. This causes crashes when Lua code concatenates
    /// or compares nil-expected fields (e.g., `config_resolver.lua:238`).
    /// The fix is to use `json_to_lua()` from `primitives/json.rs`.
    #[test]
    fn test_fire_json_event_null_is_nil_not_userdata() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        // Register callback that checks the type of a null field
        runtime
            .lua()
            .load(
                r#"
            null_field_type = "not_set"
            null_field_is_nil = false
            events.on("test_null", function(data)
                null_field_type = type(data.nullable_field)
                null_field_is_nil = (data.nullable_field == nil)
            end)
        "#,
            )
            .exec()
            .unwrap();

        let payload = serde_json::json!({
            "name": "test",
            "nullable_field": null
        });

        runtime
            .fire_json_event("test_null", &payload)
            .expect("Should fire event");

        // JSON null should become Lua nil (type "nil"), not userdata
        let field_type: String = runtime
            .lua()
            .globals()
            .get("null_field_type")
            .unwrap();
        let is_nil: bool = runtime
            .lua()
            .globals()
            .get("null_field_is_nil")
            .unwrap();

        assert_eq!(
            field_type, "nil",
            "JSON null should map to Lua nil, got '{}' (userdata = mlua NULL sentinel)",
            field_type
        );
        assert!(
            is_nil,
            "JSON null field should be == nil in Lua"
        );
    }

    /// Verifies `call_tui_message` converts JSON null to Lua nil, not userdata.
    ///
    /// Same root cause as `fire_json_event` — both use `lua.to_value()`.
    /// TUI messages with null fields (e.g., optional `profile_name`) must
    /// arrive as nil in Lua, not as truthy userdata.
    #[test]
    fn test_call_tui_message_null_is_nil_not_userdata() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime
            .lua()
            .load(
                r#"
            tui_null_type = "not_set"
            tui_null_is_nil = false
            tui.on_message(function(msg)
                tui_null_type = type(msg.optional_field)
                tui_null_is_nil = (msg.optional_field == nil)
            end)
        "#,
            )
            .exec()
            .unwrap();

        let msg = serde_json::json!({
            "type": "test",
            "optional_field": null
        });

        runtime
            .call_tui_message(msg)
            .expect("Should call callback");

        let field_type: String = runtime
            .lua()
            .globals()
            .get("tui_null_type")
            .unwrap();
        let is_nil: bool = runtime
            .lua()
            .globals()
            .get("tui_null_is_nil")
            .unwrap();

        assert_eq!(
            field_type, "nil",
            "JSON null in TUI message should map to Lua nil, got '{}'",
            field_type
        );
        assert!(
            is_nil,
            "JSON null field in TUI message should be == nil in Lua"
        );
    }

    /// Verifies `fire_json_event` skips null values in nested objects.
    ///
    /// `json_to_lua()` skips null keys entirely in objects (they become
    /// absent = nil in Lua). This test ensures nested null fields don't
    /// leak as userdata through the conversion.
    #[test]
    fn test_fire_json_event_nested_null_fields_absent() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime
            .lua()
            .load(
                r#"
            nested_has_key = true
            events.on("test_nested_null", function(data)
                -- rawget avoids __index metamethods; absent key = nil
                nested_has_key = rawget(data.config, "profile") ~= nil
            end)
        "#,
            )
            .exec()
            .unwrap();

        let payload = serde_json::json!({
            "config": {
                "name": "test",
                "profile": null
            }
        });

        runtime
            .fire_json_event("test_nested_null", &payload)
            .expect("Should fire event");

        let has_key: bool = runtime
            .lua()
            .globals()
            .get("nested_has_key")
            .unwrap();

        assert!(
            !has_key,
            "Null field in nested object should be absent (nil), not present as userdata"
        );
    }

    /// Verifies that `call_tui_message` delivers JSON to Lua callbacks and
    /// that the callback can successfully queue a `HubRequest::Quit`.
    ///
    /// This exercises the TUI→Hub message delivery pipeline:
    /// `call_tui_message()` → Lua callback → `hub.quit()` → `HubRequest::Quit`.
    ///
    /// NOTE: `hub.quit()` is registered by `register_hub_primitives()`, which
    /// requires `HandleCache` etc. In tests, we verify the Lua callback receives
    /// the message correctly and can interact with hub primitives. The quit test
    /// in `primitives/hub.rs` already proves `hub.quit()` queues `HubRequest::Quit`.
    #[test]
    fn test_tui_message_delivers_nested_json_to_callback() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        // Verify the callback receives the full nested JSON structure
        runtime
            .lua()
            .load(
                r#"
            received_sub_id = nil
            received_data_type = nil
            tui.on_message(function(msg)
                received_sub_id = msg.subscriptionId
                if msg.data then
                    received_data_type = msg.data.type
                end
            end)
        "#,
            )
            .exec()
            .unwrap();

        let msg = serde_json::json!({
            "subscriptionId": "tui_hub",
            "data": { "type": "quit" }
        });

        runtime
            .call_tui_message(msg)
            .expect("Should call callback");

        let sub_id: String = runtime
            .lua()
            .globals()
            .get("received_sub_id")
            .unwrap();
        let data_type: String = runtime
            .lua()
            .globals()
            .get("received_data_type")
            .unwrap();

        assert_eq!(sub_id, "tui_hub");
        assert_eq!(data_type, "quit");
    }

    /// Verifies that a Hub event fires through to the TUI event channel.
    ///
    /// When `fire_json_event("agent_created", ...)` fires, Lua observers
    /// (e.g., `connections.lua`) should call `tui.send()` to relay the event
    /// to the TUI. This test verifies that chain works end-to-end at the
    /// Rust/Lua boundary.
    #[test]
    fn test_hub_event_reaches_tui_event_channel() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let mut rx = runtime.setup_test_event_channel();

        // Set up an event handler that relays to TUI
        runtime
            .lua()
            .load(
                r#"
            events.on("agent_created", function(data)
                tui.send({
                    type = "agent_created",
                    agent_id = data.id,
                })
            end)
        "#,
            )
            .exec()
            .unwrap();

        let payload = serde_json::json!({
            "id": "owner-repo-42",
            "status": "running"
        });

        runtime
            .fire_json_event("agent_created", &payload)
            .expect("Should fire event");

        let event = rx.try_recv().expect("Hub event should produce TUI send event");
        assert!(rx.try_recv().is_err(), "No more events expected");

        match event {
            HubEvent::TuiSend(TuiSendRequest::Json { data }) => {
                assert_eq!(data["type"], "agent_created");
                assert_eq!(data["agent_id"], "owner-repo-42");
            }
            _ => panic!("Expected TuiSend Json event"),
        }
    }

    // =========================================================================
    // PTY Input Notification Gating Tests
    // =========================================================================

    /// Set up a minimal Lua environment for notification tests.
    ///
    /// Mirrors the production code structure in `handlers/connections.lua`:
    /// - `clear_agent_notification()` — shared clear logic
    /// - `_on_pty_input()` — called from Rust hot path, fires plugin hook
    /// - `_clear_agent_notification()` — called from command handler, no hook
    fn setup_notification_env(runtime: &LuaRuntime) {
        runtime.lua().load(r#"
            -- Minimal Agent mock with notification support
            _test_agents = {}
            Agent = {
                list = function()
                    local result = {}
                    for _, a in ipairs(_test_agents) do
                        table.insert(result, a)
                    end
                    return result
                end,
                all_info = function()
                    local result = {}
                    for _, a in ipairs(_test_agents) do
                        table.insert(result, { notification = a.notification })
                    end
                    return result
                end,
            }

            -- Agent.get by key (mirrors lib/agent.lua)
            Agent.get = function(key)
                for _, a in ipairs(_test_agents) do
                    if a._agent_key == key then return a end
                end
                return nil
            end

            -- Track hook notifications for plugin test
            _pty_input_hook_calls = {}
            _pty_notification_calls = {}
            hooks = {
                notify = function(event_name, data)
                    if event_name == "pty_input" then
                        table.insert(_pty_input_hook_calls, data)
                    elseif event_name == "pty_notification" then
                        table.insert(_pty_notification_calls, data)
                    end
                end,
            }

            -- Track broadcast calls
            _broadcasts = {}
            function broadcast_hub_event(event_type, data)
                table.insert(_broadcasts, { type = event_type, data = data })
            end

            -- Shared clear logic (mirrors connections.lua)
            local function clear_agent_notification(agent_index)
                local agents = Agent.list()
                local agent = agents[agent_index + 1]
                local cleared = false
                if agent and agent.notification then
                    agent.notification = false
                    broadcast_hub_event("agent_list", { agents = Agent.all_info() })
                    cleared = true
                end
                local any_remaining = false
                for _, a in ipairs(agents) do
                    if a.notification then any_remaining = true; break end
                end
                return cleared, any_remaining, agent
            end

            -- Called from Rust PTY input hot path
            function _on_pty_input(agent_index)
                local cleared, any_remaining, agent = clear_agent_notification(agent_index)
                if cleared and agent then
                    hooks.notify("pty_input", {
                        agent_index = agent_index,
                        agent_key = agent._agent_key,
                    })
                end
                return any_remaining
            end

            -- Called from clear_notification command (TUI agent switch)
            function _clear_agent_notification(agent_index)
                local _, any_remaining = clear_agent_notification(agent_index)
                return any_remaining
            end
        "#).exec().unwrap();
    }

    /// Add a test agent to the Lua mock.
    fn add_test_agent(runtime: &LuaRuntime, key: &str, notification: bool) {
        runtime.lua().load(&format!(
            r#"table.insert(_test_agents, {{ _agent_key = "{key}", notification = {notif} }})"#,
            key = key,
            notif = notification,
        )).exec().unwrap();
    }

    #[test]
    fn test_pty_input_listening_starts_false() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        assert!(!runtime.pty_input_listening, "Should start disarmed");
    }

    #[test]
    fn test_notify_pty_notification_arms_listener() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);

        // Set up minimal hooks for notify_pty_notification
        runtime.lua().load(r#"
            hooks.on = function() end
            hooks.off = function() end
        "#).exec().unwrap();

        assert!(!runtime.pty_input_listening);

        runtime.notify_pty_notification(
            "test-agent",
            "agent",
            &crate::agent::AgentNotification::Osc9(Some("bell".to_string())),
        );

        assert!(runtime.pty_input_listening, "Should be armed after notification");
    }

    #[test]
    fn test_notify_pty_input_noop_when_disarmed() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);

        // Don't arm — pty_input_listening is false
        assert!(!runtime.pty_input_listening);

        // This should be a pure bool check, no Lua call
        runtime.notify_pty_input(0);

        // Verify _on_pty_input was never called (no broadcasts)
        let broadcast_count: i64 = runtime.lua().load("return #_broadcasts")
            .eval().unwrap();
        assert_eq!(broadcast_count, 0, "Should not call Lua when disarmed");
    }

    #[test]
    fn test_notify_pty_input_clears_notification_and_disarms() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);
        add_test_agent(&runtime, "agent-0", true);  // has notification

        // Arm the listener
        runtime.pty_input_listening = true;

        // First keystroke should clear the notification
        runtime.notify_pty_input(0);

        // Notification should be cleared
        let notif: bool = runtime.lua().load(
            "return _test_agents[1].notification"
        ).eval().unwrap();
        assert!(!notif, "Notification should be cleared after input");

        // Should have broadcast the updated agent list
        let broadcast_count: i64 = runtime.lua().load("return #_broadcasts")
            .eval().unwrap();
        assert_eq!(broadcast_count, 1, "Should broadcast agent_list update");

        // Listener should be disarmed (no more notifications pending)
        assert!(
            !runtime.pty_input_listening,
            "Should disarm when no notifications remain"
        );
    }

    #[test]
    fn test_notify_pty_input_stays_armed_with_multiple_notifications() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);
        add_test_agent(&runtime, "agent-0", true);  // has notification
        add_test_agent(&runtime, "agent-1", true);  // also has notification

        runtime.pty_input_listening = true;

        // Keystroke on agent 0 — clears its notification
        runtime.notify_pty_input(0);

        // Agent 0 cleared, agent 1 still has notification
        let notif0: bool = runtime.lua().load(
            "return _test_agents[1].notification"
        ).eval().unwrap();
        let notif1: bool = runtime.lua().load(
            "return _test_agents[2].notification"
        ).eval().unwrap();
        assert!(!notif0, "Agent 0 notification should be cleared");
        assert!(notif1, "Agent 1 notification should remain");

        // Listener should stay armed
        assert!(
            runtime.pty_input_listening,
            "Should stay armed when other agents have notifications"
        );

        // Keystroke on agent 1 — clears its notification
        runtime.notify_pty_input(1);

        let notif1_after: bool = runtime.lua().load(
            "return _test_agents[2].notification"
        ).eval().unwrap();
        assert!(!notif1_after, "Agent 1 notification should be cleared");

        // Now disarmed — no notifications remain
        assert!(
            !runtime.pty_input_listening,
            "Should disarm when all notifications cleared"
        );
    }

    #[test]
    fn test_notify_pty_input_noop_for_agent_without_notification() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);
        add_test_agent(&runtime, "agent-0", false);  // no notification
        add_test_agent(&runtime, "agent-1", true);   // has notification

        runtime.pty_input_listening = true;

        // Keystroke on agent 0 (no notification) — should not broadcast
        runtime.notify_pty_input(0);

        let broadcast_count: i64 = runtime.lua().load("return #_broadcasts")
            .eval().unwrap();
        assert_eq!(broadcast_count, 0, "Should not broadcast when agent has no notification");

        // But should stay armed because agent 1 still has one
        assert!(runtime.pty_input_listening, "Should stay armed");
    }

    #[test]
    fn test_notify_pty_input_fires_hook_for_plugins() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);
        add_test_agent(&runtime, "my-agent", true);

        runtime.pty_input_listening = true;
        runtime.notify_pty_input(0);

        // Should have fired hooks.notify("pty_input", ...) for plugins
        let hook_call_count: i64 = runtime.lua().load(
            "return #_pty_input_hook_calls"
        ).eval().unwrap();
        assert_eq!(hook_call_count, 1, "Should fire pty_input hook for plugins");

        let hook_agent_key: String = runtime.lua().load(
            "return _pty_input_hook_calls[1].agent_key"
        ).eval().unwrap();
        assert_eq!(hook_agent_key, "my-agent");
    }

    #[test]
    fn test_notify_pty_input_no_hook_when_no_notification_cleared() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);
        add_test_agent(&runtime, "agent-0", false);  // no notification
        add_test_agent(&runtime, "agent-1", true);   // keeps us armed

        runtime.pty_input_listening = true;

        // Keystroke on agent 0 (no notification to clear)
        runtime.notify_pty_input(0);

        let hook_call_count: i64 = runtime.lua().load(
            "return #_pty_input_hook_calls"
        ).eval().unwrap();
        assert_eq!(hook_call_count, 0, "Should not fire hook when no notification cleared");
    }

    #[test]
    fn test_full_lifecycle_arm_clear_rearm() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);
        add_test_agent(&runtime, "agent-0", false);

        // Initially disarmed
        assert!(!runtime.pty_input_listening);
        runtime.notify_pty_input(0);  // no-op

        // Simulate notification arriving (set flag + agent state)
        runtime.lua().load("_test_agents[1].notification = true").exec().unwrap();
        runtime.pty_input_listening = true;

        // Keystroke clears it
        runtime.notify_pty_input(0);
        assert!(!runtime.pty_input_listening, "Disarmed after clear");

        // Another keystroke — should be pure bool check, no Lua
        let broadcasts_before: i64 = runtime.lua().load("return #_broadcasts")
            .eval().unwrap();
        runtime.notify_pty_input(0);
        let broadcasts_after: i64 = runtime.lua().load("return #_broadcasts")
            .eval().unwrap();
        assert_eq!(broadcasts_before, broadcasts_after, "No Lua call when disarmed");

        // Second notification arrives
        runtime.lua().load("_test_agents[1].notification = true").exec().unwrap();
        runtime.pty_input_listening = true;

        // Keystroke clears again
        runtime.notify_pty_input(0);
        assert!(!runtime.pty_input_listening, "Disarmed after second clear");

        // Total: 2 broadcasts (one per notification clear)
        let total_broadcasts: i64 = runtime.lua().load("return #_broadcasts")
            .eval().unwrap();
        assert_eq!(total_broadcasts, 2);
    }

    #[test]
    fn test_clear_agent_notification_command_path_no_hook() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);
        add_test_agent(&runtime, "agent-0", true);

        // Simulate the clear_notification command path (TUI agent switch)
        let remaining: bool = runtime.lua().load(
            "return _clear_agent_notification(0)"
        ).eval().unwrap();

        // Notification should be cleared
        let notif: bool = runtime.lua().load(
            "return _test_agents[1].notification"
        ).eval().unwrap();
        assert!(!notif, "Notification should be cleared");

        // Should have broadcast the update
        let broadcast_count: i64 = runtime.lua().load("return #_broadcasts")
            .eval().unwrap();
        assert_eq!(broadcast_count, 1, "Should broadcast agent_list update");

        // But should NOT fire the pty_input hook (this wasn't typing)
        let hook_call_count: i64 = runtime.lua().load(
            "return #_pty_input_hook_calls"
        ).eval().unwrap();
        assert_eq!(hook_call_count, 0, "Command path should not fire pty_input hook");

        // No more notifications remaining
        assert!(!remaining);
    }

    #[test]
    fn test_command_clear_and_pty_input_clear_both_broadcast() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);
        add_test_agent(&runtime, "agent-0", true);
        add_test_agent(&runtime, "agent-1", true);

        // Clear agent 0 via command path (TUI agent switch)
        runtime.lua().load("_clear_agent_notification(0)").exec().unwrap();

        // Clear agent 1 via PTY input path (typing)
        runtime.pty_input_listening = true;
        runtime.notify_pty_input(1);

        // Both should have broadcast
        let broadcast_count: i64 = runtime.lua().load("return #_broadcasts")
            .eval().unwrap();
        assert_eq!(broadcast_count, 2, "Both paths should broadcast");

        // Only the PTY input path should fire the hook
        let hook_call_count: i64 = runtime.lua().load(
            "return #_pty_input_hook_calls"
        ).eval().unwrap();
        assert_eq!(hook_call_count, 1, "Only PTY input should fire hook");

        // Listener should be disarmed (all cleared)
        assert!(!runtime.pty_input_listening);
    }

    // =========================================================================
    // PTY Notification Enrichment Tests
    // =========================================================================

    /// Set up the enrichment bridge that mirrors production `connections.lua`.
    ///
    /// Registers the `_pty_notification_raw` → `pty_notification` observer
    /// so tests can verify `already_notified` enrichment end-to-end.
    fn setup_enrichment_bridge(runtime: &LuaRuntime) {
        runtime.lua().load(r#"
            -- Register the enrichment bridge (mirrors connections.lua)
            hooks.on = function(event, name, callback)
                if event == "_pty_notification_raw" and name == "enrich_and_dispatch" then
                    -- Wire the bridge into hooks.notify so Rust's
                    -- hooks.notify("_pty_notification_raw", data) triggers it
                    local base = hooks.notify
                    hooks.notify = function(ev, ...)
                        if ev == "_pty_notification_raw" then
                            callback(...)
                        end
                        return base(ev, ...)
                    end
                end
            end
            hooks.off = function() end

            -- Register the bridge
            hooks.on("_pty_notification_raw", "enrich_and_dispatch", function(info)
                local agent = info.agent_key and Agent.get(info.agent_key)
                info.already_notified = agent and agent.notification or false
                hooks.notify("pty_notification", info)
            end)
        "#).exec().unwrap();
    }

    #[test]
    fn test_enrichment_sets_already_notified_false_when_no_prior() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);
        setup_enrichment_bridge(&runtime);
        add_test_agent(&runtime, "agent-0", false); // no prior notification

        runtime.notify_pty_notification(
            "agent-0",
            "agent",
            &crate::agent::AgentNotification::Osc9(Some("bell".to_string())),
        );

        let already: bool = runtime.lua().load(
            "return _pty_notification_calls[1].already_notified"
        ).eval().unwrap();
        assert!(!already, "Should be false when agent has no prior notification");
    }

    #[test]
    fn test_enrichment_sets_already_notified_true_when_pending() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);
        setup_enrichment_bridge(&runtime);
        add_test_agent(&runtime, "agent-0", true); // already has notification

        runtime.notify_pty_notification(
            "agent-0",
            "agent",
            &crate::agent::AgentNotification::Osc9(Some("bell".to_string())),
        );

        let already: bool = runtime.lua().load(
            "return _pty_notification_calls[1].already_notified"
        ).eval().unwrap();
        assert!(already, "Should be true when agent already has a pending notification");
    }

    #[test]
    fn test_enrichment_preserves_original_fields() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        setup_notification_env(&runtime);
        setup_enrichment_bridge(&runtime);
        add_test_agent(&runtime, "agent-0", false);

        runtime.notify_pty_notification(
            "agent-0",
            "cli",
            &crate::agent::AgentNotification::Osc777 {
                title: "Build Done".to_string(),
                body: "All tests passed".to_string(),
            },
        );

        let agent_key: String = runtime.lua().load(
            "return _pty_notification_calls[1].agent_key"
        ).eval().unwrap();
        let session: String = runtime.lua().load(
            "return _pty_notification_calls[1].session_name"
        ).eval().unwrap();
        let ntype: String = runtime.lua().load(
            "return _pty_notification_calls[1].type"
        ).eval().unwrap();
        let title: String = runtime.lua().load(
            "return _pty_notification_calls[1].title"
        ).eval().unwrap();
        let body: String = runtime.lua().load(
            "return _pty_notification_calls[1].body"
        ).eval().unwrap();

        assert_eq!(agent_key, "agent-0");
        assert_eq!(session, "cli");
        assert_eq!(ntype, "osc777");
        assert_eq!(title, "Build Done");
        assert_eq!(body, "All tests passed");
    }
}
