//! Lua source loading, bootstrapping, and hot-reload for the TUI.
//!
//! Manages the lifecycle of Lua sources from discovery through initial load
//! to runtime hot-reloading. Three main types:
//!
//! - [`LayoutSource`] / [`ExtensionSource`] — loaded Lua source + filesystem path
//! - [`LuaBootstrap`] — consumed once at startup to initialize Lua and create a [`HotReloader`]
//! - [`HotReloader`] — polls filesystem for changes, reloads Lua state in-place

// Rust guideline compliant 2026-02

use std::collections::HashSet;
use std::path::PathBuf;

use super::layout_lua::LayoutLua;

// ── Source types ──────────────────────────────────────────────────────

/// Result of loading a built-in Lua UI module.
pub(super) struct LayoutSource {
    /// The Lua source code.
    pub source: String,
    /// Filesystem path if loaded from disk (for hot-reload watching).
    /// None if loaded from embedded (no watching needed).
    pub fs_path: Option<PathBuf>,
}

/// A UI extension source loaded from a plugin or user directory.
#[derive(Debug)]
pub(super) struct ExtensionSource {
    /// Lua source code.
    pub source: String,
    /// Human-readable name for error messages (e.g., "plugin:my-plugin/layout").
    pub name: String,
    /// Filesystem path for hot-reload watching.
    pub fs_path: PathBuf,
}

// ── Source loaders ───────────────────────────────────────────────────

/// Load a Lua UI module by name.
///
/// Returns the built-in source (embedded or source tree). User overrides
/// in `~/.botster/lua/ui/` are loaded separately as extensions that layer
/// on top — redefining only the functions they want to customize.
fn load_lua_ui_source(name: &str) -> Option<LayoutSource> {
    let rel_path = format!("ui/{name}");

    // 1. Embedded (release builds).
    if let Some(source) = crate::lua::embedded::get(&rel_path) {
        log::info!("Loaded {name} from embedded");
        return Some(LayoutSource {
            source: source.to_string(),
            fs_path: None,
        });
    }

    // 2. Local source tree (debug builds where embedded is stubbed out).
    let local = PathBuf::from("lua").join(&rel_path);
    if let Ok(source) = std::fs::read_to_string(&local) {
        let fs_path = local.canonicalize().unwrap_or(local);
        log::info!("Loaded {name} from source tree: {}", fs_path.display());
        return Some(LayoutSource {
            source,
            fs_path: Some(fs_path),
        });
    }

    log::warn!("No {name} found");
    None
}

/// Discover user UI override files from `~/.botster/lua/ui/`.
///
/// These are loaded as extensions on top of the built-in UI modules,
/// so they only need to redefine the functions they want to customize.
/// For example, a user `layout.lua` containing only
/// `function render_overlay(state) ... end` overrides just the overlay
/// while `render()` stays built-in.
pub(super) fn discover_user_ui_overrides() -> Vec<ExtensionSource> {
    let mut overrides = Vec::new();
    let ui_dir = match dirs::home_dir() {
        Some(home) => home.join(format!(".{}", crate::env::APP_NAME)).join("lua").join("ui"),
        None => return overrides,
    };

    if let Ok(entries) = std::fs::read_dir(&ui_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "lua") {
                if let Ok(source) = std::fs::read_to_string(&path) {
                    let name = path
                        .file_stem()
                        .map(|s| format!("user_ui_{}", s.to_string_lossy()))
                        .unwrap_or_else(|| "user_ui".to_string());
                    log::info!("Found user UI override: {}", path.display());
                    overrides.push(ExtensionSource {
                        name,
                        source,
                        fs_path: path.canonicalize().unwrap_or(path),
                    });
                }
            }
        }
    }

    overrides
}

