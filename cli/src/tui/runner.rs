//! TUI Runner - independent TUI thread with its own event loop.
//!
//! The TuiRunner owns all TUI state and runs in its own thread, communicating
//! with the Hub via channels. This isolates terminal handling from hub logic.
//!
//! # Architecture
//!
//! ```text
//! TuiRunner (TUI thread)
//! ├── parser_pool: HashMap<(agent, pty), Parser>  - per-PTY terminal emulation
//! ├── vt100_parser: Arc<Mutex<Parser>>  - alias into pool for focused PTY
//! ├── active_subscriptions: HashSet<(agent, pty)>  - synced from render tree
//! ├── terminal: Terminal<CrosstermBackend>  - ratatui terminal
//! ├── mode (shadow of Lua _tui_state.mode)  - for PTY routing
//! ├── selected_agent, current_agent_index  - focus state
//! ├── request_tx  - send requests to Hub
//! └── output_rx  - receive PTY output and Lua events from Hub
//! ```
//!
//! # Event Loop
//!
//! The TuiRunner event loop:
//! 1. Polls for keyboard/mouse input
//! 2. Polls for PTY output and Lua events (via Hub output channel)
//! 3. Renders the UI
//!
//! All communication with Hub is non-blocking via channels.
//!
//! # Running with Hub
//!
//! Use [`run_with_hub`] to run the TUI alongside a Hub. This spawns
//! TuiRunner in a dedicated thread while the main thread runs Hub
//! operations.
//!
//! # Module Organization
//!
//! Handler methods are split across modules for maintainability:
//! - [`super::runner_handlers`] - `handle_tui_action()` for generic UI actions
//!
//! # Event Flow
//!
//! Agent lifecycle events flow through Lua (`broadcast_hub_event()` in
//! `connections.lua`) and arrive as `TuiOutput::Message` JSON. TuiRunner
//! dispatches these through `events.lua` via `call_on_hub_event()`, which
//! returns ops that update cached state mechanically.

// Rust guideline compliant 2026-02

use std::io::Stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::execute;
use ratatui::backend::Backend;
use ratatui::Terminal;
use vt100::Parser;

use ratatui::backend::CrosstermBackend;

use crate::client::{TuiOutput, TuiRequest};
use crate::hub::Hub;
use crate::tui::layout::terminal_widget_inner_area;

use super::actions::TuiAction;
use super::layout_lua::{KeyContext, LayoutLua, LuaKeyAction};
use super::raw_input::{InputEvent, RawInputReader, ScrollDirection};
use super::qr::ConnectionCodeData;

/// Default scrollback lines for VT100 parser.
pub(super) const DEFAULT_SCROLLBACK: usize = 1000;

/// TUI Runner - owns all TUI state and runs the TUI event loop.
///
/// Created by Hub and spawned in its own thread. Communicates with Hub
/// exclusively via channels, enabling clean separation of concerns.
///
/// The `B` type parameter is the ratatui backend type. For production use,
/// this is `CrosstermBackend<Stdout>`. For testing, `TestBackend` can be used.
///
/// # Architecture
///
/// ```text
/// Hub (main thread)
/// ├── Lua runtime (client.lua) ──► tui.send() ──► TuiOutput::Message
/// └── PTY forwarders ──────────────────────────► TuiOutput::Output
///                                                       │
///                                            TuiRunner (output_rx)
///                                            ──► vt100_parser ──► render
/// ```
///
/// TuiRunner sends `TuiRequest` messages through `request_tx`: control messages
/// go through Lua `client.lua`, PTY keyboard input goes directly to the PTY.
pub struct TuiRunner<B: Backend> {
    // === Terminal ===
    /// VT100 parser for the currently active PTY.
    ///
    /// This is an Arc clone of the active entry in `parser_pool`, so writing
    /// to one updates both. Existing scroll/resize code uses this directly.
    pub(super) vt100_parser: Arc<Mutex<Parser>>,

    /// Pool of VT100 parsers keyed by `(agent_index, pty_index)`.
    ///
    /// Each visible terminal widget can bind to a different parser.
    /// Parsers are created on demand when PTY output arrives.
    pub(super) parser_pool: std::collections::HashMap<(usize, usize), Arc<Mutex<Parser>>>,

    /// Ratatui terminal for rendering.
    terminal: Terminal<B>,

    // === UI State ===
    /// Mode shadow for PTY routing. Canonical state is `_tui_state.mode` in Lua.
    pub(super) mode: String,

    /// Current connection code data (URL + QR ASCII) for display.
    pub(super) connection_code: Option<ConnectionCodeData>,

    /// Error message to display in Error mode.
    pub(super) error_message: Option<String>,

    // === Channels ===
    /// Request sender to Hub.
    ///
    /// Control messages (resize, subscriptions, agent lifecycle) are wrapped
    /// in `TuiRequest::LuaMessage` and routed through Lua. PTY keyboard input
    /// is sent as `TuiRequest::PtyInput` and written directly to the PTY.
    pub(super) request_tx: tokio::sync::mpsc::UnboundedSender<TuiRequest>,

    // === Selection State ===
    /// Currently selected agent ID.
    ///
    /// The agent ID (session key) of the currently selected agent.
    pub(super) selected_agent: Option<String>,

    /// Active PTY session index (0 = first session, typically "agent").
    ///
    /// Cycles through available sessions with Ctrl+]. TuiRunner owns this state.
    pub(super) active_pty_index: usize,

    /// Index of the agent currently being viewed/interacted with.
    ///
    /// Used for Lua subscribe/unsubscribe operations.
    pub(super) current_agent_index: Option<usize>,

    /// Index of the PTY currently being viewed/interacted with.
    ///
    /// 0 = CLI PTY, 1 = Server PTY. This tracks which PTY receives keyboard
    /// input and is displayed in the terminal widget.
    pub(super) current_pty_index: Option<usize>,

    /// Active terminal subscription ID for the focused PTY (receives keyboard input).
    ///
    /// Tracks which PTY subscription receives keyboard input and resize events.
    /// Uses the same subscription protocol as browser clients.
    pub(super) current_terminal_sub_id: Option<String>,

    /// Set of `(agent_index, pty_index)` pairs with active Lua subscriptions.
    ///
    /// Maintained by `sync_subscriptions()` which diffs the desired bindings
    /// (from the render tree) against this set, sending subscribe/unsubscribe
    /// messages as needed. This enables Lua to declaratively control which
    /// PTYs receive data without manual subscription management.
    pub(super) active_subscriptions: std::collections::HashSet<(usize, usize)>,

    /// Last known widget dimensions (rows, cols) for each terminal binding.
    ///
    /// Tracks the rendered area of each terminal widget so we can detect
    /// when a PTY needs resizing (layout change, terminal resize, etc.).
    pub(super) widget_dims: std::collections::HashMap<(usize, usize), (u16, u16)>,

    // === Output Channel ===
    /// Receiver for PTY output and Lua events from Hub.
    ///
    /// Hub sends `TuiOutput` messages through this channel: binary PTY data
    /// from Lua forwarder tasks and JSON events from `tui.send()` in Lua.
    output_rx: tokio::sync::mpsc::UnboundedReceiver<TuiOutput>,

    // === Control ===
    /// Shutdown flag (shared with Hub for coordinated shutdown).
    shutdown: Arc<AtomicBool>,

    /// Internal quit flag.
    pub(super) quit: bool,

    // === Dimensions ===
    /// Terminal dimensions (rows, cols).
    pub(super) terminal_dims: (u16, u16),

    // === Lua Layout ===
    /// Lua layout source code, loaded into a LayoutLua state after thread spawn.
    ///
    /// Stored as String (Send) rather than LayoutLua (!Send) so TuiRunner
    /// can be moved across threads. Converted to LayoutLua in run().
    layout_lua_source: Option<String>,

    /// Filesystem path to layout.lua for hot-reload watching.
    /// None if loaded from embedded (no watching needed).
    layout_lua_fs_path: Option<std::path::PathBuf>,

    // === Lua Keybindings ===
    /// Lua keybinding source code, loaded alongside layout.
    keybinding_lua_source: Option<String>,

    /// Filesystem path to keybindings.lua for hot-reload watching.
    keybinding_lua_fs_path: Option<std::path::PathBuf>,

    // === Lua Actions ===
    /// Lua actions source code for compound action dispatch.
    actions_lua_source: Option<String>,

    /// Filesystem path to actions.lua for hot-reload watching.
    actions_lua_fs_path: Option<std::path::PathBuf>,

    // === Lua Events ===
    /// Lua events source code for hub event handling.
    events_lua_source: Option<String>,

    /// Filesystem path to events.lua for hot-reload watching.
    events_lua_fs_path: Option<std::path::PathBuf>,

    // === Lua Extensions ===
    /// Botster API source (loaded after built-ins, before extensions).
    botster_api_source: Option<String>,

    /// UI extension sources from plugins and user directory.
    /// Loaded in order after botster API: plugins first, then user.
    extension_sources: Vec<ExtensionSource>,

    // === Raw Input ===
    /// Raw stdin reader — replaces crossterm's event reader for keyboard input.
    raw_reader: RawInputReader,

    /// SIGWINCH flag for terminal resize detection.
    pub(super) resize_flag: Arc<AtomicBool>,

    // === Terminal Mode Mirroring ===
    /// Whether we've pushed application cursor mode (DECCKM) to the outer terminal.
    outer_app_cursor: bool,
    /// Whether we've pushed bracketed paste mode to the outer terminal.
    outer_bracketed_paste: bool,

    // === Kitty Keyboard Protocol Mirroring ===
    /// Whether the active PTY has pushed Kitty keyboard protocol.
    /// Detected by scanning PTY output for CSI > flags u (push) / CSI < u (pop).
    inner_kitty_enabled: bool,
    /// Whether we've pushed Kitty to the outer terminal.
    outer_kitty_enabled: bool,

    // === Cached Overlay State ===
    /// Action strings for selectable items in the current overlay list widget.
    ///
    /// Populated after each Lua render pass by extracting actions from the
    /// overlay render tree. Indexed by selectable item index (matches
    /// `_tui_state.list_selected` in Lua). Used by Lua compound action dispatch.
    pub(super) overlay_list_actions: Vec<String>,

    /// Whether a Lua overlay is currently active (from last render pass).
    ///
    /// Used to decide whether raw input goes to PTY (no overlay) or is
    /// consumed by keybindings (overlay active). Derived from Lua
    /// `render_overlay()` returning non-nil.
    has_overlay: bool,
}

impl<B: Backend> std::fmt::Debug for TuiRunner<B>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiRunner")
            .field("mode", &self.mode)
            .field("selected_agent", &self.selected_agent)
            .field("current_agent_index", &self.current_agent_index)
            .field("terminal_dims", &self.terminal_dims)
            .field("quit", &self.quit)
            .finish_non_exhaustive()
    }
}

