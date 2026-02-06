//! Lua runtime management.
//!
//! Provides the `LuaRuntime` struct which owns and manages the Lua interpreter
//! state. Handles script loading, function invocation, and error handling based
//! on environment configuration.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use mlua::{IntoLuaMulti, Lua, LuaSerdeExt};

use crate::hub::handle_cache::HandleCache;

use super::file_watcher::LuaFileWatcher;
use super::primitives;
use super::primitives::events::SharedEventCallbacks;
use super::primitives::connection::{ConnectionRequest, ConnectionRequestQueue};
use super::primitives::hub::{HubRequest, HubRequestQueue};
use super::primitives::worktree::{WorktreeRequest, WorktreeRequestQueue};
use super::primitives::pty::{PtyOutputContext, PtyRequest, PtyRequestQueue};
use super::primitives::tui::{
    registry_keys as tui_registry_keys, TuiSendQueue, TuiSendRequest,
};
use super::primitives::webrtc::{registry_keys, WebRtcSendQueue, WebRtcSendRequest};

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
/// Call `start_file_watching()` to enable hot-reload, then call
/// `poll_and_reload()` periodically in the event loop.
///
/// # Example
///
/// ```ignore
/// let mut lua = LuaRuntime::new()?;
/// lua.load_file(Path::new("init.lua"))?;
/// lua.start_file_watching()?;
///
/// // In event loop:
/// lua.poll_and_reload();
/// lua.call_function("on_startup", ())?;
/// ```
pub struct LuaRuntime {
    /// The Lua interpreter state.
    lua: Lua,
    /// Base path for loading Lua scripts.
    base_path: PathBuf,
    /// Whether to panic on Lua errors (strict mode).
    strict: bool,
    /// Optional file watcher for hot-reload support.
    file_watcher: Option<LuaFileWatcher>,
    /// Queue for outgoing WebRTC messages from Lua callbacks.
    webrtc_send_queue: WebRtcSendQueue,
    /// Queue for outgoing TUI messages from Lua callbacks.
    tui_send_queue: TuiSendQueue,
    /// Queue for PTY operations from Lua callbacks.
    pty_request_queue: PtyRequestQueue,
    /// Queue for Hub operations from Lua callbacks.
    hub_request_queue: HubRequestQueue,
    /// Queue for connection operations from Lua callbacks.
    connection_request_queue: ConnectionRequestQueue,
    /// Queue for worktree operations from Lua callbacks.
    worktree_request_queue: WorktreeRequestQueue,
    /// Event callbacks registered by Lua scripts.
    event_callbacks: SharedEventCallbacks,
}