/// Discover UI extension files from plugins and user directories.
///
/// Returns extensions in load order:
/// 1. Plugin `ui/` files (alphabetical by plugin name)
/// 2. User `~/.botster/lua/user/ui/` files (highest priority)
pub(super) fn discover_ui_extensions(lua_base: &std::path::Path) -> Vec<ExtensionSource> {
    let mut extensions = Vec::new();
    let ui_files = ["layout.lua", "keybindings.lua", "actions.lua"];

    // Plugin UI extensions: ~/.botster/plugins/*/ui/{layout,keybindings,actions}.lua
    // lua_base is ~/.botster/lua, plugins are at ~/.botster/plugins
    let plugins_dir = lua_base.parent().unwrap_or(lua_base).join("plugins");

    if let Ok(entries) = std::fs::read_dir(&plugins_dir) {
        let mut plugin_dirs: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        plugin_dirs.sort_by_key(|e| e.file_name());

        for entry in plugin_dirs {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let plugin_name = entry.file_name().to_string_lossy().to_string();

            for ui_file in &ui_files {
                let ui_path = path.join("ui").join(ui_file);
                if let Ok(source) = std::fs::read_to_string(&ui_path) {
                    log::info!("Discovered plugin UI extension: {plugin_name}/{ui_file}");
                    extensions.push(ExtensionSource {
                        source,
                        name: format!("plugin:{plugin_name}/{ui_file}"),
                        fs_path: ui_path,
                    });
                }
            }
        }
    }

    // User UI overrides: ~/.botster/lua/user/ui/{layout,keybindings,actions}.lua
    let user_ui_dir = lua_base.join("user").join("ui");
    for ui_file in &ui_files {
        let path = user_ui_dir.join(ui_file);
        if let Ok(source) = std::fs::read_to_string(&path) {
            log::info!("Discovered user UI extension: {ui_file}");
            extensions.push(ExtensionSource {
                source,
                name: format!("user/{ui_file}"),
                fs_path: path,
            });
        }
    }

    extensions
}

/// Resolve the user-level Lua path (`~/.botster/lua/`).
///
/// Used for discovering extensions and user overrides — not for loading
/// core UI modules (which use embedded or source tree).
pub(super) fn resolve_lua_user_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(format!(".{}", crate::env::APP_NAME)).join("lua"))
        .unwrap_or_else(|| PathBuf::from(format!(".{}/lua", crate::env::APP_NAME)))
}

/// Truncate an error message to a maximum length, adding ellipsis if needed.
pub(super) fn truncate_error(msg: &str, max_len: usize) -> String {
    let trimmed = msg.lines().next().unwrap_or(msg);
    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..max_len.saturating_sub(3)])
    }
}

// ── Bootstrap ────────────────────────────────────────────────────────

/// Lua sources consumed once at TUI startup to create a [`LayoutLua`] state
/// and a [`HotReloader`].
///
/// Stored as `String` (Send) because `mlua::Lua` is !Send — the conversion
/// to `LayoutLua` happens after `thread::spawn` in [`LuaBootstrap::init`].
///
/// # Usage
///
/// ```text
/// let bootstrap = LuaBootstrap::load();  // in run_with_hub (main thread)
/// // ... thread::spawn ...
/// let (lua, reloader) = bootstrap.init(); // in run() (TUI thread)
/// ```
#[derive(Debug)]
pub struct LuaBootstrap {
    layout_source: Option<String>,
    layout_fs_path: Option<PathBuf>,
    keybinding_source: Option<String>,
    keybinding_fs_path: Option<PathBuf>,
    actions_source: Option<String>,
    actions_fs_path: Option<PathBuf>,
    events_source: Option<String>,
    events_fs_path: Option<PathBuf>,
    botster_api_source: Option<String>,
    extension_sources: Vec<ExtensionSource>,
}