impl<B> TuiRunner<B>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Create a new TuiRunner.
    ///
    /// # Arguments
    ///
    /// * `terminal` - The ratatui terminal (ownership transferred to runner)
    /// * `request_tx` - Sender for requests to Hub
    /// * `output_rx` - Receiver for PTY output and Lua events from Hub
    /// * `shutdown` - Shared shutdown flag
    /// * `terminal_dims` - Initial terminal dimensions (rows, cols)
    ///
    /// # Returns
    ///
    /// A new TuiRunner ready to run.
    pub fn new(
        terminal: Terminal<B>,
        request_tx: tokio::sync::mpsc::UnboundedSender<TuiRequest>,
        output_rx: tokio::sync::mpsc::UnboundedReceiver<TuiOutput>,
        shutdown: Arc<AtomicBool>,
        terminal_dims: (u16, u16),
    ) -> Self {
        let (rows, cols) = terminal_dims;
        let parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
        let vt100_parser = Arc::new(Mutex::new(parser));

        Self {
            vt100_parser,
            parser_pool: std::collections::HashMap::new(),
            terminal,
            mode: String::new(),
            connection_code: None,
            error_message: None,
            request_tx,
            selected_agent: None,
            active_pty_index: 0,
            current_agent_index: None,
            current_pty_index: None,
            current_terminal_sub_id: None,
            active_subscriptions: std::collections::HashSet::new(),
            widget_dims: std::collections::HashMap::new(),
            output_rx,
            shutdown,
            quit: false,
            terminal_dims,
            layout_lua_source: None,
            layout_lua_fs_path: None,
            keybinding_lua_source: None,
            keybinding_lua_fs_path: None,
            actions_lua_source: None,
            actions_lua_fs_path: None,
            events_lua_source: None,
            events_lua_fs_path: None,
            botster_api_source: None,
            extension_sources: Vec::new(),
            raw_reader: RawInputReader::new(),
            resize_flag: Arc::new(AtomicBool::new(false)),
            outer_app_cursor: false,
            outer_bracketed_paste: false,
            inner_kitty_enabled: false,
            outer_kitty_enabled: false,
            overlay_list_actions: Vec::new(),
            has_overlay: false,
        }
    }

    /// Set the Lua layout source for declarative UI.
    ///
    /// The source is stored as a string and loaded into a `LayoutLua` state
    /// when `run()` is called (after the TuiRunner is moved to its thread).
    pub fn set_layout_lua_source(&mut self, lua_source: String) {
        self.layout_lua_source = Some(lua_source);
    }

    /// Set the Lua keybinding source for hot-reloadable key handling.
    pub fn set_keybinding_lua_source(&mut self, lua_source: String) {
        self.keybinding_lua_source = Some(lua_source);
    }

    /// Set the Lua actions source for compound action dispatch.
    pub fn set_actions_lua_source(&mut self, lua_source: String) {
        self.actions_lua_source = Some(lua_source);
    }

    /// Set the Lua events source for hub event handling.
    pub fn set_events_lua_source(&mut self, lua_source: String) {
        self.events_lua_source = Some(lua_source);
    }

    /// Get the VT100 parser handle for the active PTY.
    ///
    /// Used for rendering the terminal content.
    #[must_use]
    pub fn parser_handle(&self) -> Arc<Mutex<Parser>> {
        Arc::clone(&self.vt100_parser)
    }

    /// Resolve a parser from the pool by agent/PTY identity.
    ///
    /// If identity is `Some`, looks up or creates a parser in the pool.
    /// If identity is `None`, falls back to `current_agent_index`/`current_pty_index`.
    /// Creates parsers on demand with current terminal dimensions.
    fn resolve_parser(
        &mut self,
        agent_index: Option<usize>,
        pty_index: Option<usize>,
    ) -> Arc<Mutex<Parser>> {
        let key = (
            agent_index.or(self.current_agent_index).unwrap_or(0),
            pty_index.or(self.current_pty_index).unwrap_or(0),
        );
        let (rows, cols) = self.terminal_dims;
        self.parser_pool
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(Parser::new(rows, cols, DEFAULT_SCROLLBACK))))
            .clone()
    }

    /// Synchronize PTY subscriptions to match what the render tree needs.
    ///
    /// Walks the render tree, collects all `(agent_index, pty_index)` bindings
    /// from terminal widgets, then diffs against `active_subscriptions`:
    /// - **New pairs**: sends subscribe JSON, creates parser in pool
    /// - **Removed pairs**: sends unsubscribe JSON, removes parser from pool
    ///
    /// This is the declarative reconciliation layer: Lua says "show these PTYs",
    /// Rust ensures exactly those subscriptions are active. Called after each
    /// successful Lua render.
    pub(super) fn sync_subscriptions(&mut self, tree: &super::render_tree::RenderNode) {
        let default_agent = self.current_agent_index.unwrap_or(0);
        let default_pty = self.current_pty_index.unwrap_or(0);

        let desired = super::render_tree::collect_terminal_bindings(tree, default_agent, default_pty);

        // Subscribe to new pairs
        for &(agent_idx, pty_idx) in &desired {
            if !self.active_subscriptions.contains(&(agent_idx, pty_idx)) {
                let sub_id = format!("tui:{}:{}", agent_idx, pty_idx);
                self.send_msg(serde_json::json!({
                    "type": "subscribe",
                    "channel": "terminal",
                    "subscriptionId": sub_id,
                    "params": {
                        "agent_index": agent_idx,
                        "pty_index": pty_idx,
                    }
                }));

                // Ensure parser exists for this binding.
                // Use per-widget dims if known (from previous render), else full terminal dims.
                let (rows, cols) = self.widget_dims
                    .get(&(agent_idx, pty_idx))
                    .copied()
                    .unwrap_or(self.terminal_dims);
                self.parser_pool
                    .entry((agent_idx, pty_idx))
                    .or_insert_with(|| Arc::new(Mutex::new(Parser::new(rows, cols, DEFAULT_SCROLLBACK))));
            }
        }

        // Unsubscribe from removed pairs
        for &(agent_idx, pty_idx) in &self.active_subscriptions {
            if !desired.contains(&(agent_idx, pty_idx)) {
                let sub_id = format!("tui:{}:{}", agent_idx, pty_idx);
                self.send_msg(serde_json::json!({
                    "type": "unsubscribe",
                    "subscriptionId": sub_id,
                }));

                // Remove parser from pool (data no longer needed)
                self.parser_pool.remove(&(agent_idx, pty_idx));
            }
        }

        self.active_subscriptions = desired;
    }

    /// Resize parsers and PTYs to match the actual rendered widget areas.
    ///
    /// Called after each render pass. Compares the collected terminal widget
    /// areas against the last known dimensions. For any that changed, resizes
    /// both the vt100 parser (so output is interpreted correctly) and sends a
    /// resize command to the PTY process (so programs like vim/less reflow).
    pub(super) fn sync_widget_dims(
        &mut self,
        areas: &std::collections::HashMap<(usize, usize), (u16, u16)>,
    ) {
        for (&(agent_idx, pty_idx), &(rows, cols)) in areas {
            if rows == 0 || cols == 0 {
                continue;
            }

            let prev = self.widget_dims.get(&(agent_idx, pty_idx));
            if prev == Some(&(rows, cols)) {
                continue; // No change
            }

            self.widget_dims.insert((agent_idx, pty_idx), (rows, cols));

            // Resize the parser
            if let Some(parser) = self.parser_pool.get(&(agent_idx, pty_idx)) {
                let mut p = parser.lock().expect("parser lock poisoned");
                p.screen_mut().set_size(rows, cols);
            }

            // Send resize to the PTY process
            let sub_id = format!("tui:{}:{}", agent_idx, pty_idx);
            self.send_msg(serde_json::json!({
                "subscriptionId": sub_id,
                "data": {
                    "type": "resize",
                    "rows": rows,
                    "cols": cols,
                }
            }));
        }

        // Clean up stale entries for bindings no longer rendered
        self.widget_dims.retain(|k, _| areas.contains_key(k));
    }

    /// Get the current mode string.
    #[must_use]
    pub fn mode(&self) -> &str {
        &self.mode
    }

    /// Get the selected agent key.
    #[must_use]
    pub fn selected_agent(&self) -> Option<&str> {
        self.selected_agent.as_deref()
    }

    /// Get the agent list.
    /// Build an `ActionContext` from current TuiRunner state.
    ///
    /// Shared by action dispatch and hub event dispatch so both Lua
    /// callbacks receive the same context shape.
    pub(super) fn build_action_context(&self) -> super::layout_lua::ActionContext {
        super::layout_lua::ActionContext {
            overlay_actions: self.overlay_list_actions.clone(),
            selected_agent: self.selected_agent.clone(),
            selected_agent_index: self.current_agent_index,
            active_pty_index: self.active_pty_index,
            action_char: None,
        }
    }

    /// Check if the runner should quit.
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit || self.shutdown.load(Ordering::SeqCst)
    }

    /// Run the TUI event loop.
    ///
    /// This is the main entry point. Blocks until quit is requested.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal operations fail.
    pub fn run(&mut self) -> Result<()> {
        log::info!("TuiRunner event loop starting");

        // Create LayoutLua from stored source (if any).
        // Done here (after thread::spawn) because mlua::Lua is !Send.
        let mut layout_lua = self.layout_lua_source.take().and_then(|source| {
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

        // Load keybindings and actions into the same Lua state (if available).
        if let Some(ref mut lua) = layout_lua {
            if let Some(kb_source) = self.keybinding_lua_source.take() {
                match lua.load_keybindings(&kb_source) {
                    Ok(()) => log::info!("Lua keybindings loaded"),
                    Err(e) => log::warn!("Failed to load Lua keybindings: {e}"),
                }
            }
            if let Some(actions_source) = self.actions_lua_source.take() {
                match lua.load_actions(&actions_source) {
                    Ok(()) => log::info!("Lua actions loaded"),
                    Err(e) => log::warn!("Failed to load Lua actions: {e}"),
                }
            }
            if let Some(events_source) = self.events_lua_source.take() {
                match lua.load_events(&events_source) {
                    Ok(()) => log::info!("Lua events loaded"),
                    Err(e) => log::warn!("Failed to load Lua events: {e}"),
                }
            }

            // Bootstrap TUI client-side state.
            // _tui_state is LayoutLua's local state — same role as JavaScript
            // state in the browser client. All UI modules read/write it directly.
            let _ = lua.load_extension(
                "_tui_state = _tui_state or {\
                    agents = {},\
                    pending_fields = {},\
                    available_worktrees = {},\
                    mode = 'normal',\
                    input_buffer = '',\
                    list_selected = 0,\
                    selected_agent = nil,\
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

            // Let Lua declare the initial mode (Rust has no opinion on mode names)
            self.mode = lua.call_initial_mode();
        }

        // Set up file watcher for hot-reload.
        // Watches: built-in ui/ directory, user ui/ directory, and plugin ui/ directories.
        let keybinding_fs_path = self.keybinding_lua_fs_path.take();
        let actions_fs_path = self.actions_lua_fs_path.take();
        let events_fs_path = self.events_lua_fs_path.take();
        let extension_sources = std::mem::take(&mut self.extension_sources);
        let botster_api_source = self.botster_api_source.take();

        let layout_watcher = self.layout_lua_fs_path.take().and_then(|path| {
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
                    let mut watched_plugin_dirs = std::collections::HashSet::new();
                    for ext in &extension_sources {
                        if let Some(parent) = ext.fs_path.parent() {
                            if watched_plugin_dirs.insert(parent.to_path_buf()) {
                                if let Err(e) = watcher.watch(parent, false) {
                                    log::warn!("Failed to watch plugin UI dir {}: {e}", parent.display());
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

        // Error tracking for layout Lua failures
        let mut layout_error: Option<String> = None;

        // Initialize parser with terminal dimensions
        let (rows, cols) = self.terminal_dims;
        log::info!("Initial TUI dimensions: {}cols x {}rows", cols, rows);

        // Send initial resize to Lua client so it knows the actual terminal
        // dimensions before the first terminal subscription. Without this,
        // the Lua Client defaults to 24x80 and the first PTY gets wrong dims.
        self.send_msg(serde_json::json!({
            "subscriptionId": "tui_hub",
            "data": {
                "type": "resize",
                "rows": rows,
                "cols": cols,
            }
        }));

        while !self.should_quit() {
            // 1. Handle keyboard/mouse input
            self.poll_input(layout_lua.as_ref());

            if self.should_quit() {
                break;
            }

            // 2. Poll PTY output and Lua events (via Hub output channel)
            self.poll_pty_events(layout_lua.as_ref());

            // 2b. Mirror terminal modes from PTY to outer terminal
            self.sync_terminal_modes();

            // 3. Hot-reload: built-in UI files and extensions
            if let Some((ref watcher, ref layout_path)) = layout_watcher {
                let events = watcher.poll();
                if !events.is_empty() {
                    let is_modify = |evt: &crate::file_watcher::FileEvent| {
                        matches!(
                            evt.kind,
                            crate::file_watcher::FileEventKind::Create
                                | crate::file_watcher::FileEventKind::Modify
                                | crate::file_watcher::FileEventKind::Rename
                        )
                    };

                    let layout_changed = events.iter().any(|evt| {
                        is_modify(evt) && evt.path.file_name() == layout_path.file_name()
                    });

                    let keybinding_changed = keybinding_fs_path.as_ref().map_or(false, |kb_path| {
                        events.iter().any(|evt| {
                            is_modify(evt) && evt.path.file_name() == kb_path.file_name()
                        })
                    });

                    let actions_changed = actions_fs_path.as_ref().map_or(false, |a_path| {
                        events.iter().any(|evt| {
                            is_modify(evt) && evt.path.file_name() == a_path.file_name()
                        })
                    });

                    let events_changed = events_fs_path.as_ref().map_or(false, |e_path| {
                        events.iter().any(|evt| {
                            is_modify(evt) && evt.path.file_name() == e_path.file_name()
                        })
                    });

                    // Check if any extension file changed
                    let extension_changed = extension_sources.iter().any(|ext| {
                        events.iter().any(|evt| is_modify(evt) && evt.path == ext.fs_path)
                    });

                    // Also check if a file changed in user/ui/ or the user override ui/ dir
                    let user_ui_changed = events.iter().any(|evt| {
                        is_modify(evt)
                            && evt.path.extension().is_some_and(|e| e == "lua")
                            && evt.path.parent().map_or(false, |p| {
                                p.ends_with("user/ui") || p.ends_with(".botster/lua/ui")
                            })
                    });

                    let any_builtin_changed = layout_changed || keybinding_changed || actions_changed || events_changed;
                    let any_extension_changed = extension_changed || user_ui_changed;

                    // Reload built-in files if they changed
                    if layout_changed {
                        match std::fs::read_to_string(layout_path) {
                            Ok(new_source) => {
                                if let Some(ref mut lua) = layout_lua {
                                    match lua.reload(&new_source) {
                                        Ok(()) => {
                                            log::info!("Layout hot-reloaded");
                                            layout_error = None;
                                        }
                                        Err(e) => {
                                            let msg = format!("{e}");
                                            log::warn!("Layout reload failed: {msg}");
                                            layout_error = Some(truncate_error(&msg, 80));
                                        }
                                    }
                                } else {
                                    match LayoutLua::new(&new_source) {
                                        Ok(lua) => {
                                            log::info!("Layout engine recovered via hot-reload");
                                            layout_lua = Some(lua);
                                            layout_error = None;
                                        }
                                        Err(e) => {
                                            let msg = format!("{e}");
                                            log::warn!("Layout reload failed: {msg}");
                                            layout_error = Some(truncate_error(&msg, 80));
                                        }
                                    }
                                }
                            }
                            Err(e) => log::warn!("Failed to read layout.lua: {e}"),
                        }
                    }

                    if keybinding_changed {
                        if let Some(ref kb_path) = keybinding_fs_path {
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
                        if let Some(ref a_path) = actions_fs_path {
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
                        if let Some(ref e_path) = events_fs_path {
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
                    if (any_builtin_changed || any_extension_changed) && layout_lua.is_some() {
                        if let Some(ref lua) = layout_lua {
                            // Re-discover extensions and user overrides (picks up new files)
                            let lua_base = resolve_lua_user_path();
                            let mut fresh_extensions = discover_ui_extensions(&lua_base);
                            fresh_extensions.extend(discover_user_ui_overrides());

                            // Reload botster API
                            if let Some(ref bs) = botster_api_source {
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
                }
            }

            // 4. Render
            self.render(layout_lua.as_ref(), layout_error.as_deref())?;

            // Small sleep to prevent CPU spinning (60 FPS max)
            std::thread::sleep(Duration::from_millis(16));
        }

        log::info!("TuiRunner event loop exiting");
        Ok(())
    }

    /// Poll for keyboard/mouse input and handle it.
    ///
    /// Reads raw bytes from stdin and parses them into events. Also checks
    /// the SIGWINCH flag for terminal resize. This replaces crossterm's
    /// event reader to preserve raw bytes for PTY passthrough.
    fn poll_input(&mut self, layout_lua: Option<&LayoutLua>) {
        let events = self.raw_reader.drain_events();
        for event in events {
            self.handle_raw_input_event(event, layout_lua);
        }
        // Check SIGWINCH resize flag
        if self.resize_flag.swap(false, Ordering::SeqCst) {
            let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
            let (inner_rows, inner_cols) = terminal_widget_inner_area(cols, rows);
            self.handle_resize(inner_rows, inner_cols);
        }
    }

    /// Handle a raw input event from the stdin reader.
    ///
    /// Key events go through Lua keybinding dispatch:
    /// 1. Use descriptor from raw byte parser (same format Lua expects)
    /// 2. Ctrl+Q is hardcoded as Quit (safety — works even if Lua is broken)
    /// 3. Call Lua `handle_key(descriptor, mode, context)`
    /// 4. If Lua returns an action → map to `TuiAction` and handle
    /// 5. If Lua returns `nil` in Normal mode → forward original raw bytes to PTY
    /// 6. If Lua returns `nil` in modal mode → ignore (swallow key)
    ///
    /// Mouse scroll events are handled directly.
    fn handle_raw_input_event(&mut self, event: InputEvent, layout_lua: Option<&LayoutLua>) {
        match event {
            InputEvent::Key { descriptor, raw_bytes } => {
                // Safety: Ctrl+Q always works, even if Lua is broken.
                // Sends quit message directly (duplicates Lua's quit action)
                // because this path must work without Lua.
                if descriptor == "ctrl+q" {
                    self.send_msg(serde_json::json!({
                        "subscriptionId": "tui_hub",
                        "data": { "type": "quit" }
                    }));
                    self.quit = true;
                    return;
                }

                // Try Lua keybinding dispatch
                if let Some(lua) = layout_lua {
                    if lua.has_keybindings() {
                        let context = KeyContext {
                            list_count: self.overlay_list_actions.len(),
                            terminal_rows: self.terminal_dims.0,
                        };

                        match lua.call_handle_key(&descriptor, &self.mode, &context) {
                            Ok(Some(lua_action)) => {
                                self.handle_lua_key_action(&lua_action, lua);
                                return;
                            }
                            Ok(None) => {
                                // Unbound key — forward raw bytes to PTY only in insert mode
                                if self.mode == "insert" && !self.has_overlay && !raw_bytes.is_empty() {
                                    self.handle_pty_input(&raw_bytes);
                                }
                                return;
                            }
                            Err(e) => {
                                log::warn!("Lua handle_key failed: {e}");
                                if self.mode == "insert" && !self.has_overlay && !raw_bytes.is_empty() {
                                    self.handle_pty_input(&raw_bytes);
                                }
                                return;
                            }
                        }
                    }
                }

                // No Lua keybindings loaded — forward raw bytes only in insert mode
                if self.mode == "insert" && !self.has_overlay && !raw_bytes.is_empty() {
                    self.handle_pty_input(&raw_bytes);
                }
            }
            InputEvent::MouseScroll { direction } => {
                if !self.has_overlay {
                    match direction {
                        ScrollDirection::Up => {
                            self.handle_tui_action(TuiAction::ScrollUp(3));
                        }
                        ScrollDirection::Down => {
                            self.handle_tui_action(TuiAction::ScrollDown(3));
                        }
                    }
                }
            }
        }
    }

    /// Map a Lua key action to a `TuiAction` and handle it.
    ///
    /// Generic actions (scroll, list nav, input chars) are mapped directly to
    /// `TuiAction` variants. Application-specific actions (list_select,
    /// input_submit, confirm_close, etc.) are dispatched through Lua
    /// `actions.on_action()` which returns compound operations for Rust
    /// to execute generically.
    fn handle_lua_key_action(&mut self, lua_action: &LuaKeyAction, layout_lua: &LayoutLua) {
        let action_str = lua_action.action.as_str();

        // Generic UI primitives that Rust handles directly.
        // Scroll actions are handled directly by Rust (no Lua state involved).
        let scroll_action = match action_str {
            "scroll_half_up" => {
                Some(TuiAction::ScrollUp(self.terminal_dims.0 as usize / 2))
            }
            "scroll_half_down" => {
                Some(TuiAction::ScrollDown(self.terminal_dims.0 as usize / 2))
            }
            "scroll_top" => Some(TuiAction::ScrollToTop),
            "scroll_bottom" => Some(TuiAction::ScrollToBottom),
            _ => None,
        };

        if let Some(tui_action) = scroll_action {
            self.handle_tui_action(tui_action);
            return;
        }

        // Everything else goes through Lua compound action dispatch.
        // Lua owns mode, input_buffer, list_selected via _tui_state.
        if layout_lua.has_actions() {
            let mut context = self.build_action_context();

            // Pass character for input_char action
            if action_str == "input_char" {
                context.action_char = lua_action.char;
            }

            // Pass list_select index override
            if let Some(idx) = lua_action.index {
                // Number shortcut — temporarily set list_selected in _tui_state
                // so actions.lua sees the right index
                let _ = layout_lua.exec(&format!("_tui_state.list_selected = {idx}"));
            }

            match layout_lua.call_on_action(action_str, &context) {
                Ok(Some(ops)) => {
                    self.execute_lua_ops(ops);
                    return;
                }
                Ok(None) => {
                    log::debug!("Lua actions returned nil for '{action_str}', no-op");
                }
                Err(e) => {
                    log::warn!("Lua on_action failed for '{action_str}': {e}");
                }
            }
        } else {
            log::warn!("No Lua actions module loaded, cannot handle '{action_str}'");
        }
    }

    /// Send raw PTY input bytes directly to the PTY writer.
    ///
    /// Bypasses Lua entirely — no JSON serialization, no `from_utf8_lossy`.
    /// Uses `current_agent_index` and `current_pty_index` to route to the
    /// correct PTY. No-op if no PTY is currently focused.
    fn handle_pty_input(&mut self, data: &[u8]) {
        if let (Some(agent_index), Some(pty_index)) =
            (self.current_agent_index, self.current_pty_index)
        {
            if let Err(e) = self.request_tx.send(TuiRequest::PtyInput {
                agent_index,
                pty_index,
                data: data.to_vec(),
            }) {
                log::error!("Failed to send PTY input: {e}");
            }
        }
    }

    /// Mirror terminal modes from PTY to the outer terminal.
    ///
    /// When PTY apps (vim, less) change terminal modes via escape sequences,
    /// we push those same modes to the outer terminal. This makes raw stdin
    /// passthrough work correctly — the outer terminal generates the same
    /// byte sequences that the PTY app expects.
    ///
    /// Tracked modes:
    /// - DECCKM (application cursor): arrow keys send ESC O x vs ESC [ x
    /// - Bracketed paste: paste is wrapped in ESC [200~ / ESC [201~
    fn sync_terminal_modes(&mut self) {
        let parser = self.vt100_parser.lock().expect("parser lock poisoned");
        let screen = parser.screen();

        let app_cursor = screen.application_cursor();
        if app_cursor != self.outer_app_cursor {
            self.outer_app_cursor = app_cursor;
            let seq = if app_cursor {
                b"\x1b[?1h" as &[u8]
            } else {
                b"\x1b[?1l" as &[u8]
            };
            let _ = std::io::Write::write_all(&mut std::io::stdout(), seq);
        }

        let bp = screen.bracketed_paste();
        if bp != self.outer_bracketed_paste {
            self.outer_bracketed_paste = bp;
            let seq = if bp {
                b"\x1b[?2004h" as &[u8]
            } else {
                b"\x1b[?2004l" as &[u8]
            };
            let _ = std::io::Write::write_all(&mut std::io::stdout(), seq);
        }

        // Drop the parser lock before writing Kitty sequences (which use execute!())
        drop(parser);

        // Kitty keyboard protocol: only push when PTY wants it AND we're in Normal mode.
        // In modal modes (menu, input, etc.) we want traditional bytes for our keybindings.
        let desired_kitty = self.inner_kitty_enabled && !self.has_overlay;
        if desired_kitty != self.outer_kitty_enabled {
            self.outer_kitty_enabled = desired_kitty;
            if desired_kitty {
                let _ = execute!(
                    std::io::stdout(),
                    crossterm::event::PushKeyboardEnhancementFlags(
                        crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    )
                );
            } else {
                let _ = execute!(
                    std::io::stdout(),
                    crossterm::event::PopKeyboardEnhancementFlags
                );
            }
        }
    }

    /// Handle resize event.
    ///
    /// Updates both local state and propagates to the connected PTY:
    /// 1. Updates `terminal_dims` for TuiRunner's own use
    /// 2. Resizes the vt100 parser so output is interpreted correctly
    /// 3. If connected, sends resize through Lua terminal subscription
    /// 4. Sends client-level resize through hub subscription for dims tracking
    fn handle_resize(&mut self, rows: u16, cols: u16) {
        self.terminal_dims = (rows, cols);

        // Clear cached widget dims so the next render + sync_widget_dims will
        // recompute and resize all parsers/PTYs to their actual widget areas.
        self.widget_dims.clear();

        // Also resize the fallback parser (used when no Lua layout is active)
        {
            let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
            parser.screen_mut().set_size(rows, cols);
        }

        // Update client-level dims via hub subscription so
        // client.lua tracks dimensions for future PTY subscriptions.
        self.send_msg(serde_json::json!({
            "subscriptionId": "tui_hub",
            "data": {
                "type": "resize",
                "rows": rows,
                "cols": cols,
            }
        }));
    }

    /// Poll PTY output and Lua events from Hub output channel.
    ///
    /// Hub sends `TuiOutput` messages through the channel: binary PTY data
    /// from Lua forwarder tasks and JSON events from `tui.send()`. TuiRunner
    /// processes them here (feeding to vt100 parser, handling Lua messages, etc.).
    fn poll_pty_events(&mut self, layout_lua: Option<&LayoutLua>) {
        use tokio::sync::mpsc::error::TryRecvError;

        // Process up to 100 events per tick
        for _ in 0..100 {
            match self.output_rx.try_recv() {
                Ok(TuiOutput::Scrollback { agent_index, pty_index, data }) => {
                    let parser = self.resolve_parser(agent_index, pty_index);
                    parser.lock().expect("parser lock poisoned").process(&data);
                    log::debug!("Processed {} bytes of scrollback", data.len());
                }
                Ok(TuiOutput::Output { agent_index, pty_index, data }) => {
                    // Scan active PTY output for Kitty keyboard protocol push/pop
                    let is_active = agent_index.unwrap_or(0) == self.current_agent_index.unwrap_or(0)
                        && pty_index.unwrap_or(0) == self.current_pty_index.unwrap_or(0);
                    if is_active {
                        if let Some(kitty_state) = scan_kitty_keyboard_state(&data) {
                            self.inner_kitty_enabled = kitty_state;
                        }
                    }
                    let parser = self.resolve_parser(agent_index, pty_index);
                    parser.lock().expect("parser lock poisoned").process(&data);
                }
                Ok(TuiOutput::ProcessExited { exit_code, .. }) => {
                    log::info!("PTY process exited with code {:?}", exit_code);
                    // Process exited - we remain connected for any final output
                }
                Ok(TuiOutput::Message(value)) => {
                    self.dispatch_hub_event(value, layout_lua);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    log::debug!("PTY output channel disconnected");
                    // Channel closed - Hub was dropped or terminated.
                    // Unsubscribe from all active subscriptions.
                    for &(agent_idx, pty_idx) in &self.active_subscriptions {
                        let sub_id = format!("tui:{}:{}", agent_idx, pty_idx);
                        self.send_msg(serde_json::json!({
                            "type": "unsubscribe",
                            "subscriptionId": sub_id,
                        }));
                    }
                    self.active_subscriptions.clear();
                    self.current_terminal_sub_id = None;
                    self.current_agent_index = None;
                    self.current_pty_index = None;
                    self.selected_agent = None;
                    break;
                }
            }
        }
    }

    /// Dispatch a hub event message through Lua events module.
    ///
    /// Extracts the event type from the message and passes it to Lua's
    /// `on_hub_event()`. If Lua returns ops, executes them. Falls back
    /// to logging for unhandled events.
    fn dispatch_hub_event(
        &mut self,
        msg: serde_json::Value,
        layout_lua: Option<&LayoutLua>,
    ) {
        let event_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

        if let Some(lua) = layout_lua {
            if lua.has_events() {
                let context = self.build_action_context();
                match lua.call_on_hub_event(event_type, &msg, &context) {
                    Ok(Some(ops)) => {
                        log::info!("Hub event '{event_type}' → {} ops", ops.len());
                        self.execute_lua_ops(ops);
                        return;
                    }
                    Ok(None) => {
                        log::debug!("Lua on_hub_event returned nil for '{event_type}'");
                        return;
                    }
                    Err(e) => {
                        log::warn!("Lua on_hub_event failed for '{event_type}': {e}");
                        return;
                    }
                }
            } else {
                log::warn!("Hub event '{event_type}' dropped: events module not loaded");
            }
        } else {
            log::warn!("Hub event '{event_type}' dropped: no layout_lua");
        }
    }

    /// Render the TUI.
    fn render(
        &mut self,
        layout_lua: Option<&LayoutLua>,
        layout_error: Option<&str>,
    ) -> Result<()> {
        use super::render::{render, RenderContext};

        // Selected agent index from current_agent_index (set by focus_terminal)
        let selected_agent_index = self.current_agent_index.unwrap_or(0);

        // Check scroll state from parser
        let (scroll_offset, is_scrolled) = {
            let parser = self.vt100_parser.lock().expect("parser lock poisoned");
            let offset = parser.screen().scrollback();
            (offset, offset > 0)
        };

        // Connection code is cached from Lua responses (requested via show_connection_code action)

        // Build render context from TuiRunner state
        let ctx = RenderContext {
            // Note: mode, list_selected, input_buffer live in Lua's _tui_state (not passed here)
            error_message: self.error_message.as_deref(),
            connection_code: self.connection_code.as_ref(),
            bundle_used: false, // TuiRunner doesn't track this - would need from Hub

            // Selection State
            selected_agent_index,

            // Terminal State - use TuiRunner's local parser
            active_parser: Some(self.parser_handle()),
            parser_pool: &self.parser_pool,
            active_pty_index: self.active_pty_index,
            scroll_offset,
            is_scrolled,

            // Status Indicators - TuiRunner doesn't track these, use defaults
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,

            // Terminal dimensions for responsive layout
            terminal_cols: self.terminal_dims.1,
            terminal_rows: self.terminal_dims.0,

            // Widget area tracking (populated during rendering)
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        // Try Lua-driven render, fall back to hardcoded Rust layout
        let lua_result = if let Some(layout_lua) = layout_lua {
            match render_with_lua(&mut self.terminal, layout_lua, &ctx) {
                Ok(result) => Some(result),
                Err(e) => {
                    log::warn!("Lua layout render failed, using fallback: {e}");
                    None
                }
            }
        } else {
            None
        };

        if lua_result.is_none() {
            render(&mut self.terminal, &ctx, None)?;
        }

        // Extract terminal areas and drop ctx (which borrows self) before mutation
        let rendered_areas = ctx.terminal_areas.borrow().clone();
        drop(ctx);

        if let Some(ref result) = lua_result {
            // Sync subscriptions to match what the render tree declares
            self.sync_subscriptions(&result.tree);

            // Track overlay presence for input routing (PTY vs keybindings)
            self.has_overlay = result.overlay.is_some();

            // Cache overlay list actions for menu selection dispatch
            self.overlay_list_actions = result
                .overlay
                .as_ref()
                .map(super::render_tree::extract_list_actions)
                .unwrap_or_default();
        }

        // Resize parsers and PTYs to match actual widget areas from the render pass
        if !rendered_areas.is_empty() {
            self.sync_widget_dims(&rendered_areas);
        }

        // Render error indicator overlay if there's a layout error
        if let Some(err_msg) = layout_error {
            render_layout_error_indicator(&mut self.terminal, err_msg)?;
        }

        Ok(())
    }

    /// Send a JSON message to Hub via the Lua client protocol.
    ///
    /// Wraps the JSON in `TuiRequest::LuaMessage` for routing through
    /// `lua.call_tui_message()` — the same `Client:on_message()` path
    /// as browser clients. Used for resize, subscriptions, agent lifecycle.
    ///
    /// For PTY keyboard input, use `handle_pty_input()` instead — it sends
    /// raw bytes via `TuiRequest::PtyInput`, bypassing Lua.
    pub(super) fn send_msg(&self, msg: serde_json::Value) {
        if let Err(e) = self.request_tx.send(TuiRequest::LuaMessage(msg)) {
            log::error!("Failed to send Lua message: {}", e);
        }
    }

    /// Execute a sequence of compound action operations from Lua.
    ///
    /// Each op is a JSON object with an `op` field and operation-specific parameters.
    /// This is the Rust side of the Lua compound action dispatch system.
    pub(super) fn execute_lua_ops(&mut self, ops: Vec<serde_json::Value>) {
        for op in ops {
            let op_name = op.get("op").and_then(|v| v.as_str()).unwrap_or("");
            match op_name {
                "set_mode" => {
                    // Shadow update only — canonical state is _tui_state.mode in Lua.
                    if let Some(mode) = op.get("mode").and_then(|v| v.as_str()) {
                        self.mode = mode.to_string();
                    }
                }
                "send_msg" => {
                    if let Some(data) = op.get("data") {
                        self.send_msg(data.clone());
                    }
                }
                "quit" => {
                    self.quit = true;
                }
                "focus_terminal" => {
                    self.execute_focus_terminal(&op);
                }
                "set_connection_code" => {
                    let url = op.get("url").and_then(|v| v.as_str());
                    let qr_ascii = op.get("qr_ascii").and_then(|v| v.as_array());

                    if let (Some(url), Some(qr_array)) = (url, qr_ascii) {
                        let qr_lines: Vec<String> = qr_array
                            .iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect();

                        let qr_width = qr_lines.first().map(|l| l.chars().count() as u16).unwrap_or(0);
                        let qr_height = qr_lines.len() as u16;
                        self.connection_code = Some(ConnectionCodeData {
                            url: url.to_string(),
                            qr_ascii: qr_lines,
                            qr_width,
                            qr_height,
                        });
                    } else {
                        log::warn!("set_connection_code op missing url or qr_ascii");
                    }
                }
                "clear_connection_code" => {
                    self.connection_code = None;
                }
                _ => {
                    log::warn!("Unknown Lua compound op: {op_name}");
                }
            }
        }
    }

    /// Execute the `focus_terminal` op — switch to a specific agent and PTY.
    ///
    /// If `agent_id` is absent/null, clears the current selection.
    /// Otherwise, looks up the agent by ID, unsubscribes from the current
    /// focused PTY, switches the parser pointer, and subscribes to the new one.
    fn execute_focus_terminal(&mut self, op: &serde_json::Value) {
        let agent_id = op.get("agent_id").and_then(|v| v.as_str());
        let pty_index = op.get("pty_index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let agent_index = op.get("agent_index").and_then(|v| v.as_u64()).map(|v| v as usize);
        log::info!(
            "focus_terminal: agent_id={:?}, agent_index={:?}, pty_index={}",
            agent_id, agent_index, pty_index
        );

        // Clear selection if no agent_id
        let Some(agent_id) = agent_id else {
            // Unsubscribe from current focused PTY
            if let Some(ref sub_id) = self.current_terminal_sub_id {
                self.send_msg(serde_json::json!({
                    "type": "unsubscribe",
                    "subscriptionId": sub_id,
                }));
                if let (Some(ai), Some(pi)) = (self.current_agent_index, self.current_pty_index) {
                    self.active_subscriptions.remove(&(ai, pi));
                }
            }
            self.selected_agent = None;
            self.current_agent_index = None;
            self.current_pty_index = None;
            self.current_terminal_sub_id = None;
            return;
        };

        // Agent index provided by Lua (computed from _tui_state.agents)
        let Some(index) = agent_index else {
            log::warn!("focus_terminal: missing agent_index for agent {agent_id}");
            return;
        };

        // Skip if already focused on same agent + pty
        if self.current_agent_index == Some(index) && self.current_pty_index == Some(pty_index) {
            log::debug!("focus_terminal: already focused on agent {agent_id} pty {pty_index}");
            return;
        }

        // Unsubscribe from current focused PTY
        if let Some(ref sub_id) = self.current_terminal_sub_id {
            self.send_msg(serde_json::json!({
                "type": "unsubscribe",
                "subscriptionId": sub_id,
            }));
            if let (Some(ai), Some(pi)) = (self.current_agent_index, self.current_pty_index) {
                self.active_subscriptions.remove(&(ai, pi));
            }
        }

        // Point vt100_parser at the pool entry for the target PTY
        let (rows, cols) = self.terminal_dims;
        let parser = self.parser_pool
            .entry((index, pty_index))
            .or_insert_with(|| Arc::new(Mutex::new(Parser::new(rows, cols, DEFAULT_SCROLLBACK))))
            .clone();
        self.vt100_parser = parser;

        // Subscribe to new PTY via Lua protocol
        let sub_id = format!("tui:{}:{}", index, pty_index);
        self.send_msg(serde_json::json!({
            "type": "subscribe",
            "channel": "terminal",
            "subscriptionId": sub_id,
            "params": {
                "agent_index": index,
                "pty_index": pty_index,
            }
        }));

        // Update state
        self.selected_agent = Some(agent_id.to_string());
        self.current_agent_index = Some(index);
        self.current_pty_index = Some(pty_index);
        self.active_pty_index = pty_index;
        self.current_terminal_sub_id = Some(sub_id);
        self.active_subscriptions.insert((index, pty_index));
    }

}

/// Result from Lua layout rendering.
struct LuaRenderResult {
    /// Main layout tree (used for subscription sync).
    tree: super::render_tree::RenderNode,
    /// Optional overlay tree (used for action extraction).
    overlay: Option<super::render_tree::RenderNode>,
}

/// Render using the Lua layout engine (free function to avoid borrow conflicts).
///
/// Calls Lua `render(state)` and `render_overlay(state)`, interprets
/// the returned render trees into ratatui calls. Returns both trees
/// so the caller can sync subscriptions and extract overlay actions.
fn render_with_lua<B>(
    terminal: &mut Terminal<B>,
    layout_lua: &LayoutLua,
    ctx: &super::render::RenderContext,
) -> Result<LuaRenderResult>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    use super::render_tree::interpret_tree;

    // Get main layout tree from Lua
    let tree = layout_lua.call_render(ctx)?;

    // Get optional overlay tree from Lua
    let overlay = layout_lua.call_render_overlay(ctx)?;

    // Render to terminal
    terminal.draw(|f| {
        let area = f.area();
        interpret_tree(&tree, f, ctx, area);

        if let Some(ref overlay_tree) = overlay {
            interpret_tree(overlay_tree, f, ctx, area);
        }
    })?;

    Ok(LuaRenderResult { tree, overlay })
}

/// Render a dim error indicator in the bottom-right corner of the terminal.
///
/// Overlaid on top of whatever was already rendered. Shows layout errors
/// so the user knows the Lua layout has issues.
fn render_layout_error_indicator<B>(terminal: &mut Terminal<B>, error_msg: &str) -> Result<()>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;
    use ratatui::widgets::Paragraph;

    terminal.draw(|f| {
        let area = f.area();
        let text = format!(" [Layout: {error_msg}] ");
        let width = text.len() as u16;

        // Position in bottom-right corner
        if area.width >= width && area.height >= 1 {
            let indicator_area = ratatui::layout::Rect::new(
                area.x + area.width - width,
                area.y + area.height - 1,
                width,
                1,
            );
            let indicator = Paragraph::new(Span::styled(
                text,
                Style::default().add_modifier(Modifier::DIM),
            ));
            f.render_widget(indicator, indicator_area);
        }
    })?;

    Ok(())
}

/// Truncate an error message to a maximum length, adding ellipsis if needed.
fn truncate_error(msg: &str, max_len: usize) -> String {
    let trimmed = msg.lines().next().unwrap_or(msg);
    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..max_len.saturating_sub(3)])
    }
}

/// Run the TUI alongside a Hub.
///
/// This is the main entry point for TUI mode. It spawns TuiRunner in a
/// dedicated thread to handle terminal I/O, while the main thread runs
/// Hub operations (browser events, polling, heartbeats).
///
/// # Architecture
///
/// ```text
/// Main Thread                    TUI Thread
/// +------------------+           +------------------+
/// | Hub tick loop    |           | TuiRunner        |
/// | - Browser events |<--events--| - Input handling |
/// | - Polling        |---cmds--->| - Rendering      |
/// | - Heartbeats     |           | - PTY output     |
/// +------------------+           +------------------+
/// ```
///
/// TuiRunner owns all TUI state (mode, menus, selections) and the terminal.
/// Hub handles non-TUI concerns: browser relay, server polling, heartbeats.
///
/// # Arguments
///
/// * `hub` - The Hub instance to run
/// * `terminal` - The ratatui terminal (ownership transferred to TuiRunner)
/// * `shutdown_flag` - Atomic flag for external shutdown requests (signals)
///
/// # Errors
///
/// Returns an error if terminal operations or thread spawning fails.
pub fn run_with_hub(
    hub: &mut Hub,
    terminal: Terminal<CrosstermBackend<Stdout>>,
    shutdown_flag: &AtomicBool,
) -> Result<()> {
    log::info!("Hub event loop starting (TUI mode)");

    // Calculate initial terminal dimensions for PTY sizing
    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let (inner_rows, inner_cols) = terminal_widget_inner_area(term_cols, term_rows);
    let terminal_dims = (inner_rows, inner_cols);

    log::info!(
        "Terminal: {}x{} -> PTY inner area: {}x{}",
        term_cols,
        term_rows,
        inner_cols,
        inner_rows
    );

    // Create channel for TuiRunner -> Hub communication
    let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<TuiRequest>();

    // Register TUI via Lua for Hub-side request processing.
    // Hub processes JSONs directly in its tick loop.
    let output_rx = hub.register_tui_via_lua(request_rx);

    let shutdown = Arc::new(AtomicBool::new(false));
    let tui_shutdown = Arc::clone(&shutdown);

    let mut tui_runner = TuiRunner::new(
        terminal,
        request_tx,
        output_rx,
        tui_shutdown,
        terminal_dims,
    );

    // Load Lua layout source: try filesystem first, then embedded.
    // Filesystem allows hot-reload in dev; embedded is the release fallback.
    if let Some(layout) = load_layout_lua_source() {
        tui_runner.set_layout_lua_source(layout.source);
        tui_runner.layout_lua_fs_path = layout.fs_path;
    }

    // Load Lua keybinding source alongside layout.
    if let Some(kb) = load_keybinding_lua_source() {
        tui_runner.set_keybinding_lua_source(kb.source);
        tui_runner.keybinding_lua_fs_path = kb.fs_path;
    }

    // Load Lua actions source for compound action dispatch.
    if let Some(actions) = load_actions_lua_source() {
        tui_runner.set_actions_lua_source(actions.source);
        tui_runner.actions_lua_fs_path = actions.fs_path;
    }

    // Load Lua events source for hub event handling.
    if let Some(events) = load_events_lua_source() {
        tui_runner.set_events_lua_source(events.source);
        tui_runner.events_lua_fs_path = events.fs_path;
    }

    // Load botster API and discover UI extensions (plugins + user overrides).
    tui_runner.botster_api_source = load_botster_api_source();
    let lua_base = resolve_lua_user_path();
    let mut extensions = discover_ui_extensions(&lua_base);
    // User UI overrides (~/.botster/lua/ui/) layer on top of built-in modules.
    // They redefine only the functions they want to customize.
    extensions.extend(discover_user_ui_overrides());
    tui_runner.extension_sources = extensions;

    // Register SIGWINCH to set the resize flag (TuiRunner polls this each tick)
    #[cfg(unix)]
    {
        use signal_hook::consts::signal::SIGWINCH;
        if let Err(e) =
            signal_hook::flag::register(SIGWINCH, Arc::clone(&tui_runner.resize_flag))
        {
            log::warn!("Failed to register SIGWINCH handler: {e}");
        }
    }

    // Spawn TUI thread
    let tui_handle = thread::Builder::new()
        .name("tui-runner".to_string())
        .spawn(move || {
            if let Err(e) = tui_runner.run() {
                log::error!("TuiRunner error: {}", e);
            }
        })?;

    log::info!("TuiRunner spawned in dedicated thread");

    // Main thread: Hub tick loop for non-TUI operations.
    // Client request processing is handled by each client's async run_task().
    while !hub.quit && !shutdown_flag.load(Ordering::SeqCst) {
        // Periodic tasks (command channel, heartbeat, Lua queues, notifications)
        hub.tick();

        // Small sleep to prevent CPU spinning (60 FPS max)
        thread::sleep(Duration::from_millis(16));
    }

    // Signal TUI thread to shutdown
    shutdown.store(true, Ordering::SeqCst);

    // Wait for TUI thread to finish
    log::info!("Waiting for TuiRunner thread to finish...");
    if let Err(e) = tui_handle.join() {
        log::error!("TuiRunner thread panicked: {:?}", e);
    }

    log::info!("Hub event loop exiting");
    Ok(())
}

/// Scan PTY output bytes for Kitty keyboard protocol push/pop sequences.
///
/// Scan PTY output bytes for Kitty keyboard protocol push/pop sequences.
///
/// Returns `Some(true)` if the last relevant sequence is a push (`CSI > flags u`),
/// `Some(false)` if it's a pop (`CSI < u`), or `None` if no Kitty sequences found.
///
/// We scan for the *last* occurrence because a single output chunk could contain
/// multiple push/pop sequences (e.g., during shell startup).
fn scan_kitty_keyboard_state(data: &[u8]) -> Option<bool> {
    let mut result = None;

    // Scan for ESC [ > ... u (push) and ESC [ < ... u (pop)
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0x1b && data[i + 1] == b'[' {
            let start = i + 2;
            if start < data.len() && data[start] == b'>' {
                // Potential push: ESC [ > <digits> u
                let mut j = start + 1;
                while j < data.len() && data[j].is_ascii_digit() {
                    j += 1;
                }
                if j < data.len() && data[j] == b'u' {
                    result = Some(true);
                    i = j + 1;
                    continue;
                }
            } else if start < data.len() && data[start] == b'<' {
                // Potential pop: ESC [ < u  (or ESC [ < <digits> u)
                let mut j = start + 1;
                while j < data.len() && data[j].is_ascii_digit() {
                    j += 1;
                }
                if j < data.len() && data[j] == b'u' {
                    result = Some(false);
                    i = j + 1;
                    continue;
                }
            }
        }
        i += 1;
    }

    result
}

/// Result of loading Lua layout source.
struct LayoutSource {
    /// The Lua source code.
    source: String,
    /// Filesystem path if loaded from disk (for hot-reload watching).
    /// None if loaded from embedded (no watching needed).
    fs_path: Option<std::path::PathBuf>,
}

/// A UI extension source loaded from a plugin or user directory.
#[derive(Debug)]
struct ExtensionSource {
    /// Lua source code.
    source: String,
    /// Human-readable name for error messages (e.g., "plugin:my-plugin/layout").
    name: String,
    /// Filesystem path for hot-reload watching.
    fs_path: std::path::PathBuf,
}

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
    let local = std::path::PathBuf::from("lua").join(&rel_path);
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
/// For example, a user `layout.lua` containing only `function render_overlay(state) ... end`
/// overrides just the overlay while `render()` stays built-in.
fn discover_user_ui_overrides() -> Vec<ExtensionSource> {
    let mut overrides = Vec::new();
    let ui_dir = match dirs::home_dir() {
        Some(home) => home.join(".botster").join("lua").join("ui"),
        None => return overrides,
    };

    if let Ok(entries) = std::fs::read_dir(&ui_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "lua") {
                if let Ok(source) = std::fs::read_to_string(&path) {
                    let name = path.file_stem()
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

fn load_layout_lua_source() -> Option<LayoutSource> {
    load_lua_ui_source("layout.lua")
}

fn load_keybinding_lua_source() -> Option<LayoutSource> {
    load_lua_ui_source("keybindings.lua")
}

fn load_actions_lua_source() -> Option<LayoutSource> {
    load_lua_ui_source("actions.lua")
}

fn load_events_lua_source() -> Option<LayoutSource> {
    load_lua_ui_source("events.lua")
}

fn load_botster_api_source() -> Option<String> {
    load_lua_ui_source("botster.lua").map(|s| s.source)
}

/// Discover UI extension files from plugins and user directories.
///
/// Returns extensions in load order:
/// 1. Plugin `ui/` files (alphabetical by plugin name)
/// 2. User `~/.botster/lua/user/ui/` files (highest priority)
fn discover_ui_extensions(lua_base: &std::path::Path) -> Vec<ExtensionSource> {
    let mut extensions = Vec::new();
    let ui_files = ["layout.lua", "keybindings.lua", "actions.lua"];

    // Plugin UI extensions: ~/.botster/plugins/*/ui/{layout,keybindings,actions}.lua
    // lua_base is ~/.botster/lua, plugins are at ~/.botster/plugins
    let plugins_dir = lua_base
        .parent()
        .unwrap_or(lua_base)
        .join("plugins");

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

/// Resolve candidate Lua base paths for loading UI modules.
///
/// Returns paths in priority order — loaders check each until the file is
/// found. This allows `~/.botster/lua/` overrides to coexist with dev
/// defaults in `./lua/`, like Neovim's runtimepath.
///
/// Resolve the user-level Lua path (`~/.botster/lua/`).
///
/// Used for discovering extensions and user overrides — not for loading
/// core UI modules (which use `resolve_lua_search_paths`).
fn resolve_lua_user_path() -> std::path::PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".botster").join("lua"))
        .unwrap_or_else(|| std::path::PathBuf::from(".botster/lua"))
}

#[cfg(test)]
mod tests {
    //! TuiRunner tests - comprehensive end-to-end tests through the input chain.
    //!
    //! # Test Philosophy
    //!
    //! Tests in this module exercise real code paths via:
    //! 1. Keyboard events through Lua `handle_key()` -> `handle_raw_input_event()` -> `handle_tui_action()`
    //! 2. Verification of commands sent through channels
    //! 3. Real PTY event polling through `poll_pty_events()`
    //!
    //! # Test Infrastructure
    //!
    //! We use two test patterns:
    //!
    //! 1. **`create_test_runner()`**: Simple tests that don't need Hub responses.
    //!    Uses mock channels where operations gracefully fail.
    //!
    //! 2. **`create_test_runner_with_mock_client()`**: Integration tests that need
    //!    request verification. Spawns a responder thread that passthroughs all
    //!    `JSON` messages for inspection. Application state (agents, worktrees)
    //!    lives in Lua's `_tui_state` — set it via `lua.exec()` in tests.
    //!
    //! # M-DESIGN-FOR-AI Compliance
    //!
    //! Tests follow MS Rust guidelines with canonical documentation format.

    use super::*;
    use crate::client::{CreateAgentRequest, DeleteAgentRequest};
    use ratatui::backend::TestBackend;
    use tokio::sync::mpsc;

    // =========================================================================
    // Test Infrastructure
    // =========================================================================

    /// Creates a `TuiRunner` with a `TestBackend` for unit testing.
    ///
    /// Returns the runner and request receiver. The receiver allows verifying
    /// what requests were sent without an actual Hub.
    ///
    /// # Note
    ///
    /// This setup does NOT respond to messages. Application state lives in
    /// Lua's `_tui_state` — use `process_key_with_lua()` for tests needing it.
    /// Use `create_test_runner_with_mock_client` for flows requiring a
    /// responder thread.
    fn create_test_runner() -> (TuiRunner<TestBackend>, mpsc::UnboundedReceiver<TuiRequest>) {
        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).expect("Failed to create test terminal");

        let (request_tx, request_rx) = mpsc::unbounded_channel::<TuiRequest>();

        // Create output channel (Hub would send here, but we don't have one in tests)
        let (_output_tx, output_rx) = tokio::sync::mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));

        let mut runner = TuiRunner::new(
            terminal,
            request_tx,
            output_rx,
            shutdown,
            (24, 80), // rows, cols
        );

        // Initialize mode from Lua (same as production boot path)
        let lua = make_test_layout_with_keybindings();
        runner.mode = lua.call_initial_mode();

        (runner, request_rx)
    }

    /// Creates a `TuiRunner` with a mock Hub responder for testing.
    ///
    /// Spawns a responder thread that passthroughs all `JSON` messages
    /// for verification. Application state (agents, worktrees) lives in
    /// Lua's `_tui_state` — set it via `lua.exec()` in tests.
    ///
    /// # Returns
    ///
    /// - `TuiRunner` connected to mock Hub
    /// - `mpsc::UnboundedSender<TuiOutput>` for delivering Lua events to TuiRunner
    /// - `mpsc::UnboundedReceiver` for inspecting requests sent by TuiRunner
    /// - `Arc<AtomicBool>` to signal shutdown to the responder thread
    fn create_test_runner_with_mock_client() -> (
        TuiRunner<TestBackend>,
        mpsc::UnboundedSender<TuiOutput>,
        mpsc::UnboundedReceiver<TuiRequest>,
        Arc<AtomicBool>,
    ) {
        // Create our own request channel that we control
        // TuiRunner sends requests here, and the responder handles them
        let (request_tx, mut request_rx) = mpsc::unbounded_channel::<TuiRequest>();
        let (passthrough_tx, passthrough_rx) = mpsc::unbounded_channel::<TuiRequest>();

        let shutdown = Arc::new(AtomicBool::new(false));
        let responder_shutdown = Arc::clone(&shutdown);

        // Spawn request responder thread that passthroughs all messages
        // for test verification.
        thread::spawn(move || {
            while !responder_shutdown.load(Ordering::Relaxed) {
                match request_rx.try_recv() {
                    Ok(request) => {
                        let _ = passthrough_tx.send(request);
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
        });

        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).expect("Failed to create test terminal");

        // Create output channel for TuiOutput delivery to TuiRunner
        let (output_tx, output_rx) = tokio::sync::mpsc::unbounded_channel();

        let mut runner = TuiRunner::new(
            terminal,
            request_tx,
            output_rx,
            Arc::clone(&shutdown),
            (24, 80),
        );

        // Initialize mode from Lua (same as production boot path)
        let lua = make_test_layout_with_keybindings();
        runner.mode = lua.call_initial_mode();

        (runner, output_tx, passthrough_rx, shutdown)
    }

    /// Creates an `InputEvent::Key` for a plain character.
    fn make_key_char(c: char) -> InputEvent {
        InputEvent::Key {
            descriptor: c.to_string(),
            raw_bytes: c.to_string().into_bytes(),
        }
    }

    /// Creates an `InputEvent::Key` for a special key by descriptor.
    fn make_key_desc(descriptor: &str, raw_bytes: &[u8]) -> InputEvent {
        InputEvent::Key {
            descriptor: descriptor.to_string(),
            raw_bytes: raw_bytes.to_vec(),
        }
    }

    /// Creates an `InputEvent::Key` for Enter.
    fn make_key_enter() -> InputEvent {
        make_key_desc("enter", b"\r")
    }

    /// Creates an `InputEvent::Key` for Escape.
    fn make_key_escape() -> InputEvent {
        make_key_desc("escape", &[0x1b])
    }

    /// Creates an `InputEvent::Key` for Up arrow.
    fn make_key_up() -> InputEvent {
        make_key_desc("up", &[0x1b, b'[', b'A'])
    }

    /// Creates an `InputEvent::Key` for Down arrow.
    fn make_key_down() -> InputEvent {
        make_key_desc("down", &[0x1b, b'[', b'B'])
    }

    /// Creates an `InputEvent::Key` for Ctrl+<char>.
    fn make_key_ctrl(c: char) -> InputEvent {
        let ctrl_byte = (c.to_ascii_uppercase() as u8).wrapping_sub(b'@');
        InputEvent::Key {
            descriptor: format!("ctrl+{}", c.to_ascii_lowercase()),
            raw_bytes: vec![ctrl_byte],
        }
    }

    /// Extract the JSON value from a `TuiRequest::LuaMessage`.
    ///
    /// Panics if the request is not a `LuaMessage` variant.
    fn unwrap_lua_msg(request: TuiRequest) -> serde_json::Value {
        match request {
            TuiRequest::LuaMessage(msg) => msg,
            other => panic!("Expected LuaMessage, got {other:?}"),
        }
    }

    /// Creates a `LayoutLua` with keybindings and actions loaded from actual files.
    fn make_test_layout_with_keybindings() -> LayoutLua {
        let layout_source = "function render(s) return { type = 'empty' } end\nfunction render_overlay(s) return nil end\nfunction initial_mode() return 'normal' end";
        let kb_source = include_str!("../../lua/ui/keybindings.lua");
        let actions_source = include_str!("../../lua/ui/actions.lua");
        let events_source = include_str!("../../lua/ui/events.lua");
        let mut lua = LayoutLua::new(layout_source).expect("test layout should load");
        // Bootstrap _tui_state (actions.lua and events.lua read from it)
        lua.load_extension(
            "_tui_state = _tui_state or { agents = {}, pending_fields = {}, available_worktrees = {}, mode = 'normal', input_buffer = '', list_selected = 0 }",
            "_tui_state_init",
        ).expect("_tui_state bootstrap should succeed");
        lua.load_keybindings(kb_source)
            .expect("test keybindings should load");
        lua.load_actions(actions_source)
            .expect("test actions should load");
        lua.load_events(events_source)
            .expect("test events should load");
        lua
    }

    /// Processes a keyboard event through the full Lua-driven input pipeline.
    ///
    /// This exercises: `InputEvent` -> `handle_raw_input_event()` with Lua keybindings.
    /// Uses a fresh LayoutLua per call — suitable for simple single-key tests.
    fn process_key(runner: &mut TuiRunner<TestBackend>, event: InputEvent) {
        let lua = make_test_layout_with_keybindings();
        runner.handle_raw_input_event(event, Some(&lua));
    }

    /// Processes a keyboard event with a persistent LayoutLua.
    ///
    /// For multi-step e2e tests that need `_tui_state` to persist between keys.
    fn process_key_with_lua(runner: &mut TuiRunner<TestBackend>, event: InputEvent, lua: &LayoutLua) {
        runner.handle_raw_input_event(event, Some(lua));
    }

    // =========================================================================
    // Display & Property Tests
    // =========================================================================

    /// Verifies dynamic menu builds correctly for different contexts.
    ///
    /// Tests that the menu structure adapts based on context (agent selected,
    /// server PTY available, etc.) and that actions can be correctly retrieved
    /// by selection index.
    /// Stub overlay_list_actions with a test fixture.
    ///
    /// In production, Lua renders the menu overlay and Rust extracts action
    /// strings from the render tree. Tests don't run Lua, so we stub the
    /// cache with a fixed set of actions. This is pure test fixture data —
    /// Lua owns the real menu content.
    fn stub_menu_actions(runner: &mut TuiRunner<TestBackend>) {
        runner.overlay_list_actions = vec![
            "new_agent".to_string(),
            "show_connection_code".to_string(),
        ];
    }

    /// Find the index of an action string in overlay_list_actions.
    fn find_action_index(runner: &TuiRunner<TestBackend>, action: &str) -> Option<usize> {
        runner.overlay_list_actions.iter().position(|a| a == action)
    }

    /// Navigate to a specific menu selection index from index 0.
    fn navigate_to_menu_index_with_lua(
        runner: &mut TuiRunner<TestBackend>,
        lua: &LayoutLua,
        target_idx: usize,
    ) {
        for _ in 0..target_idx {
            process_key_with_lua(runner, make_key_down(), lua);
        }
    }

    /// Read `_tui_state.list_selected` from Lua.
    fn lua_list_selected(lua: &LayoutLua) -> usize {
        lua.eval_usize("return _tui_state.list_selected").unwrap_or(0)
    }

    /// Read `_tui_state.input_buffer` from Lua.
    fn lua_input_buffer(lua: &LayoutLua) -> String {
        lua.eval_string("return _tui_state.input_buffer").unwrap_or_default()
    }

    // =========================================================================
    // Builder Pattern Tests
    // =========================================================================

    /// Verifies `DeleteAgentRequest` builder patterns.
    #[test]
    fn test_delete_agent_request_builder() {
        // Without worktree deletion
        let req = DeleteAgentRequest::new("agent-1");
        assert_eq!(req.agent_id, "agent-1");
        assert!(!req.delete_worktree);

        // With worktree deletion
        let req = DeleteAgentRequest::new("agent-2").with_worktree_deletion();
        assert_eq!(req.agent_id, "agent-2");
        assert!(req.delete_worktree);
    }

    /// Verifies `CreateAgentRequest` builder patterns.
    #[test]
    fn test_create_agent_request_builder() {
        // Basic creation
        let req = CreateAgentRequest::new("issue-42");
        assert_eq!(req.issue_or_branch, "issue-42");
        assert!(req.prompt.is_none());
        assert!(req.from_worktree.is_none());

        // With prompt
        let req = CreateAgentRequest::new("issue-42").with_prompt("Fix the bug");
        assert_eq!(req.prompt, Some("Fix the bug".to_string()));

        // From existing worktree
        let path = std::path::PathBuf::from("/path/to/worktree");
        let req = CreateAgentRequest::new("branch-name").from_worktree(path.clone());
        assert_eq!(req.from_worktree, Some(path));
    }

    // =========================================================================
    // PTY Hot Path Tests
    // =========================================================================
    //
    // These tests verify the critical output path:
    // PTY broadcast -> poll_pty_events() -> vt100_parser.process()

    /// Verifies PTY output is fed to the VT100 parser.

    /// Verifies multiple PTY outputs are processed in sequence.

    /// Verifies `poll_pty_events()` is safe without a connected PTY.

    /// Verifies polling an empty channel does not block.

    /// Verifies disconnect is handled gracefully.

    /// Verifies non-output events do not affect parser content.

    /// Verifies mixed output and non-output events are handled correctly.

    // =========================================================================
    // E2E Menu Flow Tests - Full Keyboard Input Chain
    // =========================================================================

    /// Verifies Ctrl+P opens the menu from Normal mode.
    #[test]
    fn test_e2e_ctrl_p_opens_menu() {
        let (mut runner, _cmd_rx) = create_test_runner();

        assert_eq!(runner.mode(), "normal");

        process_key(&mut runner, make_key_ctrl('p'));

        assert_eq!(runner.mode(), "menu", "Ctrl+P should open menu");
    }

    /// Verifies menu navigation with arrow keys.
    #[test]
    fn test_e2e_menu_arrow_navigation() {
        let (mut runner, _cmd_rx) = create_test_runner();
        let lua = make_test_layout_with_keybindings();

        // Open menu
        process_key_with_lua(&mut runner, make_key_ctrl('p'), &lua);
        assert_eq!(runner.mode(), "menu");
        assert_eq!(lua_list_selected(&lua), 0);
        stub_menu_actions(&mut runner);

        // Navigate down (menu has 2 items: new_agent, show_connection_code)
        process_key_with_lua(&mut runner, make_key_down(), &lua);
        assert_eq!(lua_list_selected(&lua), 1);

        // Should clamp at max (1)
        process_key_with_lua(&mut runner, make_key_down(), &lua);
        assert_eq!(lua_list_selected(&lua), 1);

        // Navigate up
        process_key_with_lua(&mut runner, make_key_up(), &lua);
        assert_eq!(lua_list_selected(&lua), 0);

        // Close with Escape
        process_key_with_lua(&mut runner, make_key_escape(), &lua);
        assert_eq!(runner.mode(), "normal");
    }

    /// Verifies menu up does not go below zero.
    #[test]
    fn test_e2e_menu_up_clamps_at_zero() {
        let (mut runner, _cmd_rx) = create_test_runner();
        let lua = make_test_layout_with_keybindings();

        process_key_with_lua(&mut runner, make_key_ctrl('p'), &lua);
        assert_eq!(lua_list_selected(&lua), 0);

        process_key_with_lua(&mut runner, make_key_up(), &lua);
        assert_eq!(lua_list_selected(&lua), 0, "Should not go below 0");
    }

    /// Verifies menu number shortcuts select items directly.
    ///
    /// Number shortcuts are 1-indexed (matching the display) and map to
    /// selectable items (0-indexed internally). For example:
    /// - Pressing '1' selects selectable index 0
    /// - Pressing '2' selects selectable index 1
    ///
    /// The actual action at each index depends on the dynamic menu context.
    #[test]
    fn test_e2e_menu_number_shortcuts() {
        let (mut runner, _cmd_rx) = create_test_runner();
        let lua = make_test_layout_with_keybindings();

        // Open menu (no agent selected)
        process_key_with_lua(&mut runner, make_key_ctrl('p'), &lua);
        assert_eq!(runner.mode(), "menu");
        stub_menu_actions(&mut runner);

        // Find what action is at index 1 (which corresponds to pressing '2')
        let action_at_1 = find_action_index(&runner, "show_connection_code");
        assert_eq!(
            action_at_1,
            Some(1),
            "show_connection_code should be at index 1 when no agent selected"
        );

        // Press '2' to select the item at index 1
        process_key_with_lua(&mut runner, make_key_char('2'), &lua);

        assert_eq!(
            runner.mode(),
            "connection_code",
            "Number shortcut '2' should select ShowConnectionCode"
        );
    }

    /// Verifies Ctrl+Q triggers quit.
    #[test]
    fn test_e2e_ctrl_q_quits() {
        let (mut runner, _cmd_rx) = create_test_runner();

        assert!(!runner.quit);

        process_key(&mut runner, make_key_ctrl('q'));

        assert!(runner.quit, "Ctrl+Q should set quit flag");
    }

    /// Verifies plain keys in Normal mode go to PTY (via Lua returning nil).
    #[test]
    fn test_e2e_normal_mode_keys_go_to_pty() {
        let lua = make_test_layout_with_keybindings();
        let context = KeyContext::default();

        // Plain 'q' should NOT match any binding in normal mode -> nil -> PTY
        let result = lua.call_handle_key("q", "normal", &context).unwrap();
        assert!(result.is_none(), "Plain 'q' should return nil (PTY forward)");

        // Plain 'p' should NOT match any binding in normal mode -> nil -> PTY
        let result = lua.call_handle_key("p", "normal", &context).unwrap();
        assert!(result.is_none(), "Plain 'p' should return nil (PTY forward)");
    }

    // =========================================================================
    // E2E Connection Code Flow Tests
    // =========================================================================

    /// Verifies complete connection code flow: menu -> select -> regenerate -> close.
    ///
    /// Uses `find_menu_action_index` to dynamically locate the Connection Code action,
    /// ensuring this test works regardless of menu structure changes.
    ///
    /// Verifies complete connection code flow: menu -> select -> close.
    ///
    /// Uses `find_menu_action_index` to dynamically locate the Connection Code action,
    /// ensuring this test works regardless of menu structure changes.
    ///
    /// NOTE: This test uses `create_test_runner()` with mock channels. The 'r'
    /// key (regenerate) is tested separately in `test_regenerate_connection_code_resets_qr_flag`.
    /// We don't test 'r' here because the mock channels return an error (which is
    /// handled gracefully), and we want this E2E test to focus on the UI flow, not
    /// the refresh behavior.
    #[test]
    fn test_e2e_connection_code_full_flow() {
        let (mut runner, _cmd_rx) = create_test_runner();
        let lua = make_test_layout_with_keybindings();

        // 1. Open menu
        process_key_with_lua(&mut runner, make_key_ctrl('p'), &lua);
        assert_eq!(runner.mode(), "menu");
        stub_menu_actions(&mut runner);

        // 2. Find and navigate to Connection Code using cached overlay actions
        let connection_idx = find_action_index(&runner, "show_connection_code")
            .expect("show_connection_code should be in menu");
        navigate_to_menu_index_with_lua(&mut runner, &lua, connection_idx);
        assert_eq!(lua_list_selected(&lua), connection_idx);

        // 3. Select with Enter
        process_key_with_lua(&mut runner, make_key_enter(), &lua);
        assert_eq!(runner.mode(), "connection_code");

        // 4. Close with Escape
        process_key_with_lua(&mut runner, make_key_escape(), &lua);
        assert_eq!(runner.mode(), "normal");
    }

    // =========================================================================
    // E2E Agent Deletion Flow Tests
    // =========================================================================

    /// Verifies close agent flow: menu -> confirm -> Y (keep worktree).
    ///
    /// Uses `find_menu_action_index` to dynamically locate the Close Agent action.

    /// Verifies close agent flow: confirm -> D (delete worktree).

    /// Verifies close agent cancel with Escape.

    /// Verifies close agent is not available without selected agent.
    ///
    /// When no agent is selected, the CloseAgent menu item doesn't appear in the
    /// dynamic menu. This test verifies that the dynamic menu correctly omits
    /// CloseAgent when no agent is selected.

    // =========================================================================
    // E2E Agent Creation Flow Tests
    // =========================================================================

    /// Verifies full agent creation flow: menu -> worktree select -> issue input -> prompt -> create.
    ///
    /// This test uses a mock Hub responder for controlled responses.
    /// It verifies both the request-sending path AND the event-receiving path.
    ///
    /// # Test Strategy
    ///
    /// 1. Controlled JSON responses for deterministic tests
    /// 2. Verifies request is sent with correct parameters
    /// 3. Sends AgentCreated event via TuiOutput channel to verify TUI transitions
    #[test]
    fn test_e2e_new_agent_full_flow() {
        let (mut runner, _output_tx, mut request_rx, shutdown) = create_test_runner_with_mock_client();
        let lua = make_test_layout_with_keybindings();

        // 1. Open menu and navigate to New Agent using cached overlay actions
        process_key_with_lua(&mut runner, make_key_ctrl('p'), &lua);
        stub_menu_actions(&mut runner);

        let new_agent_idx = find_action_index(&runner, "new_agent")
            .expect("new_agent should be in menu");
        navigate_to_menu_index_with_lua(&mut runner, &lua, new_agent_idx);
        assert_eq!(lua_list_selected(&lua), new_agent_idx);

        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        // Small delay to let responder process messages
        thread::sleep(Duration::from_millis(10));

        assert_eq!(
            runner.mode(),
            "new_agent_select_worktree",
            "Should enter worktree selection"
        );

        // 2. Select "Create new worktree" (index 1, after "Use Main Branch")
        process_key_with_lua(&mut runner, make_key_down(), &lua);
        assert_eq!(lua_list_selected(&lua), 1);
        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        assert_eq!(runner.mode(), "new_agent_create_worktree");

        // 3. Type issue name
        for c in "issue-42".chars() {
            process_key_with_lua(&mut runner, make_key_char(c), &lua);
        }
        assert_eq!(lua_input_buffer(&lua), "issue-42");

        // 4. Submit issue name
        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        assert_eq!(runner.mode(), "new_agent_prompt");
        // pending_fields now live in Lua's _tui_state, verified by actions.lua tests

        // 5. Type prompt and submit
        for c in "Fix bug".chars() {
            process_key_with_lua(&mut runner, make_key_char(c), &lua);
        }
        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        // Wait for responder to process
        thread::sleep(Duration::from_millis(10));

        // Verify create_agent JSON message (skip list_worktrees request)
        let mut found_create = false;
        while let Ok(req) = request_rx.try_recv() {
            let msg = unwrap_lua_msg(req);
            if let Some(data) = msg.get("data") {
                if data.get("type").and_then(|t| t.as_str()) == Some("create_agent") {
                    assert_eq!(
                        data.get("issue_or_branch").and_then(|v| v.as_str()),
                        Some("issue-42")
                    );
                    assert_eq!(
                        data.get("prompt").and_then(|v| v.as_str()),
                        Some("Fix bug")
                    );
                    found_create = true;
                    break;
                }
            }
        }
        assert!(found_create, "create_agent JSON message should be sent");

        // Modal closes after submit — stays in normal mode until agent_created
        // event arrives and selects the agent (which sets insert mode).
        assert_eq!(
            runner.mode(),
            "normal",
            "Should be normal mode until agent_created event selects the agent"
        );

        // Cleanup
        shutdown.store(true, Ordering::Relaxed);
    }

    /// Verifies selecting an existing worktree skips prompt and creates agent immediately.
    ///
    /// This test uses a mock Hub responder for controlled responses.
    /// It verifies the full flow from worktree selection through agent creation.
    ///
    /// # Test Strategy
    ///
    /// 1. Controlled JSON responses for deterministic tests
    /// 2. Verifies request includes from_worktree path
    #[test]
    fn test_e2e_reopen_existing_worktree() {
        let (mut runner, _output_tx, mut request_rx, shutdown) = create_test_runner_with_mock_client();
        let lua = make_test_layout_with_keybindings();

        // Pre-populate worktrees in _tui_state (normally delivered via worktree_list event)
        lua.load_extension(
            r#"_tui_state.available_worktrees = {
                { path = "/path/worktree-1", branch = "feature-branch" },
                { path = "/path/worktree-2", branch = "bugfix-branch" },
            }"#,
            "test_worktrees",
        ).unwrap();

        // Open menu and navigate to New Agent using cached overlay actions
        process_key_with_lua(&mut runner, make_key_ctrl('p'), &lua);
        stub_menu_actions(&mut runner);

        let new_agent_idx = find_action_index(&runner, "new_agent")
            .expect("new_agent should be in menu");
        navigate_to_menu_index_with_lua(&mut runner, &lua, new_agent_idx);
        assert_eq!(lua_list_selected(&lua), new_agent_idx);

        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        thread::sleep(Duration::from_millis(10));
        assert_eq!(runner.mode(), "new_agent_select_worktree");

        // Navigate to first existing worktree (index 2, after "Use Main Branch" and "Create New Worktree")
        runner.overlay_list_actions = vec![
            "main".to_string(),
            "create_new".to_string(),
            "worktree_0".to_string(),
            "worktree_1".to_string(),
        ];
        process_key_with_lua(&mut runner, make_key_down(), &lua);
        process_key_with_lua(&mut runner, make_key_down(), &lua);
        assert_eq!(lua_list_selected(&lua), 2);

        // Select existing worktree
        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        thread::sleep(Duration::from_millis(10));

        // Modal closes after selection — stays in normal mode until agent_created event
        assert_eq!(
            runner.mode(),
            "normal",
            "Should be normal mode until agent_created event selects the agent"
        );

        // Verify reopen_worktree JSON message with path
        let mut found_create = false;
        while let Ok(req) = request_rx.try_recv() {
            let msg = unwrap_lua_msg(req);
            if let Some(data) = msg.get("data") {
                if data.get("type").and_then(|t| t.as_str()) == Some("reopen_worktree") {
                    assert_eq!(
                        data.get("path").and_then(|v| v.as_str()),
                        Some("/path/worktree-1")
                    );
                    assert_eq!(
                        data.get("branch").and_then(|v| v.as_str()),
                        Some("feature-branch")
                    );
                    found_create = true;
                    break;
                }
            }
        }
        assert!(found_create, "reopen_worktree JSON message should be sent");

        // Cleanup
        shutdown.store(true, Ordering::Relaxed);
    }

    /// Verifies empty issue name is rejected.
    #[test]
    fn test_e2e_empty_issue_name_rejected() {
        let (mut runner, _cmd_rx) = create_test_runner();
        let lua = make_test_layout_with_keybindings();

        // Bypass to NewAgentCreateWorktree mode directly
        runner.mode = "new_agent_create_worktree".to_string();
        let _ = lua.exec("_tui_state.mode = 'new_agent_create_worktree'");

        // Submit empty input
        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        // Should stay in same mode
        assert_eq!(
            runner.mode(),
            "new_agent_create_worktree",
            "Empty issue name should be rejected"
        );
    }

    /// Verifies cancel at each stage returns to Normal.
    #[test]
    fn test_e2e_cancel_agent_creation_at_each_stage() {
        let (mut runner, mut cmd_rx) = create_test_runner();
        let lua = make_test_layout_with_keybindings();

        // Cancel at worktree selection
        runner.mode = "new_agent_select_worktree".to_string();
        let _ = lua.exec("_tui_state.mode = 'new_agent_select_worktree'");
        process_key_with_lua(&mut runner, make_key_escape(), &lua);
        assert_eq!(runner.mode(), "normal");

        // Cancel at issue input
        runner.mode = "new_agent_create_worktree".to_string();
        let _ = lua.exec("_tui_state.mode = 'new_agent_create_worktree'; _tui_state.input_buffer = 'partial'");
        process_key_with_lua(&mut runner, make_key_escape(), &lua);
        assert_eq!(runner.mode(), "normal");
        assert!(lua_input_buffer(&lua).is_empty(), "Buffer should be cleared");

        // Cancel at prompt
        runner.mode = "new_agent_prompt".to_string();
        let _ = lua.exec("_tui_state.mode = 'new_agent_prompt'");
        process_key_with_lua(&mut runner, make_key_escape(), &lua);
        assert_eq!(runner.mode(), "normal");

        // No commands sent
        assert!(cmd_rx.try_recv().is_err());
    }

    // =========================================================================
    // E2E Text Input Tests
    // =========================================================================

    /// Verifies backspace in text input mode.
    #[test]
    fn test_e2e_text_input_backspace() {
        let (mut runner, _cmd_rx) = create_test_runner();
        let lua = make_test_layout_with_keybindings();

        runner.mode = "new_agent_create_worktree".to_string();
        let _ = lua.exec("_tui_state.mode = 'new_agent_create_worktree'");

        // Type characters
        process_key_with_lua(&mut runner, make_key_char('a'), &lua);
        process_key_with_lua(&mut runner, make_key_char('b'), &lua);
        process_key_with_lua(&mut runner, make_key_char('c'), &lua);
        assert_eq!(lua_input_buffer(&lua), "abc");

        // Backspace
        process_key_with_lua(&mut runner, make_key_desc("backspace", &[0x7f]), &lua);
        assert_eq!(lua_input_buffer(&lua), "ab");

        process_key_with_lua(&mut runner, make_key_desc("backspace", &[0x7f]), &lua);
        process_key_with_lua(&mut runner, make_key_desc("backspace", &[0x7f]), &lua);
        assert_eq!(lua_input_buffer(&lua), "");

        // Backspace on empty is safe
        process_key_with_lua(&mut runner, make_key_desc("backspace", &[0x7f]), &lua);
        assert_eq!(lua_input_buffer(&lua), "");
    }

    /// Verifies worktree navigation with arrow keys.
    #[test]
    fn test_e2e_worktree_navigation() {
        let (mut runner, _cmd_rx) = create_test_runner();
        let lua = make_test_layout_with_keybindings();

        // Stub overlay_list_actions for navigation test.
        runner.overlay_list_actions = vec![
            "main".to_string(),
            "create_new".to_string(),
            "worktree_0".to_string(),
            "worktree_1".to_string(),
        ];
        runner.mode = "new_agent_select_worktree".to_string();
        let _ = lua.exec("_tui_state.mode = 'new_agent_select_worktree'; _tui_state.list_selected = 0");

        // Navigate down
        process_key_with_lua(&mut runner, make_key_down(), &lua);
        assert_eq!(lua_list_selected(&lua), 1);

        process_key_with_lua(&mut runner, make_key_down(), &lua);
        assert_eq!(lua_list_selected(&lua), 2);

        // Continue to max
        process_key_with_lua(&mut runner, make_key_down(), &lua);
        assert_eq!(lua_list_selected(&lua), 3);

        // Should not exceed max
        process_key_with_lua(&mut runner, make_key_down(), &lua);
        assert_eq!(lua_list_selected(&lua), 3);

        // Navigate up
        process_key_with_lua(&mut runner, make_key_up(), &lua);
        assert_eq!(lua_list_selected(&lua), 2);

        process_key_with_lua(&mut runner, make_key_up(), &lua);
        assert_eq!(lua_list_selected(&lua), 1);

        process_key_with_lua(&mut runner, make_key_up(), &lua);
        assert_eq!(lua_list_selected(&lua), 0);

        // Should not go below 0
        process_key_with_lua(&mut runner, make_key_up(), &lua);
        assert_eq!(lua_list_selected(&lua), 0);
    }

    // =========================================================================
    // E2E Scroll Tests
    // =========================================================================

    /// Verifies scroll key bindings produce correct actions.
    #[test]
    fn test_e2e_scroll_keys() {
        let lua = make_test_layout_with_keybindings();
        let context = KeyContext::default();

        // Shift+PageUp for scroll up
        let result = lua
            .call_handle_key("shift+pageup", "normal", &context)
            .unwrap();
        assert_eq!(
            result.as_ref().map(|a| a.action.as_str()),
            Some("scroll_half_up"),
            "Shift+PageUp should produce scroll_half_up"
        );

        // Shift+PageDown for scroll down
        let result = lua
            .call_handle_key("shift+pagedown", "normal", &context)
            .unwrap();
        assert_eq!(
            result.as_ref().map(|a| a.action.as_str()),
            Some("scroll_half_down"),
            "Shift+PageDown should produce scroll_half_down"
        );
    }

    /// Verifies scroll actions are processed without panic.
    #[test]
    fn test_e2e_scroll_actions_processed() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // These should not panic
        runner.handle_tui_action(TuiAction::ScrollUp(10));
        runner.handle_tui_action(TuiAction::ScrollDown(5));
        runner.handle_tui_action(TuiAction::ScrollToTop);
        runner.handle_tui_action(TuiAction::ScrollToBottom);
    }

    // =========================================================================
    // E2E Agent Navigation Tests
    // =========================================================================

    /// Verifies Ctrl+J/K produce SelectNext/SelectPrevious actions via Lua.
    #[test]
    fn test_e2e_agent_navigation_keybindings() {
        let lua = make_test_layout_with_keybindings();
        let context = KeyContext::default();

        let result = lua
            .call_handle_key("ctrl+j", "normal", &context)
            .unwrap();
        assert_eq!(
            result.as_ref().map(|a| a.action.as_str()),
            Some("select_next"),
            "Ctrl+J should be select_next"
        );

        let result = lua
            .call_handle_key("ctrl+k", "normal", &context)
            .unwrap();
        assert_eq!(
            result.as_ref().map(|a| a.action.as_str()),
            Some("select_previous"),
            "Ctrl+K should be select_previous"
        );
    }

    /// Verifies Ctrl+] produces toggle_pty action via Lua.
    #[test]
    fn test_e2e_pty_toggle_keybinding() {
        let lua = make_test_layout_with_keybindings();
        let context = KeyContext::default();

        let result = lua
            .call_handle_key("ctrl+]", "normal", &context)
            .unwrap();
        assert_eq!(
            result.as_ref().map(|a| a.action.as_str()),
            Some("toggle_pty"),
            "Ctrl+] should be toggle_pty"
        );
    }

    /// Verifies SelectNext/SelectPrevious are no-op with empty agent list.

    // =========================================================================
    // E2E PTY View Toggle Tests
    // =========================================================================

    /// Verifies PTY toggle without agent is no-op.

    /// Verifies PTY toggle without server PTY stays on CLI.

    /// Verifies PTY toggle with server PTY switches views.

    // =========================================================================
    // Misc Action Tests
    // =========================================================================

    /// Verifies Quit action sets the local quit flag and sends JSON quit to Hub.
    ///
    /// TuiRunner.quit stops the TUI event loop; the JSON message tells Hub to
    /// stop its tick loop via Lua `hub.quit()`. Both are needed for clean exit.
    #[test]
    fn test_quit_action() {
        let (mut runner, _cmd_rx) = create_test_runner();

        assert!(!runner.quit);

        // TuiAction::Quit is a pure UI primitive — sets the quit flag.
        // The quit message to Hub is sent by Lua's quit action handler
        // (actions.lua returns send_msg + quit ops).
        runner.handle_tui_action(TuiAction::Quit);

        assert!(runner.quit, "Quit should set local quit flag");
    }

    /// Verifies None action is a no-op.
    #[test]
    fn test_none_action_is_noop() {
        let (mut runner, _cmd_rx) = create_test_runner();

        let mode_before = runner.mode().to_string();

        runner.handle_tui_action(TuiAction::None);

        assert_eq!(runner.mode(), mode_before);
    }

    // =========================================================================
    // Edge Case Tests
    // =========================================================================

    /// Verifies confirm close is safe even without selected agent (edge case).
    ///
    /// This tests the guard in `handle_confirm_close_agent` that prevents
    /// sending a command if no agent is selected (should not happen in
    /// normal flow but tests robustness).

    // =========================================================================
    // Error Handling Tests
    // =========================================================================

    // =========================================================================
    // Connection Code Tests
    // =========================================================================

    /// Verifies ConnectionCode mode renders gracefully when no connection code is cached.
    ///
    /// # Purpose
    ///
    /// When displaying the QR code modal ("connection_code" mode), the TUI uses
    /// the cached `self.connection_code` (populated via Lua event responses). If
    /// no code is available yet (e.g., Hub hasn't responded), render should still
    /// complete without panicking.
    #[test]
    fn test_connection_code_mode_renders_without_panic_on_hub_error() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Set mode to ConnectionCode with no cached code
        runner.mode = "connection_code".to_string();
        runner.connection_code = None;

        // Render should not panic even without cached connection code
        let result = runner.render(None, None);
        assert!(
            result.is_ok(),
            "Render should succeed even without cached connection code"
        );
    }

    /// Verifies render in Normal mode succeeds without connection code.
    ///
    /// # Purpose
    ///
    /// In Normal mode, no connection code is needed. Render should succeed
    /// regardless of the cached connection code state.
    #[test]
    fn test_normal_mode_render_succeeds_without_connection_code_fetch() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Stay in Normal mode
        assert_eq!(runner.mode, "normal");

        // Render in Normal mode should succeed without any Hub calls
        let result = runner.render(None, None);
        assert!(result.is_ok(), "Render should succeed in Normal mode");
    }

    // =========================================================================
    // Resize Propagation Tests (TDD)
    // =========================================================================

    /// **BUG FIX TEST**: Verifies resize event updates the vt100 parser dimensions.
    ///
    /// # Bug Description
    ///
    /// When terminal is resized, `handle_resize()` does:
    /// 1. Updates `terminal_dims` (correct)
    /// 2. Sends resize via Lua subscription (correct - propagates to PTY)
    ///
    /// But it **never updates the local vt100 parser dimensions**.
    /// This causes garbled display because:
    /// - PTY sends output formatted for new dimensions
    /// - Parser interprets it with old dimensions
    ///
    /// # Expected Behavior
    ///
    /// `handle_resize()` should also call:
    /// ```ignore
    /// let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
    /// parser.screen_mut().set_size(rows, cols);
    /// ```
    #[test]
    fn test_resize_updates_vt100_parser_dimensions() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Initial dimensions from create_test_runner: (24, 80)
        let initial_size = {
            let parser = runner.vt100_parser.lock().expect("parser lock poisoned");
            parser.screen().size()
        };
        assert_eq!(initial_size, (24, 80), "Initial parser size should be 24x80");

        // Simulate resize event to 40 rows x 120 cols
        // (handle_resize receives rows, cols in that order)
        runner.handle_resize(40, 120);

        // Verify terminal_dims was updated (this already works)
        assert_eq!(runner.terminal_dims, (40, 120), "terminal_dims should be updated");

        // BUG: Parser dimensions should ALSO be updated, but they're not
        let new_size = {
            let parser = runner.vt100_parser.lock().expect("parser lock poisoned");
            parser.screen().size()
        };
        assert_eq!(
            new_size, (40, 120),
            "Parser dimensions should be updated on resize. \
             BUG: vt100_parser.screen_mut().set_size() is never called in handle_resize()"
        );
    }

    // =========================================================================
    // Agent Navigation & Resize Request Tests
    // =========================================================================
    //
    // These tests verify that TuiRunner sends the correct JSON messages
    // when navigating between agents and handling terminal resize events.
    //
    // Agent navigation now uses the Lua subscribe/unsubscribe protocol (fire-and-forget),
    // so tests use `create_test_runner()` and verify subscribe messages directly.

    /// Verifies `focus_terminal` op subscribes and updates state.
    ///
    /// # Scenario
    ///
    /// Given 3 agents, `focus_terminal` with agent-1 should subscribe to
    /// that agent's PTY and update all selection state.
    #[test]
    fn test_focus_terminal_subscribes_and_updates_state() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Action: focus agent-1 (Lua provides agent_index)
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
            "agent_id": "agent-1",
            "agent_index": 1,
            "pty_index": 0,
        }));

        // Verify: subscribe message sent for agent index 1
        match request_rx.try_recv() {
            Ok(req) => {
                let msg = unwrap_lua_msg(req);
                assert_eq!(msg.get("type").and_then(|v| v.as_str()), Some("subscribe"));
                assert_eq!(msg.get("channel").and_then(|v| v.as_str()), Some("terminal"));
                let params = msg.get("params").expect("should have params");
                assert_eq!(params.get("agent_index").and_then(|v| v.as_u64()), Some(1));
                assert_eq!(params.get("pty_index").and_then(|v| v.as_u64()), Some(0));
            }
            Err(_) => panic!("Expected subscribe message to be sent"),
        }

        // Verify local state updated
        assert_eq!(runner.selected_agent.as_deref(), Some("agent-1"));
        assert_eq!(runner.current_agent_index, Some(1));
        assert_eq!(runner.current_pty_index, Some(0));
        assert_eq!(runner.current_terminal_sub_id, Some("tui:1:0".to_string()));
        assert!(runner.active_subscriptions.contains(&(1, 0)));
    }

    /// Verifies `focus_terminal` with nil agent_id clears selection.
    ///
    /// # Scenario
    ///
    /// When an agent is deleted, Lua returns `focus_terminal` with no agent_id
    /// to clear the current selection.
    #[test]
    fn test_focus_terminal_nil_clears_selection() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Setup: agent 0 selected with active subscription
        runner.selected_agent = Some("agent-0".to_string());
        runner.current_agent_index = Some(0);
        runner.current_pty_index = Some(0);
        runner.current_terminal_sub_id = Some("tui:0:0".to_string());
        runner.active_subscriptions.insert((0, 0));

        // Action: focus_terminal with no agent_id (clear selection)
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
        }));

        // Verify: unsubscribe sent
        match request_rx.try_recv() {
            Ok(req) => {
                let msg = unwrap_lua_msg(req);
                assert_eq!(msg.get("type").and_then(|v| v.as_str()), Some("unsubscribe"));
            }
            Err(_) => panic!("Expected unsubscribe message to be sent"),
        }

        // Verify selection cleared
        assert_eq!(runner.selected_agent, None);
        assert_eq!(runner.current_agent_index, None);
        assert_eq!(runner.current_pty_index, None);
        assert_eq!(runner.current_terminal_sub_id, None);
    }

    /// Verifies `focus_terminal` sends unsubscribe then subscribe when switching.
    ///
    /// # Scenario
    ///
    /// When switching from one agent to another, the TUI must unsubscribe from
    /// the current terminal before subscribing to the new one.
    #[test]
    fn test_focus_terminal_unsubscribes_then_subscribes() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Setup: agent 0 selected with active subscription
        runner.selected_agent = Some("agent-0".to_string());
        runner.current_agent_index = Some(0);
        runner.current_pty_index = Some(0);
        runner.current_terminal_sub_id = Some("tui:0:0".to_string());
        runner.active_subscriptions.insert((0, 0));

        // Action: focus agent-1 (Lua provides agent_index)
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
            "agent_id": "agent-1",
            "agent_index": 1,
            "pty_index": 0,
        }));

        // Verify: first message is unsubscribe
        match request_rx.try_recv() {
            Ok(req) => {
                let msg = unwrap_lua_msg(req);
                assert_eq!(
                    msg.get("type").and_then(|v| v.as_str()),
                    Some("unsubscribe"),
                    "First message should be unsubscribe"
                );
                assert_eq!(
                    msg.get("subscriptionId").and_then(|v| v.as_str()),
                    Some("tui:0:0")
                );
            }
            Err(_) => panic!("Expected unsubscribe message to be sent"),
        }

        // Verify: second message is subscribe
        match request_rx.try_recv() {
            Ok(req) => {
                let msg = unwrap_lua_msg(req);
                assert_eq!(
                    msg.get("type").and_then(|v| v.as_str()),
                    Some("subscribe"),
                    "Second message should be subscribe"
                );
                assert_eq!(msg.get("channel").and_then(|v| v.as_str()), Some("terminal"));
            }
            Err(_) => panic!("Expected subscribe message to be sent"),
        }

        // Verify state and active_subscriptions
        assert_eq!(runner.selected_agent.as_deref(), Some("agent-1"));
        assert_eq!(runner.current_agent_index, Some(1));
        assert_eq!(runner.current_terminal_sub_id, Some("tui:1:0".to_string()));
        assert!(!runner.active_subscriptions.contains(&(0, 0)), "Old sub should be removed");
        assert!(runner.active_subscriptions.contains(&(1, 0)), "New sub should be added");
    }

    /// Verifies `focus_terminal` with unknown agent_id is a no-op.
    ///
    /// # Scenario
    ///
    /// When the agent doesn't exist in the cache, focus_terminal should log
    /// a warning and not change any state.
    #[test]
    fn test_focus_terminal_missing_agent_index_is_noop() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Action: focus agent without agent_index (Lua bug or edge case)
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
            "agent_id": "nonexistent",
            "pty_index": 0,
        }));

        // Verify: no request sent
        assert!(
            request_rx.try_recv().is_err(),
            "No JSON should be sent when agent not found"
        );
    }

    /// Verifies `focus_terminal` skips when already focused on same agent+pty.
    ///
    /// # Scenario
    ///
    /// When already focused on agent-0 pty 0, calling focus_terminal for the
    /// same agent+pty should be a no-op (no unsub/resub).
    #[test]
    fn test_focus_terminal_same_target_is_noop() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Setup: agent 0 already focused
        runner.selected_agent = Some("agent-0".to_string());
        runner.current_agent_index = Some(0);
        runner.current_pty_index = Some(0);
        runner.current_terminal_sub_id = Some("tui:0:0".to_string());
        runner.active_subscriptions.insert((0, 0));

        // Action: focus same agent+pty (Lua provides agent_index)
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
            "agent_id": "agent-0",
            "agent_index": 0,
            "pty_index": 0,
        }));

        // Verify: no messages sent (already focused)
        assert!(
            request_rx.try_recv().is_err(),
            "No JSON should be sent when already focused on target"
        );
    }

    /// Verifies `handle_resize()` sends resize through Lua terminal subscription
    /// when connected to a PTY.
    ///
    /// # Scenario
    ///
    /// When terminal is resized to 40 rows x 120 cols with a PTY connected,
    /// TuiRunner should:
    /// 1. Update local `terminal_dims`
    /// 2. Resize the fallback parser
    /// 3. Clear `widget_dims` so next render triggers per-widget resize
    /// 4. Send client-level resize via hub subscription
    ///
    /// Per-PTY resize is NOT sent here — it's deferred to `sync_widget_dims()`
    /// after the next render pass, which knows the actual widget dimensions.
    #[test]
    fn test_handle_resize_sends_resize_via_lua() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Set up connected state with active subscription and cached widget dims
        runner.current_agent_index = Some(0);
        runner.current_pty_index = Some(0);
        runner.current_terminal_sub_id = Some("tui:0:0".to_string());
        runner.active_subscriptions.insert((0, 0));
        runner.widget_dims.insert((0, 0), (24, 80));

        // Action: resize to 40 rows x 120 cols
        runner.handle_resize(40, 120);

        // Verify: only hub-level resize sent (per-PTY resize deferred to sync_widget_dims)
        match request_rx.try_recv() {
            Ok(req) => {
                let msg = unwrap_lua_msg(req);
                assert_eq!(msg["subscriptionId"], "tui_hub");
                assert_eq!(msg["data"]["type"], "resize");
                assert_eq!(msg["data"]["rows"], 40);
                assert_eq!(msg["data"]["cols"], 120);
            }
            Err(_) => panic!("Expected hub resize message to be sent"),
        }

        // Verify: no additional messages (per-PTY resize is deferred)
        assert!(request_rx.try_recv().is_err(), "No per-PTY resize should be sent during handle_resize");

        // Verify: local state updated
        assert_eq!(runner.terminal_dims, (40, 120));

        // Verify: widget_dims cleared so next render triggers resize
        assert!(runner.widget_dims.is_empty());
    }

    /// Verifies `handle_resize()` sends only hub-level resize (not terminal)
    /// when no terminal subscription is active.
    #[test]
    fn test_handle_resize_without_terminal_sub_sends_hub_only() {
        let (mut runner, mut request_rx) = create_test_runner();

        // No terminal subscription (not connected to a PTY)
        runner.handle_resize(40, 120);

        // Verify: hub-level resize sent (client dims tracking)
        match request_rx.try_recv() {
            Ok(req) => {
                let msg = unwrap_lua_msg(req);
                assert_eq!(msg["subscriptionId"], "tui_hub");
                assert_eq!(msg["data"]["type"], "resize");
            }
            _ => panic!("Expected hub resize message"),
        }

        // Verify: no terminal resize sent (active_subscriptions is empty)
        // The hub resize was the first and only message
        assert!(
            request_rx.try_recv().is_err(),
            "No additional messages should be sent"
        );

        // Verify: local state still updated
        assert_eq!(runner.terminal_dims, (40, 120));
    }

    // === Hot-Reload & Error UX ===

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

    #[test]
    fn test_layout_lua_reload_valid() {
        let lua = LayoutLua::new("function render(s) return { type = 'empty' } end").unwrap();
        let result = lua.reload("function render(s) return { type = 'empty' } end");
        assert!(result.is_ok());
    }

    #[test]
    fn test_layout_lua_reload_invalid() {
        let lua = LayoutLua::new("function render(s) return { type = 'empty' } end").unwrap();
        let result = lua.reload("this is not valid lua!!!");
        assert!(result.is_err());
    }

    #[test]
    fn test_layout_lua_reload_preserves_old_on_error() {
        let lua =
            LayoutLua::new("function render(s) return { type = 'empty' } end\nfunction render_overlay(s) return nil end").unwrap();
        // Reload with bad source — should fail
        let _ = lua.reload("broken!!!");
        // But the old functions should still be callable... actually mlua replaces
        // on exec, so a failed load doesn't clear the old functions. Verify:
        let ctx = make_test_render_context();
        let result = lua.call_render(&ctx);
        assert!(result.is_ok(), "Old render function should still work after failed reload");
    }

    fn make_test_render_context() -> super::super::render::RenderContext<'static> {
        // 'static requires leaked reference for the empty pool
        let pool: &'static std::collections::HashMap<(usize, usize), std::sync::Arc<std::sync::Mutex<vt100::Parser>>> =
            Box::leak(Box::new(std::collections::HashMap::new()));
        super::super::render::RenderContext {
            error_message: None,
            connection_code: None,
            bundle_used: false,
            selected_agent_index: 0,
            active_parser: None,
            parser_pool: pool,
            active_pty_index: 0,
            scroll_offset: 0,
            is_scrolled: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            terminal_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }

    // === Subscription Sync Tests ===

    /// Verifies `sync_subscriptions` subscribes to a new terminal binding.
    #[test]
    fn test_sync_subscriptions_subscribes_new() {
        let (mut runner, mut request_rx) = create_test_runner();

        runner.current_agent_index = Some(0);
        runner.current_pty_index = Some(0);

        // Tree with a single terminal widget (no explicit binding → defaults)
        let tree = crate::tui::render_tree::RenderNode::Widget {
            widget_type: crate::tui::render_tree::WidgetType::Terminal,
            block: None,
            custom_lines: None,
            props: None,
        };

        runner.sync_subscriptions(&tree);

        // Should have sent a subscribe message for (0, 0)
        match request_rx.try_recv() {
            Ok(req) => {
                let msg = unwrap_lua_msg(req);
                assert_eq!(msg.get("type").and_then(|v| v.as_str()), Some("subscribe"));
                assert_eq!(msg.get("subscriptionId").and_then(|v| v.as_str()), Some("tui:0:0"));
                let params = msg.get("params").expect("should have params");
                assert_eq!(params.get("agent_index").and_then(|v| v.as_u64()), Some(0));
                assert_eq!(params.get("pty_index").and_then(|v| v.as_u64()), Some(0));
            }
            Err(_) => panic!("Expected subscribe message"),
        }

        assert!(runner.active_subscriptions.contains(&(0, 0)));
        assert!(runner.parser_pool.contains_key(&(0, 0)));
    }

    /// Verifies `sync_subscriptions` unsubscribes when a binding is removed.
    #[test]
    fn test_sync_subscriptions_unsubscribes_removed() {
        let (mut runner, mut request_rx) = create_test_runner();

        runner.current_agent_index = Some(0);
        runner.current_pty_index = Some(0);

        // Pre-populate active subscriptions for two PTYs
        runner.active_subscriptions.insert((0, 0));
        runner.active_subscriptions.insert((0, 1));

        // Tree only has terminal for PTY 0 — PTY 1 should be unsubscribed
        let tree = crate::tui::render_tree::RenderNode::Widget {
            widget_type: crate::tui::render_tree::WidgetType::Terminal,
            block: None,
            custom_lines: None,
            props: Some(crate::tui::render_tree::WidgetProps::Terminal(
                crate::tui::render_tree::TerminalBinding {
                    agent_index: Some(0),
                    pty_index: Some(0),
                },
            )),
        };

        runner.sync_subscriptions(&tree);

        // Should have sent unsubscribe for (0, 1)
        let mut found_unsubscribe = false;
        while let Ok(req) = request_rx.try_recv() {
            let msg = unwrap_lua_msg(req);
            if msg.get("type").and_then(|v| v.as_str()) == Some("unsubscribe") {
                assert_eq!(msg.get("subscriptionId").and_then(|v| v.as_str()), Some("tui:0:1"));
                found_unsubscribe = true;
            }
        }
        assert!(found_unsubscribe, "Should send unsubscribe for removed binding");

        assert!(runner.active_subscriptions.contains(&(0, 0)));
        assert!(!runner.active_subscriptions.contains(&(0, 1)));
    }

    /// Verifies `sync_subscriptions` is idempotent for unchanged bindings.
    #[test]
    fn test_sync_subscriptions_no_change_idempotent() {
        let (mut runner, mut request_rx) = create_test_runner();

        runner.current_agent_index = Some(0);
        runner.current_pty_index = Some(0);
        runner.active_subscriptions.insert((0, 0));

        let tree = crate::tui::render_tree::RenderNode::Widget {
            widget_type: crate::tui::render_tree::WidgetType::Terminal,
            block: None,
            custom_lines: None,
            props: None,
        };

        runner.sync_subscriptions(&tree);

        // No messages should be sent (already subscribed)
        assert!(
            request_rx.try_recv().is_err(),
            "No messages should be sent when subscriptions unchanged"
        );
        assert_eq!(runner.active_subscriptions.len(), 1);
    }

    // Rust guideline compliant 2026-02
}