impl std::fmt::Debug for LuaRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let webrtc_queue_len = self.webrtc_send_queue.lock().map(|q| q.len()).unwrap_or(0);
        let tui_queue_len = self.tui_send_queue.lock().map(|q| q.len()).unwrap_or(0);
        let pty_queue_len = self.pty_request_queue.lock().map(|q| q.len()).unwrap_or(0);
        let hub_queue_len = self.hub_request_queue.lock().map(|q| q.len()).unwrap_or(0);
        let conn_queue_len = self.connection_request_queue.lock().map(|q| q.len()).unwrap_or(0);
        let wt_queue_len = self.worktree_request_queue.lock().map(|q| q.len()).unwrap_or(0);
        let event_cb_count = self.event_callbacks.lock().map(|c| c.callback_count()).unwrap_or(0);
        f.debug_struct("LuaRuntime")
            .field("base_path", &self.base_path)
            .field("strict", &self.strict)
            .field("file_watching", &self.file_watcher.is_some())
            .field("webrtc_queue_len", &webrtc_queue_len)
            .field("tui_queue_len", &tui_queue_len)
            .field("pty_queue_len", &pty_queue_len)
            .field("hub_queue_len", &hub_queue_len)
            .field("connection_queue_len", &conn_queue_len)
            .field("worktree_queue_len", &wt_queue_len)
            .field("event_callback_count", &event_cb_count)
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

        // Create WebRTC send queue
        let webrtc_send_queue = primitives::new_send_queue();

        // Create TUI send queue
        let tui_send_queue = primitives::new_tui_queue();

        // Create PTY request queue
        let pty_request_queue = primitives::new_pty_queue();

        // Create Hub request queue
        let hub_request_queue = primitives::new_hub_queue();

        // Create connection request queue
        let connection_request_queue = primitives::new_connection_queue();

        // Create worktree request queue
        let worktree_request_queue = primitives::new_worktree_queue();

        // Create event callback storage
        let event_callbacks = primitives::new_event_callbacks();

        // Register all primitives
        primitives::register_all(&lua).context("Failed to register Lua primitives")?;

        // Register WebRTC primitives with the send queue
        primitives::register_webrtc(&lua, Arc::clone(&webrtc_send_queue))
            .context("Failed to register WebRTC primitives")?;

        // Register TUI primitives with the send queue
        primitives::register_tui(&lua, Arc::clone(&tui_send_queue))
            .context("Failed to register TUI primitives")?;

        // Register PTY primitives with the request queue
        primitives::register_pty(&lua, Arc::clone(&pty_request_queue))
            .context("Failed to register PTY primitives")?;

        // Register event primitives with the callback storage
        primitives::register_events(&lua, Arc::clone(&event_callbacks))
            .context("Failed to register event primitives")?;

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
            file_watcher: None,
            webrtc_send_queue,
            tui_send_queue,
            pty_request_queue,
            hub_request_queue,
            connection_request_queue,
            worktree_request_queue,
            event_callbacks,
        })
    }

    /// Configure Lua package.path to include the base path and subdirectories.
    ///
    /// This allows:
    /// - `require("core.hooks")` to find `{base_path}/core/hooks.lua`
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
        // - {base}/core/?.lua - core modules (state, hooks, loader)
        let new_path = format!(
            "{path}/?.lua;{path}/?/init.lua;{path}/lib/?.lua;{path}/handlers/?.lua;{path}/core/?.lua;{current}",
            path = base_path.display(),
            current = current_path
        );

        package
            .set("path", new_path)
            .map_err(|e| anyhow!("Failed to set package.path: {e}"))?;

        Ok(())
    }

    /// Update Lua package.path to include an additional directory (for embedded fallback).
    ///
    /// This is used when loading embedded Lua files from a different directory than
    /// the configured base path. It appends the new directory to the existing package.path
    /// so that require() calls can find modules in the embedded location.
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

        // Prepend the additional path to package.path so embedded modules are found first
        let new_path = format!(
            "{path}/?.lua;{path}/?/init.lua;{path}/lib/?.lua;{path}/handlers/?.lua;{path}/core/?.lua;{current}",
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
    /// * `relative_path` - Path relative to base path (e.g., `core/init.lua`)
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
    /// * `name` - Name for error messages (e.g., "core/init.lua")
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

    /// Load all embedded Lua files.
    ///
    /// Iterates through all files embedded at compile time and loads them
    /// in dependency order (core modules first, then lib, then handlers).
    ///
    /// # Errors
    ///
    /// Returns an error if any embedded file fails to load.
    pub fn load_embedded(&self) -> Result<()> {
        use super::embedded;

        // Get all embedded files
        let files = embedded::all();
        if files.is_empty() {
            log::warn!("No embedded Lua files found");
            return Ok(());
        }

        log::info!("Loading {} embedded Lua file(s)", files.len());

        // Sort files by load order: core/ first, then lib/, then handlers/
        let mut sorted: Vec<_> = files.iter().collect();
        sorted.sort_by(|(a, _), (b, _)| {
            let order = |p: &str| -> u8 {
                if p.starts_with("core/") {
                    0
                } else if p.starts_with("lib/") {
                    1
                } else if p.starts_with("handlers/") {
                    2
                } else {
                    3
                }
            };
            order(a).cmp(&order(b)).then_with(|| a.cmp(b))
        });

        // Load core/init.lua first - it bootstraps everything else
        if let Some(init_content) = embedded::get("core/init.lua") {
            self.load_string("core/init.lua", init_content)?;
        } else {
            return Err(anyhow!("Missing embedded core/init.lua"));
        }

        log::info!("Embedded Lua loaded successfully");
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
    // Hot-Reload Support
    // =========================================================================

    /// Start watching the Lua script directory for changes.
    ///
    /// After calling this, use `poll_and_reload()` in the event loop to
    /// check for changes and reload modified modules.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The base path directory does not exist
    /// - File watcher creation fails
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut lua = LuaRuntime::new()?;
    /// lua.load_file(Path::new("core/init.lua"))?;
    /// lua.start_file_watching()?;
    /// ```
    pub fn start_file_watching(&mut self) -> Result<()> {
        if self.file_watcher.is_some() {
            log::warn!("File watching already started");
            return Ok(());
        }

        // Only start watching if the directory exists
        if !self.base_path.exists() {
            log::debug!(
                "Lua base path does not exist, skipping file watch: {:?}",
                self.base_path
            );
            return Ok(());
        }

        let mut watcher = LuaFileWatcher::new(self.base_path.clone())?;
        watcher.start_watching()?;
        self.file_watcher = Some(watcher);

        Ok(())
    }

    /// Stop watching the Lua script directory.
    pub fn stop_file_watching(&mut self) {
        if let Some(mut watcher) = self.file_watcher.take() {
            watcher.stop_watching();
            log::info!("Stopped Lua file watching");
        }
    }

    /// Check if file watching is enabled.
    #[must_use]
    pub fn is_file_watching(&self) -> bool {
        self.file_watcher.is_some()
    }

    /// Poll for file changes and reload modified modules.
    ///
    /// Call this periodically in the event loop. Does nothing if file
    /// watching is not enabled.
    ///
    /// Uses the Lua `loader.reload()` function to reload modules, which
    /// respects the protected module list (core.state, core.hooks, etc.)
    /// and calls `_before_reload` / `_after_reload` lifecycle hooks.
    ///
    /// # Returns
    ///
    /// The number of modules that were reloaded.
    pub fn poll_and_reload(&self) -> usize {
        let Some(ref watcher) = self.file_watcher else {
            return 0;
        };

        let changes = watcher.poll_changes();
        if changes.is_empty() {
            return 0;
        }

        log::debug!("Detected {} Lua file change(s)", changes.len());

        let mut reloaded = 0;
        for module_name in changes {
            if self.reload_module(&module_name) {
                reloaded += 1;
            }
        }

        reloaded
    }

    /// Reload a single Lua module via the loader.
    ///
    /// Returns `true` if the reload succeeded.
    fn reload_module(&self, module_name: &str) -> bool {
        // Call loader.reload(module_name) using safe function call mechanism
        let result: mlua::Result<bool> = (|| {
            let loader: mlua::Table = self.lua.globals().get("loader")?;
            let reload: mlua::Function = loader.get("reload")?;
            reload.call::<bool>(module_name)
        })();

        match result {
            Ok(success) => success,
            Err(e) => {
                log::error!("Failed to reload module '{}': {}", module_name, e);
                false
            }
        }
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

            // Convert JSON to Lua value using mlua's serialize feature
            let lua_value = self.lua.to_value(&message)
                .map_err(|e| anyhow!("Failed to convert JSON to Lua value: {e}"))?;

            callback.call::<()>((peer_id, lua_value))
                .map_err(|e| anyhow!("webrtc_message callback failed: {e}"))?;
        }

        Ok(())
    }

    /// Drain pending WebRTC send requests.
    ///
    /// Returns all messages queued by Lua's `webrtc.send()` and `webrtc.send_binary()`
    /// calls since the last drain. The queue is cleared after this call.
    ///
    /// Hub should call this after invoking Lua callbacks to process any
    /// outgoing messages.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // After calling Lua callback
    /// for request in lua.drain_webrtc_sends() {
    ///     match request {
    ///         WebRtcSendRequest::Json { peer_id, data } => {
    ///             hub.send_webrtc_message(&peer_id, &data);
    ///         }
    ///         WebRtcSendRequest::Binary { peer_id, data } => {
    ///             hub.send_webrtc_raw(&peer_id, &data);
    ///         }
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn drain_webrtc_sends(&self) -> Vec<WebRtcSendRequest> {
        let mut queue = self.webrtc_send_queue.lock()
            .expect("WebRTC send queue mutex poisoned");
        std::mem::take(&mut *queue)
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

            let lua_value = self
                .lua
                .to_value(&message)
                .map_err(|e| anyhow!("Failed to convert JSON to Lua value: {e}"))?;

            callback
                .call::<()>(lua_value)
                .map_err(|e| anyhow!("tui_message callback failed: {e}"))?;
        }

        Ok(())
    }

    /// Drain pending TUI send requests.
    ///
    /// Returns all messages queued by Lua's `tui.send()` and `tui.send_binary()`
    /// calls since the last drain. The queue is cleared after this call.
    ///
    /// Hub should call this after invoking Lua callbacks to process any
    /// outgoing TUI messages.
    #[must_use]
    pub fn drain_tui_sends(&self) -> Vec<TuiSendRequest> {
        let mut queue = self
            .tui_send_queue
            .lock()
            .expect("TUI send queue mutex poisoned");
        std::mem::take(&mut *queue)
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
    // PTY Operations
    // =========================================================================

    /// Drain pending PTY requests.
    ///
    /// Returns all PTY operations queued by Lua's `webrtc.create_pty_forwarder()`,
    /// `hub.write_pty()`, `hub.resize_pty()`, etc. since the last drain.
    /// The queue is cleared after this call.
    ///
    /// Hub should call this after invoking Lua callbacks to process any
    /// PTY operations.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // After calling Lua callback
    /// for request in lua.drain_pty_requests() {
    ///     match request {
    ///         PtyRequest::CreateForwarder(req) => {
    ///             hub.create_lua_pty_forwarder(req);
    ///         }
    ///         PtyRequest::WritePty { agent_index, pty_index, data } => {
    ///             hub.write_to_pty(agent_index, pty_index, &data);
    ///         }
    ///         // ...
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn drain_pty_requests(&self) -> Vec<PtyRequest> {
        let mut queue = self.pty_request_queue.lock()
            .expect("PTY request queue mutex poisoned");
        std::mem::take(&mut *queue)
    }

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
        &self,
        ctx: &PtyOutputContext,
        data: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        // Create context table for Lua
        let ctx_table = self.lua.create_table()
            .map_err(|e| anyhow!("Failed to create context table: {e}"))?;

        ctx_table.set("agent_index", ctx.agent_index)
            .map_err(|e| anyhow!("Failed to set agent_index: {e}"))?;
        ctx_table.set("pty_index", ctx.pty_index)
            .map_err(|e| anyhow!("Failed to set pty_index: {e}"))?;
        ctx_table.set("peer_id", ctx.peer_id.clone())
            .map_err(|e| anyhow!("Failed to set peer_id: {e}"))?;

        // Convert data to Lua string (binary-safe)
        let data_str = self.lua.create_string(data)
            .map_err(|e| anyhow!("Failed to create data string: {e}"))?;

        // Call hooks.call("pty_output", ctx, data)
        let func: mlua::Function = self.lua.load(
            r#"
            return function(ctx, data)
                return hooks.call("pty_output", ctx, data)
            end
            "#
        ).eval()
            .map_err(|e| anyhow!("Failed to create PTY hook wrapper: {e}"))?;

        let result: mlua::Result<Option<mlua::String>> = func.call((ctx_table, data_str));

        match result {
            Ok(Some(transformed)) => {
                Ok(Some(transformed.as_bytes().to_vec()))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(anyhow!("PTY output hook error: {e}")),
        }
    }

    /// Store scrollback response in Lua registry for retrieval.
    ///
    /// Called by Hub when a GetScrollback request completes. The response
    /// is stored in the registry keyed by the response_key returned from
    /// `hub.get_scrollback()`.
    ///
    /// # Arguments
    ///
    /// * `response_key` - The key returned by hub.get_scrollback()
    /// * `scrollback` - The scrollback buffer data
    pub fn set_scrollback_response(&self, response_key: &str, scrollback: Vec<u8>) {
        // Store in a global table for Lua to retrieve
        if let Ok(responses) = self.get_or_create_scrollback_responses() {
            let data = match self.lua.create_string(&scrollback) {
                Ok(s) => s,
                Err(e) => {
                    log::error!("Failed to create scrollback string: {e}");
                    return;
                }
            };
            if let Err(e) = responses.set(response_key, data) {
                log::error!("Failed to store scrollback response: {e}");
            }
        }
    }

    /// Get or create the _scrollback_responses table.
    fn get_or_create_scrollback_responses(&self) -> mlua::Result<mlua::Table> {
        let globals = self.lua.globals();
        match globals.get::<mlua::Table>("_scrollback_responses") {
            Ok(t) => Ok(t),
            Err(_) => {
                let t = self.lua.create_table()?;
                globals.set("_scrollback_responses", t.clone())?;
                Ok(t)
            }
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
    ///
    /// # Errors
    ///
    /// Returns an error if registration fails.
    pub fn register_hub_primitives(
        &self,
        handle_cache: Arc<HandleCache>,
        worktree_base: PathBuf,
    ) -> Result<()> {
        primitives::register_hub(
            &self.lua,
            Arc::clone(&self.hub_request_queue),
            Arc::clone(&handle_cache),
        )
        .context("Failed to register Hub primitives")?;

        primitives::register_connection(
            &self.lua,
            Arc::clone(&self.connection_request_queue),
            Arc::clone(&handle_cache),
        )
        .context("Failed to register connection primitives")?;

        primitives::register_worktree(
            &self.lua,
            Arc::clone(&self.worktree_request_queue),
            handle_cache,
            worktree_base,
        )
        .context("Failed to register worktree primitives")?;

        Ok(())
    }

    /// Drain pending Hub requests.
    ///
    /// Returns all Hub operations queued by Lua's `hub.create_agent()` and
    /// `hub.delete_agent()` calls since the last drain. The queue is cleared
    /// after this call.
    ///
    /// Hub should call this after invoking Lua callbacks to process any
    /// agent lifecycle operations.
    ///
    /// # Example
    ///
    /// ```ignore
    /// for request in lua.drain_hub_requests() {
    ///     match request {
    ///         HubRequest::Quit => {
    ///             hub.quit = true;
    ///         }
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn drain_hub_requests(&self) -> Vec<HubRequest> {
        let mut queue = self.hub_request_queue.lock()
            .expect("Hub request queue mutex poisoned");
        std::mem::take(&mut *queue)
    }

    /// Drain pending connection requests.
    ///
    /// Returns all connection operations queued by Lua's `connection.regenerate()`
    /// calls since the last drain. The queue is cleared after this call.
    ///
    /// Hub should call this after invoking Lua callbacks to process any
    /// connection operations.
    #[must_use]
    pub fn drain_connection_requests(&self) -> Vec<ConnectionRequest> {
        let mut queue = self
            .connection_request_queue
            .lock()
            .expect("Connection request queue mutex poisoned");
        std::mem::take(&mut *queue)
    }

    /// Drain pending worktree requests.
    ///
    /// Returns all worktree operations queued by Lua's `worktree.create_async()`
    /// and `worktree.delete()` calls since the last drain. The queue is cleared
    /// after this call.
    ///
    /// Hub should call this after invoking Lua callbacks to process any
    /// worktree operations.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // After calling Lua callback
    /// for request in lua.drain_worktree_requests() {
    ///     match request {
    ///         WorktreeRequest::CreateAsync { branch, response_key } => {
    ///             hub.create_worktree_async(branch, response_key);
    ///         }
    ///         WorktreeRequest::Delete { path, branch } => {
    ///             hub.delete_worktree(&path, &branch);
    ///         }
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn drain_worktree_requests(&self) -> Vec<WorktreeRequest> {
        let mut queue = self
            .worktree_request_queue
            .lock()
            .expect("Worktree request queue mutex poisoned");
        std::mem::take(&mut *queue)
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

    /// Fire the "command_message" event with the full message payload.
    ///
    /// Called by Hub when a command channel message should be handled by Lua.
    /// Lua handlers (e.g., `handlers/agents.lua`) listen for this event and
    /// route `create_agent`, `delete_agent`, etc. to their respective handlers.
    ///
    /// # Arguments
    ///
    /// * `message` - The message payload as a JSON value. Lua receives it as
    ///   a table with fields like `type`, `issue_or_branch`, `prompt`, etc.
    pub fn fire_command_message(&self, message: &serde_json::Value) -> Result<()> {
        if !self.has_event_callbacks("command_message") {
            return Ok(());
        }

        let message = message.clone();

        self.fire_event("command_message", |lua| {
            let lua_value = lua.to_value(&message).map_err(|e| anyhow!("to_value: {e}"))?;
            Ok(lua_value)
        })
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

    #[test]
    fn test_runtime_creation() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        assert!(!runtime.strict);
        assert!(!runtime.is_file_watching());
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

    #[test]
    fn test_file_watching_on_nonexistent_dir() {
        let mut runtime = LuaRuntime::new().expect("Should create runtime");
        // Override base path to nonexistent directory
        runtime.base_path = PathBuf::from("/nonexistent/lua/path");

        // Should succeed but not start watching
        runtime.start_file_watching().expect("Should not error");
        assert!(!runtime.is_file_watching());
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
    fn test_drain_webrtc_sends_returns_queued_messages() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            webrtc.send("peer-1", { type = "hello" })
            webrtc.send("peer-2", { type = "world" })
        "#).exec().unwrap();

        let sends = runtime.drain_webrtc_sends();
        assert_eq!(sends.len(), 2);

        // Queue should be empty after drain
        let sends2 = runtime.drain_webrtc_sends();
        assert!(sends2.is_empty());
    }

    #[test]
    fn test_callback_can_send_response() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            webrtc.on_message(function(peer_id, msg)
                if msg.type == "ping" then
                    webrtc.send(peer_id, { type = "pong" })
                end
            end)
        "#).exec().unwrap();

        let ping = serde_json::json!({ "type": "ping" });
        runtime.call_webrtc_message("peer-echo", ping).expect("Should call callback");

        let sends = runtime.drain_webrtc_sends();
        assert_eq!(sends.len(), 1);

        match &sends[0] {
            WebRtcSendRequest::Json { peer_id, data } => {
                assert_eq!(peer_id, "peer-echo");
                assert_eq!(data["type"], "pong");
            }
            _ => panic!("Expected Json request"),
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
    fn test_drain_pty_requests_empty_initially() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let requests = runtime.drain_pty_requests();
        assert!(requests.is_empty());
    }

    #[test]
    fn test_pty_write_queues_request() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            hub.write_pty(0, 0, "hello")
        "#).exec().unwrap();

        let requests = runtime.drain_pty_requests();
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            PtyRequest::WritePty { agent_index, pty_index, data } => {
                assert_eq!(*agent_index, 0);
                assert_eq!(*pty_index, 0);
                assert_eq!(data, b"hello");
            }
            _ => panic!("Expected WritePty request"),
        }
    }

    #[test]
    fn test_pty_resize_queues_request() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            hub.resize_pty(1, 0, 50, 100)
        "#).exec().unwrap();

        let requests = runtime.drain_pty_requests();
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            PtyRequest::ResizePty { agent_index, pty_index, rows, cols } => {
                assert_eq!(*agent_index, 1);
                assert_eq!(*pty_index, 0);
                assert_eq!(*rows, 50);
                assert_eq!(*cols, 100);
            }
            _ => panic!("Expected ResizePty request"),
        }
    }

    #[test]
    fn test_get_scrollback_queues_request_and_returns_key() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        let key: String = runtime.lua().load(r#"
            return hub.get_scrollback(0, 1)
        "#).eval().unwrap();

        assert!(key.starts_with("scrollback:0:1:"), "Key should start with expected prefix");

        let requests = runtime.drain_pty_requests();
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            PtyRequest::GetScrollback { agent_index, pty_index, response_key } => {
                assert_eq!(*agent_index, 0);
                assert_eq!(*pty_index, 1);
                assert_eq!(response_key, &key);
            }
            _ => panic!("Expected GetScrollback request"),
        }
    }

    #[test]
    fn test_create_forwarder_queues_request() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            forwarder = webrtc.create_pty_forwarder({
                peer_id = "test-browser",
                agent_index = 0,
                pty_index = 0,
                subscription_id = "sub_1_test",
            })
        "#).exec().unwrap();

        let requests = runtime.drain_pty_requests();
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            PtyRequest::CreateForwarder(req) => {
                assert_eq!(req.peer_id, "test-browser");
                assert_eq!(req.agent_index, 0);
                assert_eq!(req.pty_index, 0);
                assert_eq!(req.subscription_id, "sub_1_test");
            }
            _ => panic!("Expected CreateForwarder request"),
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
    fn test_set_scrollback_response() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        let key = "scrollback:0:0:test-uuid";
        let scrollback = b"terminal output data".to_vec();

        runtime.set_scrollback_response(key, scrollback);

        // Verify we can retrieve it from Lua
        let retrieved: mlua::String = runtime.lua().load(
            &format!(r#"return _scrollback_responses["{}"]"#, key)
        ).eval().unwrap();

        assert_eq!(retrieved.as_bytes(), b"terminal output data");
    }

    #[test]
    fn test_call_pty_output_interceptors_passthrough() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

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
        let runtime = LuaRuntime::new().expect("Should create runtime");

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
        let runtime = LuaRuntime::new().expect("Should create runtime");

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
        let runtime = LuaRuntime::new().expect("Should create runtime");

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
    fn test_drain_hub_requests_empty_initially() {
        let runtime = LuaRuntime::new().expect("Should create runtime");
        let requests = runtime.drain_hub_requests();
        assert!(requests.is_empty());
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
    fn test_drain_tui_sends_returns_queued_messages() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            tui.send({ type = "agent_list" })
            tui.send({ type = "status" })
        "#).exec().unwrap();

        let sends = runtime.drain_tui_sends();
        assert_eq!(sends.len(), 2);

        // Queue should be empty after drain
        let sends2 = runtime.drain_tui_sends();
        assert!(sends2.is_empty());
    }

    #[test]
    fn test_tui_callback_can_send_response() {
        let runtime = LuaRuntime::new().expect("Should create runtime");

        runtime.lua().load(r#"
            tui.on_message(function(msg)
                if msg.type == "list_agents" then
                    tui.send({ type = "agent_list", count = 0 })
                end
            end)
        "#).exec().unwrap();

        let msg = serde_json::json!({ "type": "list_agents" });
        runtime.call_tui_message(msg).expect("Should call callback");

        let sends = runtime.drain_tui_sends();
        assert_eq!(sends.len(), 1);

        match &sends[0] {
            TuiSendRequest::Json { data } => {
                assert_eq!(data["type"], "agent_list");
                assert_eq!(data["count"], 0);
            }
            _ => panic!("Expected Json request"),
        }
    }
}