impl LuaBootstrap {
    /// Discover and load all Lua sources from embedded/filesystem.
    ///
    /// This is the main entry point — replaces the 6+ setter calls in
    /// `run_with_hub`. Filesystem allows hot-reload in dev; embedded is
    /// the release fallback.
    pub fn load() -> Self {
        let layout = load_lua_ui_source("layout.lua");
        let keybinding = load_lua_ui_source("keybindings.lua");
        let actions = load_lua_ui_source("actions.lua");
        let events = load_lua_ui_source("events.lua");
        let botster_api_source = load_lua_ui_source("botster.lua").map(|s| s.source);

        let lua_base = resolve_lua_user_path();
        let mut extensions = discover_ui_extensions(&lua_base);
        extensions.extend(discover_user_ui_overrides());

        Self {
            layout_source: layout.as_ref().map(|l| l.source.clone()),
            layout_fs_path: layout.and_then(|l| l.fs_path),
            keybinding_source: keybinding.as_ref().map(|l| l.source.clone()),
            keybinding_fs_path: keybinding.and_then(|l| l.fs_path),
            actions_source: actions.as_ref().map(|l| l.source.clone()),
            actions_fs_path: actions.and_then(|l| l.fs_path),
            events_source: events.as_ref().map(|l| l.source.clone()),
            events_fs_path: events.and_then(|l| l.fs_path),
            botster_api_source,
            extension_sources: extensions,
        }
    }

    /// Initialize the Lua state and create a hot-reloader.
    ///
    /// Consumes `self` — call this once, in the TUI thread (after `thread::spawn`),
    /// because `LayoutLua` is !Send.
    ///
    /// Returns `(layout_lua, initial_mode, hot_reloader)`. The caller should set
    /// `self.mode = initial_mode` after this returns.
    pub fn init(self) -> (Option<LayoutLua>, String, HotReloader) {
        // Create LayoutLua from stored source (if any).
        let mut layout_lua = self.layout_source.and_then(|source| {
            match LayoutLua::new(&source) {
                Ok(lua) => {
                    log::info!("Lua layout engine initialized");
                    Some(lua)
                }
                Err(e) => {
                    log::warn!("Failed to initialize Lua layout engine: {e}");
                    None
                }
            }
        });

        // Load keybindings, actions, events into the same Lua state.
        if let Some(ref mut lua) = layout_lua {
            if let Some(kb_source) = &self.keybinding_source {
                match lua.load_keybindings(kb_source) {
                    Ok(()) => log::info!("Lua keybindings loaded"),
                    Err(e) => log::warn!("Failed to load Lua keybindings: {e}"),
                }
            }
            if let Some(actions_source) = &self.actions_source {
                match lua.load_actions(actions_source) {
                    Ok(()) => log::info!("Lua actions loaded"),
                    Err(e) => log::warn!("Failed to load Lua actions: {e}"),
                }
            }
            if let Some(events_source) = &self.events_source {
                match lua.load_events(events_source) {
                    Ok(()) => log::info!("Lua events loaded"),
                    Err(e) => log::warn!("Failed to load Lua events: {e}"),
                }
            }

            // Bootstrap TUI client-side state.
            let _ = lua.load_extension(
                "_tui_state = _tui_state or {\
                    agents = {},\
                    pending_fields = {},\
                    available_worktrees = {},\
                    available_profiles = {},\
                    available_session_types = {},\
                    mode = 'normal',\
                    input_buffer = '',\
                    list_selected = 0,\
                    selected_agent = nil,\
                    selected_agent_index = nil,\
                    active_pty_index = 0,\
                    connection_code = nil,\
                }",
                "_tui_state_init",
            );

            // Load botster API (provides botster.keymap, botster.action, botster.ui, etc.)
            if let Some(ref botster_source) = self.botster_api_source {
                match lua.load_extension(botster_source, "botster") {
                    Ok(()) => log::info!("Botster API loaded"),
                    Err(e) => log::warn!("Failed to load botster API: {e}"),
                }
            }

            // Load UI extensions (plugins first, then user overrides)
            for ext in &self.extension_sources {
                match lua.load_extension(&ext.source, &ext.name) {
                    Ok(()) => log::info!("Loaded UI extension: {}", ext.name),
                    Err(e) => log::warn!("Failed to load UI extension '{}': {e}", ext.name),
                }
            }

            // Wire botster action/keymap dispatch after all extensions loaded
            let _ = lua.load_extension(
                "if type(botster) == 'table' then botster._wire_actions() botster._wire_keybindings() end",
                "_wire_botster",
            );
        }

        // Let Lua declare the initial mode
        let initial_mode = layout_lua
            .as_ref()
            .map(|lua| lua.call_initial_mode())
            .unwrap_or_default();

        // Build hot-reloader from consumed config
        let reloader = HotReloader::new(
            self.layout_fs_path,
            self.keybinding_fs_path,
            self.actions_fs_path,
            self.events_fs_path,
            self.botster_api_source,
            self.extension_sources,
        );

        (layout_lua, initial_mode, reloader)
    }
}

// ── Hot-reloader ─────────────────────────────────────────────────────

impl std::fmt::Debug for HotReloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HotReloader")
            .field("has_watcher", &self.watcher.is_some())
            .field("layout_error", &self.layout_error)
            .finish_non_exhaustive()
    }
}

/// Watches filesystem for Lua source changes and reloads in-place.
///
/// Pure state machine — returns `bool` (dirty) from [`HotReloader::poll`].
/// Never touches channels or stdout. The runner checks `poll()` each tick
/// and sets its own dirty flag.
pub struct HotReloader {
    /// File watcher + layout path (None if no filesystem paths to watch).
    watcher: Option<(crate::file_watcher::FileWatcher, PathBuf)>,
    /// Keybinding filesystem path for change detection.
    keybinding_fs_path: Option<PathBuf>,
    /// Actions filesystem path for change detection.
    actions_fs_path: Option<PathBuf>,
    /// Events filesystem path for change detection.
    events_fs_path: Option<PathBuf>,
    /// Botster API source (reloaded when extensions change).
    botster_api_source: Option<String>,
    /// Extension sources for change detection.
    extension_sources: Vec<ExtensionSource>,
    /// Layout error from last failed reload (displayed in UI).
    layout_error: Option<String>,
}

impl HotReloader {
    /// Create a no-op hot-reloader (for tests or when no bootstrap is provided).
    pub fn empty() -> Self {
        Self {
            watcher: None,
            keybinding_fs_path: None,
            actions_fs_path: None,
            events_fs_path: None,
            botster_api_source: None,
            extension_sources: Vec::new(),
            layout_error: None,
        }
    }

    /// Create a new hot-reloader, setting up filesystem watchers.
    fn new(
        layout_fs_path: Option<PathBuf>,
        keybinding_fs_path: Option<PathBuf>,
        actions_fs_path: Option<PathBuf>,
        events_fs_path: Option<PathBuf>,
        botster_api_source: Option<String>,
        extension_sources: Vec<ExtensionSource>,
    ) -> Self {
        let watcher = layout_fs_path.and_then(|path| {
            match crate::file_watcher::FileWatcher::new() {
                Ok(mut watcher) => {
                    // Watch the built-in ui/ directory
                    if let Some(parent) = path.parent() {
                        if let Err(e) = watcher.watch(parent, false) {
                            log::warn!("Failed to watch layout directory: {e}");
                            return None;
                        }
                    }

                    // Watch user directories for extension hot-reload
                    let lua_base = resolve_lua_user_path();
                    for subdir in ["ui", "user/ui"] {
                        let dir = lua_base.join(subdir);
                        if dir.exists() {
                            if let Err(e) = watcher.watch(&dir, false) {
                                log::warn!("Failed to watch {}: {e}", dir.display());
                            } else {
                                log::info!("Hot-reload watching: {}", dir.display());
                            }
                        }
                    }

                    // Watch plugin ui/ directories
                    let mut watched_plugin_dirs = HashSet::new();
                    for ext in &extension_sources {
                        if let Some(parent) = ext.fs_path.parent() {
                            if watched_plugin_dirs.insert(parent.to_path_buf()) {
                                if let Err(e) = watcher.watch(parent, false) {
                                    log::warn!(
                                        "Failed to watch plugin UI dir {}: {e}",
                                        parent.display()
                                    );
                                }
                            }
                        }
                    }

                    log::info!("Hot-reload watching: {}", path.display());
                    Some((watcher, path))
                }
                Err(e) => {
                    log::warn!("Failed to create layout file watcher: {e}");
                    None
                }
            }
        });

        Self {
            watcher,
            keybinding_fs_path,
            actions_fs_path,
            events_fs_path,
            botster_api_source,
            extension_sources,
            layout_error: None,
        }
    }

    /// Poll for filesystem changes and reload Lua state as needed.
    ///
    /// Returns `true` if any source was reloaded (caller should set dirty flag).
    /// Mutates `layout_lua` in-place for reloads and recovery.
    pub fn poll(&mut self, layout_lua: &mut Option<LayoutLua>) -> bool {
        // Poll watcher and clone layout path upfront to release the borrow on self.
        let (events, layout_path) = match self.watcher {
            Some((ref watcher, ref layout_path)) => {
                let events = watcher.poll();
                if events.is_empty() {
                    return false;
                }
                (events, layout_path.clone())
            }
            None => return false,
        };

        let is_modify = |evt: &crate::file_watcher::FileEvent| {
            matches!(
                evt.kind,
                crate::file_watcher::FileEventKind::Create
                    | crate::file_watcher::FileEventKind::Modify
                    | crate::file_watcher::FileEventKind::Rename
            )
        };

        let layout_changed = events
            .iter()
            .any(|evt| is_modify(evt) && evt.path.file_name() == layout_path.file_name());

        let keybinding_changed = self.keybinding_fs_path.as_ref().is_some_and(|kb_path| {
            events
                .iter()
                .any(|evt| is_modify(evt) && evt.path.file_name() == kb_path.file_name())
        });

        let actions_changed = self.actions_fs_path.as_ref().is_some_and(|a_path| {
            events
                .iter()
                .any(|evt| is_modify(evt) && evt.path.file_name() == a_path.file_name())
        });

        let events_changed = self.events_fs_path.as_ref().is_some_and(|e_path| {
            events
                .iter()
                .any(|evt| is_modify(evt) && evt.path.file_name() == e_path.file_name())
        });

        // Check if any extension file changed
        let extension_changed = self.extension_sources.iter().any(|ext| {
            events
                .iter()
                .any(|evt| is_modify(evt) && evt.path == ext.fs_path)
        });

        // Also check if a file changed in user/ui/ or the user override ui/ dir
        let user_ui_changed = events.iter().any(|evt| {
            is_modify(evt)
                && evt.path.extension().is_some_and(|e| e == "lua")
                && evt.path.parent().is_some_and(|p| {
                    p.ends_with("user/ui") || p.ends_with(".botster/lua/ui")
                })
        });

        let any_builtin_changed =
            layout_changed || keybinding_changed || actions_changed || events_changed;
        let any_extension_changed = extension_changed || user_ui_changed;

        if !any_builtin_changed && !any_extension_changed {
            return false;
        }

        // Reload built-in files if they changed
        if layout_changed {
            self.reload_layout(layout_lua, &layout_path);
        }

        if keybinding_changed {
            if let Some(ref kb_path) = self.keybinding_fs_path {
                match std::fs::read_to_string(kb_path) {
                    Ok(new_source) => {
                        if let Some(ref mut lua) = layout_lua {
                            match lua.reload_keybindings(&new_source) {
                                Ok(()) => log::info!("Keybindings hot-reloaded"),
                                Err(e) => log::warn!("Keybindings reload failed: {e}"),
                            }
                        }
                    }
                    Err(e) => log::warn!("Failed to read keybindings.lua: {e}"),
                }
            }
        }

        if actions_changed {
            if let Some(ref a_path) = self.actions_fs_path {
                match std::fs::read_to_string(a_path) {
                    Ok(new_source) => {
                        if let Some(ref mut lua) = layout_lua {
                            match lua.reload_actions(&new_source) {
                                Ok(()) => log::info!("Actions hot-reloaded"),
                                Err(e) => log::warn!("Actions reload failed: {e}"),
                            }
                        }
                    }
                    Err(e) => log::warn!("Failed to read actions.lua: {e}"),
                }
            }
        }

        if events_changed {
            if let Some(ref e_path) = self.events_fs_path {
                match std::fs::read_to_string(e_path) {
                    Ok(new_source) => {
                        if let Some(ref mut lua) = layout_lua {
                            match lua.reload_events(&new_source) {
                                Ok(()) => log::info!("Events hot-reloaded"),
                                Err(e) => log::warn!("Events reload failed: {e}"),
                            }
                        }
                    }
                    Err(e) => log::warn!("Failed to read events.lua: {e}"),
                }
            }
        }

        // Replay extensions if any built-in or extension changed
        if layout_lua.is_some() {
            self.replay_extensions(layout_lua);
        }

        true
    }

    /// Current layout error (from last failed reload), if any.
    pub fn layout_error(&self) -> Option<&str> {
        self.layout_error.as_deref()
    }

    /// Reload layout from filesystem into existing or fresh Lua state.
    fn reload_layout(&mut self, layout_lua: &mut Option<LayoutLua>, layout_path: &PathBuf) {
        match std::fs::read_to_string(layout_path) {
            Ok(new_source) => {
                if let Some(ref mut lua) = layout_lua {
                    match lua.reload(&new_source) {
                        Ok(()) => {
                            log::info!("Layout hot-reloaded");
                            self.layout_error = None;
                        }
                        Err(e) => {
                            let msg = format!("{e}");
                            log::warn!("Layout reload failed: {msg}");
                            self.layout_error = Some(truncate_error(&msg, 80));
                        }
                    }
                } else {
                    match LayoutLua::new(&new_source) {
                        Ok(lua) => {
                            log::info!("Layout engine recovered via hot-reload");
                            *layout_lua = Some(lua);
                            self.layout_error = None;
                        }
                        Err(e) => {
                            let msg = format!("{e}");
                            log::warn!("Layout reload failed: {msg}");
                            self.layout_error = Some(truncate_error(&msg, 80));
                        }
                    }
                }
            }
            Err(e) => log::warn!("Failed to read layout.lua: {e}"),
        }
    }

    /// Re-discover and replay all extensions into the Lua state.
    fn replay_extensions(&self, layout_lua: &mut Option<LayoutLua>) {
        let Some(ref lua) = layout_lua else { return };

        // Re-discover extensions and user overrides (picks up new files)
        let lua_base = resolve_lua_user_path();
        let mut fresh_extensions = discover_ui_extensions(&lua_base);
        fresh_extensions.extend(discover_user_ui_overrides());

        // Reload botster API
        if let Some(ref bs) = self.botster_api_source {
            if let Err(e) = lua.load_extension(bs, "botster") {
                log::warn!("Failed to reload botster API: {e}");
            }
        }

        // Replay all extensions (freshly read by discover_ui_extensions)
        for ext in &fresh_extensions {
            if let Err(e) = lua.load_extension(&ext.source, &ext.name) {
                log::warn!("Failed to reload extension '{}': {e}", ext.name);
            }
        }

        // Re-wire dispatch
        let _ = lua.load_extension(
            "if type(botster) == 'table' then botster._wire_actions() botster._wire_keybindings() end",
            "_wire_botster",
        );

        log::info!("Extensions replayed ({} total)", fresh_extensions.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_error_short() {
        assert_eq!(truncate_error("short error", 80), "short error");
    }

    #[test]
    fn test_truncate_error_long() {
        let long = "a".repeat(100);
        let result = truncate_error(&long, 20);
        assert_eq!(result.len(), 20);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_error_multiline() {
        let msg = "first line\nsecond line\nthird line";
        assert_eq!(truncate_error(msg, 80), "first line");
    }
}
