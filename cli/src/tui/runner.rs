//! TUI Runner - thin event loop wiring subsystems together.
//!
//! The TuiRunner is the message bus for the TUI thread. It owns subsystems
//! as fields and wires them together in a poll → process → render loop.
//! Subsystems are pure state machines — only the runner touches I/O channels.
//!
//! # Architecture
//!
//! ```text
//! TuiRunner (the bus)
//!   ├── PanelPool        — owns terminal panels, focus state, subscriptions
//!   ├── TerminalModes    — mirrors DECCKM/bracketed paste/kitty to outer terminal
//!   ├── HotReloader      — watches filesystem, reloads Lua state in-place
//!   ├── WidgetStateStore — persistent state for uncontrolled widgets
//!   ├── request_tx       — sends messages to Hub (only I/O point)
//!   └── output_rx        — receives PTY output and Lua events from Hub
//! ```
//!
//! # Event Loop
//!
//! ```text
//! while !quit {
//!     poll_input(lua);              // keyboard/mouse → Lua → TuiAction/ops
//!     poll_pty_events(lua);         // Hub output → panel_pool
//!     terminal_modes.sync(panel);   // mirror PTY modes to outer terminal
//!     if hot_reloader.poll(lua) { dirty = true; }
//!     if dirty { render(lua); }
//!     poll_wait();                  // block until next event
//! }
//! ```
//!
//! # Subsystem Communication
//!
//! Subsystem methods return `OutMessages` (Vec<serde_json::Value>) instead of
//! sending directly. The runner collects returned messages and sends them via
//! `request_tx`. No subsystem knows about channels.
//!
//! # Event Flow
//!
//! Agent lifecycle events flow through Lua (`broadcast_hub_event()` in
//! `connections.lua`) and arrive as `TuiOutput::Message` JSON. TuiRunner
//! dispatches these through `events.lua` via `call_on_hub_event()`, which
//! returns typed `LuaOp` variants that update cached state mechanically.

// Rust guideline compliant 2026-02

use std::io::Stdout;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::backend::Backend;
use ratatui::Terminal;

use ratatui::backend::CrosstermBackend;

use crate::client::{TuiOutput, TuiRequest};
use crate::hub::Hub;
use crate::tui::layout::terminal_widget_inner_area;

use super::actions::TuiAction;
use super::layout_lua::{KeyContext, LayoutLua, LuaKeyAction};
use super::qr::ConnectionCodeData;
use super::raw_input::{InputEvent, MouseEventType, RawInputReader};
use super::smooth_scroll::SmoothScroll;
use super::ColorCache;

/// Default scrollback lines for VT100 parser.

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
///                                            ──► panels[focused] ──► render
/// ```
///
/// TuiRunner sends `TuiRequest` messages through `request_tx`: control messages
/// go through Lua `client.lua`, PTY keyboard input goes directly to the PTY.
pub struct TuiRunner<B: Backend> {
    // === Subsystems ===
    /// TUI-local terminal color cache learned from the outer terminal.
    ///
    /// Seeded from the boot probe, then updated only from this client's own
    /// color replies so hub-global fallback state stays separate.
    color_cache: ColorCache,

    /// Terminal panel pool — owns panels, focus state, and subscriptions.
    pub(super) panel_pool: super::panel_pool::PanelPool,

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

    // === Output Channel ===
    /// Receiver for PTY output and Lua events from Hub.
    ///
    /// Hub sends `TuiOutput` messages through this channel: binary PTY data
    /// from Lua forwarder tasks and JSON events from `tui.send()` in Lua.
    output_rx: tokio::sync::mpsc::UnboundedReceiver<TuiOutput>,

    // === Wake Pipe ===
    /// Read end of the wake pipe. Hub/forwarders write 1 byte to the write
    /// end after sending to `output_rx`, unblocking the TUI `libc::poll()`.
    wake_fd: Option<std::os::unix::io::RawFd>,

    // === Control ===
    /// Shutdown flag (shared with Hub for coordinated shutdown).
    shutdown: Arc<AtomicBool>,

    /// Internal quit flag.
    pub(super) quit: bool,

    // === Lua Bootstrap ===
    /// Lua sources consumed once at startup by `run()`. `None` after init.
    lua_bootstrap: Option<super::hot_reload::LuaBootstrap>,

    // === Raw Input ===
    /// Raw stdin reader — replaces crossterm's event reader for keyboard input.
    raw_reader: RawInputReader,

    /// True when stdin has a permanent error (EIO). Prevents `poll_wait()`
    /// from including stdin in `libc::poll`, which would cause a tight spin
    /// loop since `POLLERR` triggers immediate readiness.
    stdin_dead: bool,

    /// Partial OSC color response bytes buffered across stdin reads.
    osc_color_response_buf: Vec<u8>,

    /// Outstanding TUI-local color probes by dynamic color index.
    ///
    /// Used to consume our own OSC 4/10/11/12 replies locally after focus
    /// regain without forwarding unsolicited responses into the PTY.
    local_color_probe_pending: std::collections::HashMap<usize, usize>,

    /// Whether the local terminal profile changed during the current input batch.
    ///
    /// Flushed to the hub once after input processing so a burst of local
    /// probe replies does not spam one JSON profile update per response.
    local_color_profile_dirty: bool,

    /// Session awaiting deferred focus-in passthrough after a local probe.
    ///
    /// When the TUI regains focus, we first probe the outer terminal and sync
    /// the resulting profile into the session. Only after that completes do we
    /// pass `ESC[I` through to the child PTY, so focus-triggered color queries
    /// see the updated session profile.
    pending_focus_in_session: Option<String>,

    /// Last time the TUI actively reprobed the outer terminal profile.
    ///
    /// Used to keep long-lived focused sessions in sync with outer-terminal
    /// theme changes even when there is no focus transition.
    last_local_color_probe_at: Option<Instant>,

    /// SIGWINCH flag for terminal resize detection.
    pub(super) resize_flag: Arc<AtomicBool>,

    // === Terminal Mode Mirroring ===
    /// Mirrors DECCKM, bracketed paste, and kitty keyboard protocol
    /// from the focused PTY to the outer terminal. Also tracks OS-level
    /// terminal focus for synthetic focus events.
    pub(super) terminal_modes: super::terminal_modes::TerminalModes,

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

    // === Widget State ===
    /// Persistent state store for uncontrolled widgets (list selection, input buffer).
    ///
    /// Widgets with an `id` prop and no explicit `selected`/`value` prop are
    /// **uncontrolled** — Rust owns their mechanical state here. Garbage
    /// collected after each render pass via `retain_seen()`.
    pub(super) widget_states: super::widget_state::WidgetStateStore,

    /// ID of the focused uncontrolled list widget (from last render pass).
    ///
    /// Used to route `list_up`/`list_down` actions to the correct widget state.
    pub(super) focused_list_id: Option<String>,

    /// ID of the focused uncontrolled input widget (from last render pass).
    ///
    /// Used to route `input_char`/`input_backspace`/cursor actions to the
    /// correct widget state.
    pub(super) focused_input_id: Option<String>,

    /// Dirty flag — when true, the next loop iteration will render.
    /// Set by any event that changes visible state (PTY output, input,
    /// resize, hot-reload). Cleared after render.
    dirty: bool,

    /// Cached notification focus state. True when the TUI is subscribed to
    /// a PTY (`current_session_uuid` is Some), has no overlay, and the
    /// terminal window is focused. Only sends `FocusChanged` when this
    /// value actually transitions.
    notification_focused: bool,

    // === Mouse State ===
    /// Cached widget areas from the last render pass, for mouse hit-testing.
    /// Keyed by widget ID (session_uuid for terminals, `id` prop for others).
    last_widget_areas: std::collections::HashMap<String, super::render::WidgetArea>,

    /// Widget currently "captured" by a mouse drag. Set on mouse-down,
    /// cleared on mouse-up. While set, all mouse events route to this
    /// widget regardless of cursor position.
    mouse_capture: Option<MouseCapture>,

    // === Wire protocol entity stores (see brief §6.2) ===
    /// Per-entity-type stores populated by wire envelopes
    /// (`entity_snapshot`/`upsert`/`patch`/`remove`). Read by the
    /// composite renderers (`session_list`, `workspace_list`, …) and the
    /// `$bind` resolver. Empty until the hub starts emitting entity frames
    /// (commit 7); the dispatcher in `dispatch_hub_event` treats any
    /// recognised entity envelope as authoritative.
    pub(super) entity_stores: super::entity_stores::TuiEntityStores,
}

/// State for an active mouse drag capture.
#[derive(Debug, Clone)]
struct MouseCapture {
    /// Widget ID that captured the mouse.
    widget_id: String,
    /// Widget type (e.g., "terminal", "list").
    widget_type: String,
    /// Widget rect at capture time (for stable local coordinate calculation).
    origin_rect: ratatui::layout::Rect,
}

impl<B: Backend> std::fmt::Debug for TuiRunner<B>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiRunner")
            .field("mode", &self.mode)
            .field("selected_agent", &self.panel_pool.selected_agent())
            .field(
                "current_session_uuid",
                &self.panel_pool.current_session_uuid(),
            )
            .field("terminal_dims", &self.panel_pool.terminal_dims())
            .field("quit", &self.quit)
            .finish_non_exhaustive()
    }
}

impl<B> TuiRunner<B>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    const PERIODIC_LOCAL_COLOR_PROBE_INTERVAL: Duration = Duration::from_secs(2);
    const FOCUS_SYNC_COLOR_INDICES: [usize; 3] = [256usize, 257usize, 258usize];

    /// Probe the spawning terminal for default colors used to seed libghostty.
    pub fn probe_spawning_terminal_colors() -> ColorCache {
        let mut store = crate::hub::terminal_profile::TerminalProfileStore::default();
        store.probe_spawning_terminal();
        store.shared_color_cache()
    }

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
        wake_fd: Option<std::os::unix::io::RawFd>,
    ) -> Self {
        Self::new_with_color_cache(
            terminal,
            request_tx,
            output_rx,
            shutdown,
            terminal_dims,
            wake_fd,
            Self::probe_spawning_terminal_colors(),
        )
    }

    /// Create a new TuiRunner with an explicit libghostty color cache.
    pub fn new_with_color_cache(
        terminal: Terminal<B>,
        request_tx: tokio::sync::mpsc::UnboundedSender<TuiRequest>,
        output_rx: tokio::sync::mpsc::UnboundedReceiver<TuiOutput>,
        shutdown: Arc<AtomicBool>,
        terminal_dims: (u16, u16),
        wake_fd: Option<std::os::unix::io::RawFd>,
        color_cache: ColorCache,
    ) -> Self {
        Self {
            color_cache: Arc::clone(&color_cache),
            panel_pool: super::panel_pool::PanelPool::new_with_color_cache(
                terminal_dims,
                color_cache,
            ),
            terminal,
            mode: String::new(),
            connection_code: None,
            error_message: None,
            request_tx,
            output_rx,
            wake_fd,
            shutdown,
            quit: false,
            lua_bootstrap: None,
            raw_reader: RawInputReader::new(),
            stdin_dead: false,
            osc_color_response_buf: Vec::new(),
            local_color_probe_pending: std::collections::HashMap::new(),
            local_color_profile_dirty: false,
            pending_focus_in_session: None,
            last_local_color_probe_at: None,
            resize_flag: Arc::new(AtomicBool::new(false)),
            terminal_modes: super::terminal_modes::TerminalModes::new(),
            overlay_list_actions: Vec::new(),
            has_overlay: false,
            widget_states: super::widget_state::WidgetStateStore::new(),
            focused_list_id: None,
            focused_input_id: None,
            dirty: true, // render on first frame
            notification_focused: false,
            last_widget_areas: std::collections::HashMap::new(),
            mouse_capture: None,
            entity_stores: super::entity_stores::TuiEntityStores::new(),
        }
    }

    /// Returns a clone of the resize flag for SIGWINCH registration.
    pub fn resize_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.resize_flag)
    }

    /// Set the Lua bootstrap (consumed once by `run()` to init Lua + hot-reloader).
    pub fn set_lua_bootstrap(&mut self, bootstrap: super::hot_reload::LuaBootstrap) {
        self.lua_bootstrap = Some(bootstrap);
    }

    /// Get the current mode string.
    #[must_use]
    pub fn mode(&self) -> &str {
        &self.mode
    }

    /// Get the selected agent key (delegates to PanelPool).
    #[must_use]
    pub fn selected_agent(&self) -> Option<&str> {
        self.panel_pool.selected_agent()
    }

    /// Build an `ActionContext` from current TuiRunner state.
    ///
    /// Shared by action dispatch and hub event dispatch so both Lua
    /// callbacks receive the same context shape.
    pub(super) fn build_action_context(&self) -> super::layout_lua::ActionContext {
        super::layout_lua::ActionContext {
            overlay_actions: self.overlay_list_actions.clone(),
            selected_agent: self.panel_pool.selected_agent.clone(),
            action_char: None,
            terminal_focused: self.terminal_modes.terminal_focused(),
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

        // Initialize Lua and hot-reloader from bootstrap (consumed once).
        // Done here (after thread::spawn) because mlua::Lua is !Send.
        let (mut layout_lua, initial_mode, mut hot_reloader) = self
            .lua_bootstrap
            .take()
            .map(|b| b.init())
            .unwrap_or_else(|| (None, String::new(), super::hot_reload::HotReloader::empty()));

        if !initial_mode.is_empty() {
            self.mode = initial_mode;
        }

        // Initialize parser with terminal dimensions
        let (rows, cols) = self.panel_pool.terminal_dims();
        log::info!("Initial TUI dimensions: {}cols x {}rows", cols, rows);

        while !self.should_quit() {
            // 1. Handle keyboard/mouse input
            self.poll_input(layout_lua.as_ref());

            if self.should_quit() {
                break;
            }

            // 2. Poll PTY output and Lua events (via Hub output channel)
            self.poll_pty_events(layout_lua.as_ref());

            // 2b. Mirror terminal modes from PTY to outer terminal.
            // Only sync from a fully connected panel; a connecting panel may
            // still hold stale parser state until fresh scrollback arrives.
            {
                let focused = self
                    .panel_pool
                    .focused_panel()
                    .filter(|panel| panel.state() == super::terminal_panel::PanelState::Connected);
                self.terminal_modes.sync(focused, self.has_overlay);
            }

            // 3. Hot-reload: built-in UI files and extensions
            if hot_reloader.poll(&mut layout_lua) {
                self.dirty = true;
            }

            self.maybe_periodic_local_color_probe();

            // 4. Render only when something changed
            if self.dirty {
                self.render(layout_lua.as_ref(), hot_reloader.layout_error())?;
                self.dirty = false;
            }

            // Block until stdin has input, wake pipe signals, or timeout.
            // Replaces the old `thread::sleep(16ms)` with event-driven wakeup:
            // zero CPU when idle, instant response when events arrive.
            self.poll_wait();
        }

        // Signal main thread to exit too (bidirectional shutdown)
        self.shutdown.store(true, Ordering::SeqCst);
        log::info!(
            "TuiRunner event loop exiting (quit={}, shutdown=true)",
            self.quit
        );
        Ok(())
    }

    /// Block until stdin has data, the wake pipe signals, or the timeout
    /// expires. Replaces `thread::sleep(16ms)` with event-driven wakeup.
    ///
    /// When a wake pipe is configured, polls both stdin (fd 0) and the wake
    /// pipe read end. Hub and forwarder tasks write 1 byte to the wake pipe
    /// after sending to `output_rx`, providing instant TUI wakeup.
    ///
    /// When stdin has a permanent error (`stdin_dead`), only polls the wake
    /// pipe to avoid a tight spin loop from `POLLERR` on stdin.
    ///
    /// Falls back to a 16ms sleep when no wake pipe is available (tests).
    fn poll_wait(&mut self) {
        let Some(wake_read_fd) = self.wake_fd else {
            // No wake pipe (tests) — fall back to original sleep behavior
            std::thread::sleep(Duration::from_millis(16));
            return;
        };

        if self.stdin_dead {
            // stdin has a permanent error — poll only the wake pipe.
            // Without this guard, POLLERR on stdin causes immediate return
            // from poll(), creating a tight spin loop.
            let mut fds = [libc::pollfd {
                fd: wake_read_fd,
                events: libc::POLLIN,
                revents: 0,
            }];
            unsafe { libc::poll(fds.as_mut_ptr(), 1, 100) };

            if fds[0].revents & libc::POLLIN != 0 {
                Self::drain_wake_pipe(wake_read_fd);
            }
            return;
        }

        let mut fds = [
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_read_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];

        // 100ms timeout as backstop for SIGWINCH resize (signal_hook uses
        // SA_RESTART so poll isn't interrupted), file watcher, and other
        // periodic checks. 6x fewer wakeups than the old 16ms sleep.
        unsafe { libc::poll(fds.as_mut_ptr(), 2, 100) };

        // Detect permanent stdin error — POLLERR or POLLHUP without POLLIN
        // means stdin is dead (terminal closed, fd invalid, etc.).
        if fds[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
            && fds[0].revents & libc::POLLIN == 0
        {
            log::warn!(
                "stdin poll returned error flags (revents=0x{:x}), disabling stdin polling",
                fds[0].revents
            );
            self.stdin_dead = true;
        }

        // Drain wake pipe to prevent accumulation (non-blocking read)
        if fds[1].revents & libc::POLLIN != 0 {
            Self::drain_wake_pipe(wake_read_fd);
        }
    }

    /// Drain the wake pipe to prevent accumulation (non-blocking reads).
    fn drain_wake_pipe(wake_read_fd: i32) {
        let mut drain_buf = [0u8; 256];
        loop {
            let n = unsafe {
                libc::read(
                    wake_read_fd,
                    drain_buf.as_mut_ptr() as *mut libc::c_void,
                    drain_buf.len(),
                )
            };
            if n <= 0 {
                break;
            }
        }
    }

    /// Poll for keyboard/mouse input and handle it.
    ///
    /// Reads raw bytes from stdin and parses them into events. Also checks
    /// the SIGWINCH flag for terminal resize. This replaces crossterm's
    /// event reader to preserve raw bytes for PTY passthrough.
    fn poll_input(&mut self, layout_lua: Option<&LayoutLua>) {
        let (events, stdin_dead) = self.raw_reader.drain_events();
        if stdin_dead {
            self.stdin_dead = true;
        }

        for event in events {
            match &event {
                InputEvent::MouseScroll { direction, x, y } if !self.has_overlay => {
                    self.dirty = true;
                    self.route_mouse_scroll(*direction, *x, *y);
                }
                InputEvent::MouseScroll { .. } => {
                    // Overlay active — swallow scroll events.
                }
                InputEvent::Mouse {
                    button,
                    event_type,
                    x,
                    y,
                } if !self.has_overlay => {
                    self.dirty = true;
                    self.handle_mouse_event(*button, *event_type, *x, *y, layout_lua);
                }
                InputEvent::Mouse { .. } => {
                    // Overlay active — swallow mouse events.
                }
                InputEvent::FocusGained => {
                    self.terminal_modes.on_focus_gained();
                    self.request_local_color_probe();
                    log::debug!(
                        "[FOCUS] terminal gained focus, mode={} overlay={}",
                        self.mode,
                        self.has_overlay
                    );
                    self.sync_notification_focus();
                    if self.mode == "terminal" && !self.has_overlay {
                        if self.focus_reporting_enabled_for_current_session() {
                            self.pending_focus_in_session =
                                self.panel_pool.current_session_uuid().map(str::to_string);
                            self.flush_pending_focus_in();
                        }
                    }
                }
                InputEvent::FocusLost => {
                    self.terminal_modes.on_focus_lost();
                    self.pending_focus_in_session = None;
                    log::debug!(
                        "[FOCUS] terminal lost focus, mode={} overlay={}",
                        self.mode,
                        self.has_overlay
                    );
                    self.sync_notification_focus();
                    if self.mode == "terminal" && !self.has_overlay {
                        if self.focus_reporting_enabled_for_current_session() {
                            log::debug!(
                                "[FOCUS] forwarding \\x1b[O to PTY session={:?}",
                                self.panel_pool.current_session_uuid()
                            );
                            self.handle_pty_input(b"\x1b[O");
                        }
                    }
                }
                InputEvent::Paste { ref raw_bytes } => {
                    self.dirty = true;
                    // Forward the complete bracketed paste atomically to the PTY.
                    // Apps like Claude Code rely on receiving the full paste
                    // (start marker + content + end marker) in one read to detect
                    // file drag/drop vs typed input.
                    if self.mode == "terminal" && !self.has_overlay {
                        log::debug!("[PASTE] Forwarding {} bytes to PTY", raw_bytes.len());
                        self.handle_pty_input(raw_bytes);
                    }
                }
                InputEvent::Key { ref raw_bytes, .. } => {
                    if Self::is_color_scheme_response(raw_bytes) {
                        log::debug!(
                            "[PTY-PROBE] Intercepted terminal color-scheme response ({} bytes), routing to PTY",
                            raw_bytes.len()
                        );
                        self.handle_pty_input(raw_bytes);
                        continue;
                    }

                    // Intercept OSC color responses from the outer terminal.
                    // These may arrive split across multiple stdin reads, so
                    // buffer until we have complete OSC 4/10/11/12 replies.
                    let responses = self.take_osc_color_responses(raw_bytes);
                    if !responses.is_empty() {
                        for response in responses {
                            let Some((index, color)) = Self::parse_color_cache_entry(&response)
                            else {
                                continue;
                            };
                            if self.consume_local_color_probe(index) {
                                self.observe_terminal_color_response(index, color);
                                continue;
                            }
                            log::debug!(
                                "[PTY-PROBE] Intercepted terminal color response ({} bytes), routing to PTY",
                                response.len()
                            );
                            self.handle_pty_input(&response);
                        }
                        self.dirty = true;
                        continue;
                    }

                    self.dirty = true;
                    self.handle_raw_input_event(event, layout_lua);
                }
            }
        }

        // Reset scroll acceleration for next tick.
        self.flush_terminal_color_profile_updates();
        for panel in self.panel_pool.panels.values_mut() {
            panel.reset_scroll_accel();
        }
        // Check SIGWINCH resize flag
        if self.resize_flag.swap(false, Ordering::SeqCst) {
            self.dirty = true;
            let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
            let (inner_rows, inner_cols) = terminal_widget_inner_area(cols, rows);
            self.handle_resize(inner_rows, inner_cols);
        }
    }

    /// Route a mouse scroll event to the widget under the cursor.
    ///
    /// Hit-tests `last_widget_areas` to find which widget contains (x, y),
    /// then delegates to that widget's `mouse_scroll()`. If nothing
    /// scrollable is under the cursor, the event is discarded.
    fn route_mouse_scroll(&mut self, direction: super::raw_input::ScrollDirection, x: u16, y: u16) {
        let target_uuid = self
            .last_widget_areas
            .iter()
            .find(|(_, area)| {
                let r = &area.rect;
                x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
            })
            .map(|(id, _)| id.clone());

        if let Some(panel) = target_uuid.and_then(|uuid| self.panel_pool.panels.get_mut(&uuid)) {
            panel.mouse_scroll(direction);
        }
    }

    /// Handle a mouse button event (press, drag, release).
    ///
    /// Implements GUI-style pointer capture semantics:
    /// - Press: hit-test widget areas, set capture, dispatch to Lua
    /// - Drag: route to captured widget regardless of cursor position
    /// - Release: route to captured widget, clear capture
    /// - Modal blocking: only overlay widgets receive events when overlay active
    fn handle_mouse_event(
        &mut self,
        button: super::raw_input::MouseButton,
        event_type: MouseEventType,
        x: u16,
        y: u16,
        layout_lua: Option<&super::layout_lua::LayoutLua>,
    ) {
        let button_str = match button {
            super::raw_input::MouseButton::Left => "left",
            super::raw_input::MouseButton::Middle => "middle",
            super::raw_input::MouseButton::Right => "right",
        };

        // Resolve the target widget and local coordinates based on event type.
        let target: Option<(String, String, ratatui::layout::Rect, u16, u16)> = match event_type {
            MouseEventType::Press => {
                // Hit-test: find which widget contains (x, y)
                let hit = self.last_widget_areas.iter().find(|(_, area)| {
                    let r = &area.rect;
                    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
                });

                if let Some((widget_id, area)) = hit {
                    let local_x = x.saturating_sub(area.rect.x);
                    let local_y = y.saturating_sub(area.rect.y);

                    let capture = MouseCapture {
                        widget_id: widget_id.clone(),
                        widget_type: area.widget_type.clone(),
                        origin_rect: area.rect,
                    };
                    let result = Some((
                        widget_id.clone(),
                        area.widget_type.clone(),
                        area.rect,
                        local_x,
                        local_y,
                    ));
                    self.mouse_capture = Some(capture);
                    result
                } else {
                    None
                }
            }
            MouseEventType::Drag => self.mouse_capture.as_ref().map(|capture| {
                let local_x = x.saturating_sub(capture.origin_rect.x);
                let local_y = y.saturating_sub(capture.origin_rect.y);
                (
                    capture.widget_id.clone(),
                    capture.widget_type.clone(),
                    capture.origin_rect,
                    local_x,
                    local_y,
                )
            }),
            MouseEventType::Release => self.mouse_capture.take().map(|capture| {
                let local_x = x.saturating_sub(capture.origin_rect.x);
                let local_y = y.saturating_sub(capture.origin_rect.y);
                (
                    capture.widget_id,
                    capture.widget_type,
                    capture.origin_rect,
                    local_x,
                    local_y,
                )
            }),
        };

        let Some((widget_id, widget_type, _rect, local_x, local_y)) = target else {
            return;
        };

        let event_str = match event_type {
            MouseEventType::Press => "press",
            MouseEventType::Drag => "drag",
            MouseEventType::Release => "release",
        };

        log::debug!(
            "[MOUSE] {} {:?} on {}:{} at local ({},{})",
            event_str,
            button,
            widget_type,
            widget_id,
            local_x,
            local_y
        );

        // Dispatch to Lua
        if let Some(lua) = layout_lua {
            match lua.call_handle_mouse(
                event_str,
                button_str,
                x,
                y,
                Some(&widget_type),
                Some(&widget_id),
                local_x,
                local_y,
            ) {
                Ok(Some(ops)) => self.execute_lua_ops(ops),
                Ok(None) => {} // No handler claimed the event
                Err(e) => log::warn!("[MOUSE] Lua handle_mouse error: {e}"),
            }
        }
    }

    /// Handle a raw input event from the stdin reader.
    ///
    /// Key events go through Lua keybinding dispatch:
    /// 1. Use descriptor from raw byte parser (same format Lua expects)
    /// 2. Ctrl+Q is hardcoded as Quit (safety — works even if Lua is broken)
    /// 3. Call Lua `handle_key(descriptor, mode, context)`
    /// 4. If Lua returns an action → map to `TuiAction` and handle
    /// 5. If Lua returns `nil` in List mode → forward original raw bytes to PTY
    /// 6. If Lua returns `nil` in modal mode → ignore (swallow key)
    ///
    /// Mouse scroll events are handled directly.
    fn handle_raw_input_event(&mut self, event: InputEvent, layout_lua: Option<&LayoutLua>) {
        match event {
            InputEvent::Key {
                mut descriptor,
                mut raw_bytes,
            } => {
                // Ghostty workaround: shift+enter arrives as 0x0a (LF = ctrl+j)
                // even when kitty keyboard protocol is active. With kitty mode 1,
                // a real ctrl+j press arrives as CSI 106;5 u, so bare 0x0a can
                // only be Ghostty's broken shift+enter. Remap both the descriptor
                // (for Lua keybinding lookup) and the raw bytes (for PTY forward,
                // since the inner app expects kitty encoding).
                // See: https://github.com/ghostty-org/ghostty/issues/1850
                if self.terminal_modes.outer_kitty_enabled() && raw_bytes == [0x0a] {
                    descriptor = "shift+enter".to_string();
                    raw_bytes = b"\x1b[13;2u".to_vec();
                }

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
                            terminal_rows: self.panel_pool.terminal_dims().0,
                        };

                        match lua.call_handle_key(&descriptor, &self.mode, &context) {
                            Ok(Some(lua_action)) => {
                                log::info!(
                                    "[TUI-KEY] '{}' in mode '{}' -> action='{}' char={:?}",
                                    descriptor,
                                    self.mode,
                                    lua_action.action,
                                    lua_action.char
                                );
                                self.handle_lua_key_action(&lua_action, lua);
                                return;
                            }
                            Ok(None) => {
                                // Unbound key — forward raw bytes to PTY only in terminal mode
                                if self.mode == "terminal"
                                    && !self.has_overlay
                                    && !raw_bytes.is_empty()
                                {
                                    self.handle_pty_input(&raw_bytes);
                                } else if !raw_bytes.is_empty() {
                                    log::debug!(
                                        "[TUI-KEY] Swallowed unbound key: mode='{}' overlay={} bytes={}",
                                        self.mode, self.has_overlay, raw_bytes.len()
                                    );
                                }
                                return;
                            }
                            Err(e) => {
                                log::warn!("Lua handle_key failed: {e}");
                                if self.mode == "terminal"
                                    && !self.has_overlay
                                    && !raw_bytes.is_empty()
                                {
                                    self.handle_pty_input(&raw_bytes);
                                }
                                return;
                            }
                        }
                    }
                }

                // No Lua keybindings loaded — forward raw bytes only in terminal mode
                if self.mode == "terminal" && !self.has_overlay && !raw_bytes.is_empty() {
                    self.handle_pty_input(&raw_bytes);
                }
            }
            InputEvent::Paste { .. }
            | InputEvent::MouseScroll { .. }
            | InputEvent::Mouse { .. }
            | InputEvent::FocusGained
            | InputEvent::FocusLost => {
                // Paste, mouse scroll, mouse button, and focus events are handled
                // in poll_input() and never reach here. Arms for exhaustiveness.
            }
        }
    }

    fn focus_reporting_enabled_for_current_session(&self) -> bool {
        self.panel_pool
            .current_session_uuid()
            .and_then(|uuid| self.panel_pool.panels().get(uuid))
            .map(|panel| panel.focus_reporting())
            .unwrap_or(false)
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
            "scroll_half_up" => Some(TuiAction::ScrollUp(
                self.panel_pool.terminal_dims().0 as usize / 2,
            )),
            "scroll_half_down" => Some(TuiAction::ScrollDown(
                self.panel_pool.terminal_dims().0 as usize / 2,
            )),
            "scroll_top" => Some(TuiAction::ScrollToTop),
            "scroll_bottom" => Some(TuiAction::ScrollToBottom),
            _ => None,
        };

        if let Some(tui_action) = scroll_action {
            self.handle_tui_action(tui_action);
            return;
        }

        // Widget-intrinsic actions: route to Rust WidgetStateStore when an
        // uncontrolled widget is focused, then sync back to Lua's _tui_state.
        if self.handle_widget_action(action_str, lua_action.char, layout_lua) {
            return;
        }

        // Everything else goes through Lua compound action dispatch.
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

            // Diagnostic: log all workflow actions with context
            if matches!(
                action_str,
                "list_select" | "input_submit" | "open_menu" | "close_modal"
            ) {
                log::info!(
                    "[TUI-ACTION] action='{}' mode='{}' overlay_actions={:?} focused_list={:?} focused_input={:?}",
                    action_str, self.mode, context.overlay_actions,
                    self.focused_list_id, self.focused_input_id
                );
            }

            match layout_lua.call_on_action(action_str, &context) {
                Ok(Some(ops)) => {
                    log::info!(
                        "[TUI-ACTION] action='{}' returned {} ops",
                        action_str,
                        ops.len()
                    );
                    self.execute_lua_ops(ops);
                    return;
                }
                Ok(None) => {
                    log::info!(
                        "Lua actions returned nil for '{}' in mode '{}', no-op",
                        action_str,
                        self.mode
                    );
                }
                Err(e) => {
                    log::warn!("Lua on_action failed for '{action_str}': {e}");
                }
            }
        } else {
            log::warn!("No Lua actions module loaded, cannot handle '{action_str}'");
        }
    }

    /// Handle widget-intrinsic actions (list navigation, text input) via Rust state.
    ///
    /// Returns `true` if the action was consumed by a focused uncontrolled widget,
    /// meaning it should NOT fall through to Lua's `on_action()`.
    ///
    /// After handling, syncs the result back to Lua's `_tui_state` so workflow
    /// actions (`list_select`, `input_submit`) see the correct values.
    fn handle_widget_action(
        &mut self,
        action: &str,
        action_char: Option<char>,
        layout_lua: &LayoutLua,
    ) -> bool {
        use tui_input::InputRequest;

        // List widget actions
        if let Some(ref list_id) = self.focused_list_id.clone() {
            let new_idx = match action {
                "list_up" => Some(self.widget_states.list_state(list_id).select_up()),
                "list_down" => Some(self.widget_states.list_state(list_id).select_down()),
                _ => None,
            };
            if let Some(idx) = new_idx {
                // Sync back to Lua so list_select/workflow code reads correct index
                if let Err(e) = layout_lua.exec(&format!("_tui_state.list_selected = {idx}")) {
                    log::warn!("Failed to sync list_selected to Lua: {e}");
                }
                return true;
            }
        }

        // Input widget actions
        if let Some(ref input_id) = self.focused_input_id.clone() {
            let request = match action {
                "input_char" => action_char.map(InputRequest::InsertChar),
                "input_backspace" => Some(InputRequest::DeletePrevChar),
                "input_delete" => Some(InputRequest::DeleteNextChar),
                "input_cursor_left" => Some(InputRequest::GoToPrevChar),
                "input_cursor_right" => Some(InputRequest::GoToNextChar),
                "input_cursor_home" => Some(InputRequest::GoToStart),
                "input_cursor_end" => Some(InputRequest::GoToEnd),
                "input_word_left" => Some(InputRequest::GoToPrevWord),
                "input_word_right" => Some(InputRequest::GoToNextWord),
                "input_word_backspace" => Some(InputRequest::DeletePrevWord),
                _ => None,
            };
            if let Some(req) = request {
                self.widget_states.input_state(input_id).handle(req);
                // Sync back to Lua so input_submit reads correct buffer
                let value = self.widget_states.input_state(input_id).value().to_string();
                let escaped = value
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n");
                if let Err(e) = layout_lua.exec(&format!("_tui_state.input_buffer = \"{escaped}\""))
                {
                    log::warn!("Failed to sync input_buffer to Lua: {e}");
                }
                return true;
            }
        }

        false
    }

    /// Forward pending color queries from a TUI panel to the outer terminal.
    ///
    /// With ghostty, color queries are handled internally when colors are set
    /// on the terminal. This is now a no-op but kept for call-site compatibility.
    fn forward_panel_color_queries(_panel: &super::terminal_panel::TerminalPanel) {
        // Ghostty color queries are answered locally by the parser/write_pty path.
        // No forwarding needed.
    }

    /// Detect OSC color response sequences (OSC 4/10/11/12 with rgb: or # values).
    ///
    /// These arrive on stdin when the outer terminal responds to a probe we
    /// forwarded. They are routed back to the focused PTY as input.
    fn is_osc_color_response(data: &[u8]) -> bool {
        // Must start with ESC ] and be at least ESC ] <code> ; <value> BEL
        if data.len() < 7 || data[0] != 0x1b || data[1] != b']' {
            return false;
        }
        let seq = if data.ends_with(b"\x07") {
            &data[..data.len() - 1]
        } else if data.ends_with(b"\x1b\\") {
            &data[..data.len() - 2]
        } else {
            data
        };
        let payload = &seq[2..];
        let Some(first_sep) = payload.iter().position(|byte| *byte == b';') else {
            return false;
        };
        let code = &payload[..first_sep];
        let value = &payload[first_sep + 1..];
        let value = if code == b"4" {
            let Some(second_sep) = value.iter().position(|byte| *byte == b';') else {
                return false;
            };
            let index = &value[..second_sep];
            if std::str::from_utf8(index)
                .ok()
                .and_then(|s| s.parse::<u8>().ok())
                .is_none()
            {
                return false;
            }
            &value[second_sep + 1..]
        } else if matches!(code, b"10" | b"11" | b"12") {
            value
        } else {
            return false;
        };

        value.starts_with(b"rgb:") || value.starts_with(b"#")
    }

    /// Detect Ghostty color-scheme responses (CSI ? 997 ; {1|2} n).
    fn is_color_scheme_response(data: &[u8]) -> bool {
        matches!(data, b"\x1b[?997;1n" | b"\x1b[?997;2n")
    }

    /// Extract complete OSC color responses from the current stdin chunk.
    ///
    /// Replies can be split across multiple reads, so we buffer when a chunk
    /// begins an OSC sequence and only return fully terminated responses.
    fn take_osc_color_responses(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        const MAX_OSC_BUFFER_BYTES: usize = 512;

        let should_buffer = !self.osc_color_response_buf.is_empty() || data.starts_with(b"\x1b]");
        if !should_buffer {
            return if Self::is_osc_color_response(data) {
                vec![data.to_vec()]
            } else {
                Vec::new()
            };
        }

        if data.len() >= MAX_OSC_BUFFER_BYTES {
            self.osc_color_response_buf
                .extend_from_slice(&data[data.len() - MAX_OSC_BUFFER_BYTES..]);
        } else {
            let overflow = self
                .osc_color_response_buf
                .len()
                .saturating_add(data.len())
                .saturating_sub(MAX_OSC_BUFFER_BYTES);
            if overflow > 0 {
                self.osc_color_response_buf.drain(..overflow);
            }
            self.osc_color_response_buf.extend_from_slice(data);
        }

        Self::extract_complete_osc_sequences(&mut self.osc_color_response_buf)
            .into_iter()
            .filter(|seq| Self::is_osc_color_response(seq))
            .collect()
    }

    fn observe_terminal_color_response(
        &mut self,
        index: usize,
        color: crate::terminal::Rgb,
    ) -> usize {
        if let Ok(mut cache) = self.color_cache.lock() {
            cache.insert(index, color);
        }
        self.local_color_profile_dirty = true;
        index
    }

    fn enqueue_local_color_probe(&mut self) {
        for index in [256usize, 257usize, 258usize] {
            *self.local_color_probe_pending.entry(index).or_insert(0) += 1;
        }
        for index in 0usize..256usize {
            *self.local_color_probe_pending.entry(index).or_insert(0) += 1;
        }
    }

    fn request_local_color_probe(&mut self) {
        self.enqueue_local_color_probe();
        let mut stdout = std::io::stdout();
        if let Err(e) = stdout.write_all(crate::hub::terminal_profile::full_probe_bytes()) {
            log::warn!("[PTY-PROBE] failed to write local color probe: {e}");
            self.local_color_probe_pending.clear();
            self.flush_pending_focus_in();
            return;
        }
        if let Err(e) = stdout.flush() {
            log::warn!("[PTY-PROBE] failed to flush local color probe: {e}");
            self.local_color_probe_pending.clear();
            self.flush_pending_focus_in();
            return;
        }
        self.last_local_color_probe_at = Some(Instant::now());
    }

    fn consume_local_color_probe(&mut self, index: usize) -> bool {
        let Some(count) = self.local_color_probe_pending.get_mut(&index) else {
            return false;
        };
        if *count == 0 {
            self.local_color_probe_pending.remove(&index);
            return false;
        }
        *count -= 1;
        if *count == 0 {
            self.local_color_probe_pending.remove(&index);
        }
        true
    }

    fn has_pending_focus_sync_probe(&self) -> bool {
        Self::FOCUS_SYNC_COLOR_INDICES.iter().any(|index| {
            self.local_color_probe_pending
                .get(index)
                .copied()
                .unwrap_or(0)
                > 0
        })
    }

    fn current_color_profile_message(&self) -> Option<serde_json::Value> {
        let session_uuid = self.panel_pool.current_session_uuid()?.to_string();
        let colors = self.color_cache.lock().ok()?.clone();
        Some(serde_json::json!({
            "type": "terminal_color_profile",
            "session_uuid": session_uuid,
            "colors": colors,
        }))
    }

    fn publish_terminal_color_profile_for_current_session(&self) {
        let Some(msg) = self.current_color_profile_message() else {
            return;
        };
        let session_uuid = msg
            .get("session_uuid")
            .and_then(|value| value.as_str())
            .unwrap_or("<unknown>");
        let colors: std::collections::HashMap<usize, crate::terminal::Rgb> = msg
            .get("colors")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let color_count = colors.len();
        let bg = colors
            .get(&257usize)
            .map(|rgb| serde_json::json!({"r": rgb.r, "g": rgb.g, "b": rgb.b}))
            .unwrap_or(serde_json::Value::Null);
        log::debug!(
            "[PTY-PROFILE] publishing local profile session={} colors={} bg={}",
            session_uuid,
            color_count,
            bg
        );
        if let Err(error) = self.request_tx.send(TuiRequest::LuaMessage(msg)) {
            log::error!("Failed to send terminal color profile: {error}");
        }
    }

    fn flush_terminal_color_profile_updates(&mut self) {
        if !self.local_color_profile_dirty {
            self.flush_pending_focus_in();
            return;
        }

        self.local_color_profile_dirty = false;
        self.publish_terminal_color_profile_for_current_session();
        self.flush_pending_focus_in();
    }

    fn flush_pending_focus_in(&mut self) {
        let Some(session_uuid) = self.pending_focus_in_session.clone() else {
            return;
        };
        if self.has_pending_focus_sync_probe() {
            return;
        }
        if self.panel_pool.current_session_uuid() != Some(session_uuid.as_str()) {
            self.pending_focus_in_session = None;
            return;
        }
        if !(self.mode == "terminal"
            && !self.has_overlay
            && self.terminal_modes.terminal_focused()
            && self.focus_reporting_enabled_for_current_session())
        {
            return;
        }

        self.pending_focus_in_session = None;
        log::debug!("[FOCUS] forwarding deferred \\x1b[I to PTY session={session_uuid}");
        self.handle_pty_input(b"\x1b[I");
    }

    fn maybe_periodic_local_color_probe(&mut self) {
        if !self.terminal_modes.terminal_focused() {
            return;
        }
        if self.panel_pool.current_session_uuid().is_none() {
            return;
        }
        if !self.local_color_probe_pending.is_empty() {
            return;
        }
        if self
            .last_local_color_probe_at
            .is_some_and(|last| last.elapsed() < Self::PERIODIC_LOCAL_COLOR_PROBE_INTERVAL)
        {
            return;
        }

        self.request_local_color_probe();
    }

    fn parse_color_cache_entry(data: &[u8]) -> Option<(usize, crate::terminal::Rgb)> {
        let payload = Self::osc_payload(data)?;
        let first_sep = payload.iter().position(|byte| *byte == b';')?;
        let (code, value) = (&payload[..first_sep], &payload[first_sep + 1..]);
        let (index, rgb_value) = match code {
            b"4" => {
                let second_sep = value.iter().position(|byte| *byte == b';')?;
                let (palette_index, rgb_value) = (&value[..second_sep], &value[second_sep + 1..]);
                let index = std::str::from_utf8(palette_index)
                    .ok()?
                    .parse::<usize>()
                    .ok()?;
                (index, rgb_value)
            }
            b"10" => (256usize, value),
            b"11" => (257usize, value),
            b"12" => (258usize, value),
            _ => return None,
        };
        let rgb_str = rgb_value.strip_prefix(b"rgb:")?;
        let rgb_str = std::str::from_utf8(rgb_str).ok()?;
        let mut parts = rgb_str.split('/');
        let r = u16::from_str_radix(parts.next()?, 16).ok()?;
        let g = u16::from_str_radix(parts.next()?, 16).ok()?;
        let b = u16::from_str_radix(parts.next()?, 16).ok()?;
        Some((
            index,
            crate::terminal::Rgb::new((r >> 8) as u8, (g >> 8) as u8, (b >> 8) as u8),
        ))
    }

    fn osc_payload(seq: &[u8]) -> Option<&[u8]> {
        if seq.len() < 4 || seq[0] != 0x1b || seq[1] != b']' {
            return None;
        }
        if seq.ends_with(&[0x07]) {
            return Some(&seq[2..seq.len() - 1]);
        }
        if seq.len() >= 4 && seq.ends_with(&[0x1b, b'\\']) {
            return Some(&seq[2..seq.len() - 2]);
        }
        None
    }

    fn extract_complete_osc_sequences(buffer: &mut Vec<u8>) -> Vec<Vec<u8>> {
        let mut sequences = Vec::new();
        let mut idx = 0usize;
        let mut remainder_start = None;

        while idx + 1 < buffer.len() {
            if buffer[idx] == 0x1b && buffer[idx + 1] == b']' {
                let start = idx;
                let mut scan = idx + 2;
                let mut end = None;

                while scan < buffer.len() {
                    if buffer[scan] == 0x07 {
                        end = Some(scan + 1);
                        break;
                    }
                    if buffer[scan] == 0x1b && scan + 1 < buffer.len() && buffer[scan + 1] == b'\\'
                    {
                        end = Some(scan + 2);
                        break;
                    }
                    scan += 1;
                }

                if let Some(end_idx) = end {
                    sequences.push(buffer[start..end_idx].to_vec());
                    idx = end_idx;
                    continue;
                }

                remainder_start = Some(start);
                break;
            }

            idx += 1;
        }

        let remainder = if let Some(start) = remainder_start {
            buffer[start..].to_vec()
        } else if buffer.last() == Some(&0x1b) {
            vec![0x1b]
        } else {
            Vec::new()
        };

        buffer.clear();
        buffer.extend_from_slice(&remainder);
        sequences
    }

    /// Send raw PTY input bytes directly to the PTY writer.
    ///
    /// Bypasses Lua entirely — no JSON serialization, no `from_utf8_lossy`.
    /// Uses `current_session_uuid` to route to the correct PTY.
    /// No-op if no PTY is currently focused.
    fn handle_pty_input(&mut self, data: &[u8]) {
        if let Some(uuid) = self.panel_pool.current_session_uuid.clone() {
            log::trace!(
                "[PTY-FWD] Sending {} bytes to session={} (overlay={})",
                data.len(),
                uuid,
                self.has_overlay
            );
            if let Err(e) = self.request_tx.send(TuiRequest::PtyInput {
                session_uuid: uuid,
                data: data.to_vec(),
            }) {
                log::error!("Failed to send PTY input: {e}");
            }
        } else {
            log::warn!("[PTY-FWD] Dropped {} bytes: no focused session", data.len());
        }
    }

    /// Notify the hub of focus state change for the current session.
    ///
    /// Always sent regardless of whether the child PTY requested focus
    /// reporting. This ensures `pty_clients.focused` stays accurate for
    /// notification suppression even when the child app doesn't enable
    /// `CSI ? 1004 h`.
    fn send_focus_changed(&self, focused: bool) {
        if let Some(uuid) = self.panel_pool.current_session_uuid.clone() {
            let _ = self.request_tx.send(TuiRequest::FocusChanged {
                session_uuid: uuid,
                focused,
            });
        }
    }

    /// Recompute notification focus from current state and send a
    /// `FocusChanged` message only when the value actually transitions.
    ///
    /// Focused = subscribed to a PTY AND no overlay AND terminal window
    /// has OS-level focus.
    fn sync_notification_focus(&mut self) {
        let should_focus = self.panel_pool.current_session_uuid.is_some()
            && !self.has_overlay
            && self.terminal_modes.terminal_focused();
        if should_focus != self.notification_focused {
            self.notification_focused = should_focus;
            self.send_focus_changed(should_focus);
        }
    }

    /// Mirror terminal modes from PTY to the outer terminal.
    ///
    /// Handle resize event.
    ///
    /// Updates local state only. The next render cycle will call
    /// `sync_widget_dims()` which sends per-terminal resize through
    /// terminal subscriptions → `pty_clients.update()`.
    fn handle_resize(&mut self, rows: u16, cols: u16) {
        let (prev_rows, prev_cols) = self.panel_pool.terminal_dims();
        if (prev_rows, prev_cols) != (rows, cols) {
            log::info!(
                "[TUI] resize inner area: {}x{} -> {}x{}",
                prev_cols,
                prev_rows,
                cols,
                rows
            );
        }
        self.panel_pool.handle_resize(rows, cols);
    }

    /// Poll PTY output and Lua events from Hub output channel.
    ///
    /// Hub sends `TuiOutput` messages through the channel: binary PTY data
    /// from Lua forwarder tasks and JSON events from `tui.send()`. TuiRunner
    /// processes them here (feeding to AlacrittyParser, handling Lua messages, etc.).
    fn poll_pty_events(&mut self, layout_lua: Option<&LayoutLua>) {
        use tokio::sync::mpsc::error::TryRecvError;

        // Drain all pending events (no arbitrary cap).
        loop {
            match self.output_rx.try_recv() {
                Ok(TuiOutput::Scrollback {
                    session_uuid,
                    rows,
                    cols,
                    data,
                    kitty_enabled,
                }) => {
                    self.dirty = true;
                    let panel = self.panel_pool.resolve_panel(&session_uuid);
                    let before = panel.state();
                    let before_dims = panel.dims();
                    let (applied_rows, applied_cols, used_local_dims) = if rows >= 2 && cols >= 2 {
                        (rows, cols, false)
                    } else {
                        let (local_rows, local_cols) = panel.dims();
                        (local_rows, local_cols, true)
                    };
                    panel.on_scrollback_with_dims(applied_rows, applied_cols, &data);
                    Self::forward_panel_color_queries(panel);
                    let after = panel.state();
                    let after_dims = panel.dims();
                    let is_focused = self.panel_pool.is_focused(&session_uuid);
                    if is_focused {
                        self.terminal_modes.on_kitty_changed(kitty_enabled);
                    }
                    log::debug!(
                        "Processed {} bytes of scrollback for {} (state {:?}->{:?}, snapshot={}x{}, panel_dims {}x{} -> {}x{}, kitty={}{}{})",
                        data.len(),
                        session_uuid,
                        before,
                        after,
                        applied_cols,
                        applied_rows,
                        before_dims.1,
                        before_dims.0,
                        after_dims.1,
                        after_dims.0,
                        kitty_enabled,
                        if is_focused { ", applied" } else { "" },
                        if used_local_dims { " (legacy dims fallback)" } else { "" }
                    );
                }
                Ok(TuiOutput::Output { session_uuid, data }) => {
                    self.dirty = true;
                    let panel = self.panel_pool.resolve_panel(&session_uuid);
                    panel.on_output(&data);
                    Self::forward_panel_color_queries(panel);
                }
                Ok(TuiOutput::OutputBatch {
                    session_uuid,
                    chunks,
                }) => {
                    self.dirty = true;
                    let panel = self.panel_pool.resolve_panel(&session_uuid);
                    for data in &chunks {
                        panel.on_output(data);
                    }
                    Self::forward_panel_color_queries(panel);
                }
                Ok(TuiOutput::ProcessExited {
                    session_uuid,
                    exit_code,
                }) => {
                    self.dirty = true;
                    log::info!("PTY process exited with code {:?}", exit_code);
                    if self.panel_pool.is_focused(&session_uuid) {
                        self.terminal_modes.clear_inner_kitty();
                    }
                }
                Ok(TuiOutput::Message(value)) => {
                    self.dirty = true;
                    self.dispatch_hub_event(value, layout_lua);
                }
                Ok(TuiOutput::Binary(data)) => {
                    log::debug!("[TUI] Received binary data ({} bytes)", data.len());
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    log::debug!("PTY output channel disconnected");
                    if self.terminal_modes.terminal_focused() {
                        if self.focus_reporting_enabled_for_current_session() {
                            self.handle_pty_input(b"\x1b[O");
                        }
                    }
                    let msgs = self.panel_pool.disconnect_all();
                    for msg in msgs {
                        self.send_msg(msg);
                    }
                    self.sync_notification_focus();
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
    fn dispatch_hub_event(&mut self, msg: serde_json::Value, layout_lua: Option<&LayoutLua>) {
        let event_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // Wire protocol entity envelopes (`entity_snapshot`/`upsert`/
        // `patch`/`remove`) belong to TuiEntityStores. Lua owns workflow
        // state; it should not maintain a second entity cache.
        if super::entity_stores::TuiEntityStores::handles_frame(event_type) {
            if self.entity_stores.apply_frame(&msg) {
                self.dirty = true;
            }
            return;
        }

        // Handle kitty_changed directly — sets outer terminal keyboard mode.
        // Only apply if the message is for the currently focused PTY.
        if event_type == "kitty_changed" {
            let msg_uuid = msg.get("session_uuid").and_then(|v| v.as_str());
            if msg_uuid == self.panel_pool.current_session_uuid() {
                if let Some(enabled) = msg.get("enabled").and_then(|v| v.as_bool()) {
                    self.terminal_modes.on_kitty_changed(enabled);
                }
            }
            return;
        }

        // Attach bridge reconnected to the hub socket after EOF.
        // Reset panel subscription state so the next render pass re-subscribes
        // active terminal widgets on the fresh socket connection.
        if event_type == "bridge_reconnected" {
            self.terminal_modes.clear_inner_kitty();
            self.panel_pool.reset_for_reattach();
            self.widget_states.reset_all();
            self.focused_list_id = None;
            self.focused_input_id = None;
            self.overlay_list_actions.clear();
            self.has_overlay = false;
            self.dirty = true;
            log::debug!("[TUI] Bridge reconnected: reset to attach-like state");
        }

        if event_type == "terminal_attach" {
            let session_uuid = msg
                .get("session_uuid")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let state = msg.get("state").and_then(|v| v.as_str()).unwrap_or("");
            log::debug!(
                "[TUI] terminal_attach session={} state={}",
                session_uuid,
                state
            );
            return;
        }

        // Handle focus_reporting_changed — PTY toggled focus reporting mode.
        // When newly enabled and the session is focused, send immediate focus state.
        if event_type == "focus_reporting_changed" {
            let enabled = msg
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let msg_uuid = msg.get("session_uuid").and_then(|v| v.as_str());
            if enabled {
                if msg_uuid == self.panel_pool.current_session_uuid() {
                    let seq = if self.terminal_modes.terminal_focused() {
                        b"\x1b[I" as &[u8]
                    } else {
                        b"\x1b[O"
                    };
                    log::debug!(
                        "[FOCUS] Focus reporting enabled, responding with {}",
                        if self.terminal_modes.terminal_focused() {
                            "focused"
                        } else {
                            "unfocused"
                        }
                    );
                    self.handle_pty_input(seq);
                }
            }
            return;
        }

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
    fn render(&mut self, layout_lua: Option<&LayoutLua>, layout_error: Option<&str>) -> Result<()> {
        use super::render::{render, RenderContext};

        self.panel_pool.refresh_panel_colors();

        // Read scroll state from the focused panel (no mutex needed)
        let (scroll_offset, is_scrolled) = self
            .panel_pool
            .focused_panel()
            .map(|panel| (panel.scroll_offset(), panel.is_scrolled()))
            .unwrap_or((0, false));

        // Connection code is cached from Lua responses (requested via show_connection_code action)

        // Build render context from TuiRunner state
        let ctx = RenderContext {
            // Note: mode, list_selected, input_buffer, selected_agent_index live in Lua's _tui_state
            error_message: self.error_message.as_deref(),
            connection_code: self.connection_code.as_ref(),
            bundle_used: false, // TuiRunner doesn't track this - would need from Hub

            // Terminal State — panels own parsers directly (no mutex)
            panels: &self.panel_pool.panels,
            scroll_offset,
            is_scrolled,

            // Cursor — hardware cursor only for focused terminal in terminal mode
            focused_session_uuid: self.panel_pool.current_session_uuid.as_deref(),
            is_terminal_mode: self.mode == "terminal",

            // Status Indicators - TuiRunner doesn't track these, use defaults
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,

            // Terminal dimensions for responsive layout
            terminal_cols: self.panel_pool.terminal_dims().1,
            terminal_rows: self.panel_pool.terminal_dims().0,

            // Widget area tracking (populated during rendering)
            widget_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        };

        // Try Lua-driven render, fall back to hardcoded Rust layout
        let lua_result = if let Some(layout_lua) = layout_lua {
            match render_with_lua(
                &mut self.terminal,
                layout_lua,
                &ctx,
                &self.entity_stores,
                &mut self.widget_states,
            ) {
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

        // Extract widget areas and drop ctx (which borrows self) before mutation
        let rendered_areas = ctx.widget_areas.borrow().clone();
        drop(ctx);

        // Cache widget areas for mouse hit-testing (used in poll_input).
        self.last_widget_areas = rendered_areas.clone();

        if let Some(ref result) = lua_result {
            if let Some(focused_uuid) = self.panel_pool.current_session_uuid() {
                if let Some(area) = rendered_areas.get(focused_uuid) {
                    log::debug!(
                        "[TUI] focused area for {} => {}x{} at ({},{})",
                        focused_uuid,
                        area.rect.width,
                        area.rect.height,
                        area.rect.x,
                        area.rect.y
                    );
                } else {
                    log::debug!(
                        "[TUI] focused area missing for {} on this frame",
                        focused_uuid
                    );
                }
            }

            // Sync subscriptions to match what the render tree declares
            let msgs = self
                .panel_pool
                .sync_subscriptions(&result.tree, &rendered_areas);
            for msg in msgs {
                self.send_msg(msg);
            }

            // Track overlay presence for input routing (PTY vs keybindings)
            self.has_overlay = result.overlay.is_some();
            self.sync_notification_focus();

            // Cache overlay list actions for menu selection dispatch
            self.overlay_list_actions = result
                .overlay
                .as_ref()
                .map(super::render_tree::extract_list_actions)
                .unwrap_or_default();

            // Extract focused uncontrolled widgets from the active tree
            // (overlay takes priority if present, otherwise main tree)
            let focus_tree = result.overlay.as_ref().unwrap_or(&result.tree);
            let (focused_list, focused_input) =
                super::render_tree::extract_focused_widgets(focus_tree);
            self.focused_list_id = focused_list;
            self.focused_input_id = focused_input;

            // Garbage collect widget state for IDs no longer in either tree
            let mut seen_ids = super::render_tree::collect_widget_ids(&result.tree);
            if let Some(ref overlay_tree) = result.overlay {
                seen_ids.extend(super::render_tree::collect_widget_ids(overlay_tree));
            }
            self.widget_states.retain_seen(&seen_ids);
        }

        // Resize parsers and PTYs to match actual widget areas from the render pass
        if !rendered_areas.is_empty() {
            let msgs = self.panel_pool.sync_widget_dims(&rendered_areas);
            for msg in msgs {
                self.send_msg(msg);
            }
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
    /// Parses JSON ops into typed [`LuaOp`] variants, then dispatches each.
    /// Parsing and validation happen in [`LuaOp::parse`]; this method handles
    /// only execution and side effects.
    pub(super) fn execute_lua_ops(&mut self, ops: Vec<serde_json::Value>) {
        use super::lua_ops::LuaOp;

        for op in LuaOp::parse_vec(ops) {
            match op {
                LuaOp::SetMode { mode } => {
                    log::info!("[TUI-OP] set_mode: {} -> {}", self.mode, mode);
                    let was_terminal = self.mode == "terminal" && !self.has_overlay;
                    self.mode = mode;
                    let now_terminal = self.mode == "terminal" && !self.has_overlay;
                    // Synthetic focus sequences on mode transition so
                    // focus-reporting PTY apps track active/inactive state.
                    if self.terminal_modes.terminal_focused() && was_terminal != now_terminal {
                        if self.focus_reporting_enabled_for_current_session() {
                            if now_terminal {
                                log::debug!(
                                    "[FOCUS] synthetic focus-in on mode change to terminal"
                                );
                                self.handle_pty_input(b"\x1b[I");
                            } else {
                                log::debug!(
                                    "[FOCUS] synthetic focus-out on mode change from terminal"
                                );
                                self.handle_pty_input(b"\x1b[O");
                            }
                        }
                    }
                    self.sync_notification_focus();
                    // Reset Rust-side widget state on mode transition
                    // (mirrors Lua's set_mode_ops resetting list_selected/input_buffer)
                    self.widget_states.reset_all();
                    self.focused_list_id = None;
                    self.focused_input_id = None;
                }
                LuaOp::SendMsg { data } => {
                    log::info!("[TUI-OP] send_msg: {}", data);
                    self.send_msg(data);
                }
                LuaOp::Quit => {
                    self.quit = true;
                }
                LuaOp::FocusTerminal {
                    agent_id,
                    session_uuid,
                } => {
                    self.execute_focus_terminal_typed(agent_id.as_deref(), session_uuid.as_deref());
                }
                LuaOp::SetConnectionCode { url, qr_ascii } => {
                    let qr_width = qr_ascii
                        .first()
                        .map(|l| l.chars().count() as u16)
                        .unwrap_or(0);
                    let qr_height = qr_ascii.len() as u16;
                    self.connection_code = Some(ConnectionCodeData {
                        url,
                        qr_ascii,
                        qr_width,
                        qr_height,
                    });
                }
                LuaOp::ClearConnectionCode => {
                    self.connection_code = None;
                }
                LuaOp::OscAlert { title, body } => {
                    // Strip control characters to prevent OSC injection.
                    let title: String = title.chars().filter(|c| !c.is_control()).collect();
                    let body: String = body.chars().filter(|c| !c.is_control()).collect();
                    // OSC 777 (rich: title + body) then OSC 9 (simple).
                    // Terminals silently ignore sequences they don't support.
                    let osc =
                        format!("\x1b]777;notify;{title};{body}\x07\x1b]9;{title}: {body}\x07");
                    let _ = std::io::Write::write_all(&mut std::io::stdout(), osc.as_bytes());
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                    log::debug!("[OSC_ALERT] title={title:?} body={body:?}");
                }
            }
        }
        // Subscription or mode may have changed — re-evaluate notification focus.
        self.sync_notification_focus();
    }

    /// Execute the `focus_terminal` op — switch to a specific agent and PTY.
    ///
    /// Delegates to [`PanelPool::focus_terminal`] for state logic, then
    /// applies the returned [`FocusEffects`] (PTY inputs, Hub messages,
    /// kitty state).
    fn execute_focus_terminal_typed(&mut self, agent_id: Option<&str>, session_uuid: Option<&str>) {
        log::info!(
            "focus_terminal: agent_id={:?}, session_uuid={:?}",
            agent_id,
            session_uuid
        );

        let terminal_focused = self.terminal_modes.terminal_focused();
        let effects = self
            .panel_pool
            .focus_terminal(agent_id, session_uuid, terminal_focused);
        self.apply_focus_effects(effects);
    }

    /// Apply side effects from a focus switch.
    ///
    /// Sends explicitly-targeted PTY inputs, Hub messages, and clears
    /// kitty state as needed.
    fn apply_focus_effects(&mut self, effects: super::panel_pool::FocusEffects) {
        // Subscribe/unsubscribe before focus sequences so pty_clients
        // has the "tui" entry registered before set_focused runs.
        for msg in effects.messages {
            self.send_msg(msg);
        }
        for input in effects.pty_inputs {
            // Always notify hub of focus state for notification suppression,
            // regardless of whether the child app requested focus reporting.
            let focused = input.data == b"\x1b[I";
            let _ = self.request_tx.send(TuiRequest::FocusChanged {
                session_uuid: input.session_uuid.clone(),
                focused,
            });
            let focus_reporting = self
                .panel_pool
                .panels()
                .get(&input.session_uuid)
                .map(|p| p.focus_reporting())
                .unwrap_or(false);
            if !focus_reporting {
                log::trace!(
                    "[FOCUS] skipping synthetic focus sequence for {} (focus reporting not enabled)",
                    input.session_uuid
                );
                continue;
            }
            if let Err(e) = self.request_tx.send(TuiRequest::PtyInput {
                session_uuid: input.session_uuid.clone(),
                data: input.data.to_vec(),
            }) {
                log::error!("Failed to send PTY input: {e}");
            }
        }
        if effects.clear_kitty {
            self.terminal_modes.clear_inner_kitty();
        }
    }

    /// JSON convenience wrapper for tests — delegates to [`Self::execute_focus_terminal_typed`].
    #[cfg(test)]
    pub(super) fn execute_focus_terminal(&mut self, op: &serde_json::Value) {
        let agent_id = op.get("agent_id").and_then(|v| v.as_str());
        let session_uuid = op.get("session_uuid").and_then(|v| v.as_str());
        self.execute_focus_terminal_typed(agent_id, session_uuid);
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
    entity_stores: &super::entity_stores::TuiEntityStores,
    widget_states: &mut super::widget_state::WidgetStateStore,
) -> Result<LuaRenderResult>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    use super::render_tree::interpret_tree;

    // Get main layout tree from Lua
    let tree = layout_lua.call_render_with_stores(ctx, Some(entity_stores))?;

    // Get optional overlay tree from Lua
    let overlay = layout_lua.call_render_overlay_with_stores(ctx, Some(entity_stores))?;

    // Render to terminal
    terminal.draw(|f| {
        let area = f.area();
        interpret_tree(&tree, f, ctx, area, widget_states);

        if let Some(ref overlay_tree) = overlay {
            interpret_tree(overlay_tree, f, ctx, area, widget_states);
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
    color_cache: ColorCache,
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

    // Create wake pipe for event-driven TUI wakeup.
    // Hub/forwarders write to wake_write_fd after sending to output_rx,
    // TuiRunner polls wake_read_fd alongside stdin.
    let mut pipe_fds = [0i32; 2];
    let pipe_ok = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } == 0;
    let (wake_read_fd, wake_write_fd) = if pipe_ok {
        // Set both ends to non-blocking: read end so drain never blocks,
        // write end so forwarder tasks never stall if pipe buffer is full.
        unsafe {
            let flags = libc::fcntl(pipe_fds[0], libc::F_GETFL);
            libc::fcntl(pipe_fds[0], libc::F_SETFL, flags | libc::O_NONBLOCK);
            let flags = libc::fcntl(pipe_fds[1], libc::F_GETFL);
            libc::fcntl(pipe_fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        hub.tui_wake_fd = Some(pipe_fds[1]);
        log::info!(
            "TUI wake pipe created: read={}, write={}",
            pipe_fds[0],
            pipe_fds[1]
        );
        (Some(pipe_fds[0]), Some(pipe_fds[1]))
    } else {
        log::warn!("Failed to create TUI wake pipe, falling back to sleep-based polling");
        (None, None)
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    let tui_shutdown = Arc::clone(&shutdown);

    let mut tui_runner = TuiRunner::new_with_color_cache(
        terminal,
        request_tx,
        output_rx,
        tui_shutdown,
        terminal_dims,
        wake_read_fd,
        color_cache,
    );

    // Load all Lua sources and create bootstrap (consumed once by run()).
    tui_runner.set_lua_bootstrap(super::hot_reload::LuaBootstrap::load());

    // Register SIGWINCH to set the resize flag (TuiRunner polls this each tick)
    #[cfg(unix)]
    {
        use signal_hook::consts::signal::SIGWINCH;
        if let Err(e) = signal_hook::flag::register(SIGWINCH, Arc::clone(&tui_runner.resize_flag)) {
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

    // Main thread: event-driven Hub loop using tokio::select!.
    // The `shutdown` Arc is bidirectional: main→TUI (for signal-triggered shutdown)
    // and TUI→main (for Ctrl+Q quit). Either side setting it to true ends both loops.
    crate::hub::run::run_event_loop(hub, shutdown_flag, Some(&shutdown))?;

    // Signal TUI thread to shutdown (in case main exited first via hub.quit or signal)
    shutdown.store(true, Ordering::SeqCst);

    // Wait for TUI thread to finish
    log::info!("Waiting for TuiRunner thread to finish...");
    if let Err(e) = tui_handle.join() {
        log::error!("TuiRunner thread panicked: {:?}", e);
    }

    // Close wake pipe fds
    if let Some(fd) = wake_read_fd {
        unsafe {
            libc::close(fd);
        }
    }
    if let Some(fd) = wake_write_fd {
        unsafe {
            libc::close(fd);
        }
    }
    hub.tui_wake_fd = None;

    log::info!("Hub event loop exiting");
    Ok(())
}

/// Probe the current outer terminal for colors that should seed TUI libghostty parsers.
pub fn probe_spawning_terminal_colors() -> ColorCache {
    TuiRunner::<CrosstermBackend<Stdout>>::probe_spawning_terminal_colors()
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
            None,     // no wake pipe in tests
        );

        // Initialize mode from Lua (same as production boot path)
        let lua = make_test_layout_with_keybindings();
        runner.mode = lua.call_initial_mode();
        // Disable focus passthrough in tests — tests don't expect PtyInput focus events.
        runner.terminal_modes.on_focus_lost();

        (runner, request_rx)
    }

    #[test]
    fn split_osc_color_response_is_buffered_until_complete() {
        let (mut runner, _request_rx) = create_test_runner();

        assert!(runner
            .take_osc_color_responses(b"\x1b]11;rgb:ffff/")
            .is_empty());
        let responses = runner.take_osc_color_responses(b"fcfc/f0f0\x07");

        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0], b"\x1b]11;rgb:ffff/fcfc/f0f0\x07");
    }

    #[test]
    fn split_osc_palette_response_is_buffered_until_complete() {
        let (mut runner, _request_rx) = create_test_runner();

        assert!(runner
            .take_osc_color_responses(b"\x1b]4;7;rgb:aaaa/")
            .is_empty());
        let responses = runner.take_osc_color_responses(b"bbbb/cccc\x07");

        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0], b"\x1b]4;7;rgb:aaaa/bbbb/cccc\x07");
    }

    #[test]
    fn osc_color_response_updates_local_color_cache() {
        let (mut runner, _request_rx) = create_test_runner();

        let (index, color) =
            TuiRunner::<TestBackend>::parse_color_cache_entry(b"\x1b]11;rgb:1111/2222/3333\x07")
                .expect("color response parsed");
        let index = runner.observe_terminal_color_response(index, color);

        let cache = runner.color_cache.lock().expect("runner color cache");
        assert_eq!(index, 257usize);
        assert_eq!(
            cache.get(&257usize).copied(),
            Some(crate::terminal::Rgb::new(0x11, 0x22, 0x33))
        );
    }

    #[test]
    fn local_color_probe_responses_are_consumed_locally() {
        let (mut runner, _request_rx) = create_test_runner();
        runner.local_color_probe_pending.insert(257usize, 1);

        assert!(runner.consume_local_color_probe(257usize));
        assert!(!runner.local_color_probe_pending.contains_key(&257usize));
        assert!(!runner.consume_local_color_probe(257usize));
    }

    #[test]
    fn local_palette_probe_responses_are_consumed_locally() {
        let (mut runner, _request_rx) = create_test_runner();
        runner.local_color_probe_pending.insert(7usize, 1);

        assert!(runner.consume_local_color_probe(7usize));
        assert!(!runner.local_color_probe_pending.contains_key(&7usize));
        assert!(!runner.consume_local_color_probe(7usize));
    }

    #[test]
    fn completed_local_probe_publishes_profile_update() {
        let (mut runner, mut request_rx) = create_test_runner();
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());
        runner.mode = "terminal".to_string();
        runner.local_color_probe_pending.insert(257usize, 1);

        let (index, color) =
            TuiRunner::<TestBackend>::parse_color_cache_entry(b"\x1b]11;rgb:1111/2222/3333\x07")
                .expect("color response parsed");
        runner.observe_terminal_color_response(index, color);
        assert!(runner.consume_local_color_probe(257usize));
        runner.flush_terminal_color_profile_updates();

        let first = request_rx.try_recv().expect("terminal profile publish");

        let msg = match first {
            TuiRequest::LuaMessage(msg) => msg,
            other => panic!("expected LuaMessage, got {other:?}"),
        };
        assert_eq!(
            msg.get("type").and_then(|value| value.as_str()),
            Some("terminal_color_profile")
        );
        assert_eq!(
            msg.get("session_uuid").and_then(|value| value.as_str()),
            Some("sess-0")
        );
        let colors: std::collections::HashMap<usize, crate::terminal::Rgb> =
            serde_json::from_value(msg.get("colors").cloned().expect("colors payload"))
                .expect("decode colors");
        assert_eq!(
            colors.get(&257usize).copied(),
            Some(crate::terminal::Rgb::new(0x11, 0x22, 0x33))
        );
    }

    #[test]
    fn local_probe_publishes_incrementally_while_palette_probe_is_still_pending() {
        let (mut runner, mut request_rx) = create_test_runner();
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());
        runner.mode = "terminal".to_string();
        runner.local_color_probe_pending.insert(257usize, 1);
        runner.local_color_probe_pending.insert(7usize, 1);

        let (index, color) =
            TuiRunner::<TestBackend>::parse_color_cache_entry(b"\x1b]11;rgb:1111/2222/3333\x07")
                .expect("color response parsed");
        runner.observe_terminal_color_response(index, color);
        assert!(runner.consume_local_color_probe(257usize));
        runner.flush_terminal_color_profile_updates();

        let first = request_rx.try_recv().expect("terminal profile publish");
        let msg = match first {
            TuiRequest::LuaMessage(msg) => msg,
            other => panic!("expected LuaMessage, got {other:?}"),
        };
        assert_eq!(
            msg.get("session_uuid").and_then(|value| value.as_str()),
            Some("sess-0")
        );
        assert!(runner.local_color_probe_pending.contains_key(&7usize));
    }

    #[test]
    fn deferred_focus_in_waits_only_for_default_colors() {
        let (mut runner, mut request_rx) = create_test_runner();
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());
        runner.mode = "terminal".to_string();
        runner.pending_focus_in_session = Some("sess-0".to_string());
        runner.terminal_modes.on_focus_gained();
        let mut panel = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        panel.on_scrollback(b"");
        panel.on_output(b"\x1b[?1004h");
        runner.panel_pool.panels.insert("sess-0".to_string(), panel);
        runner.local_color_probe_pending.insert(256usize, 1);
        runner.local_color_probe_pending.insert(257usize, 1);
        runner.local_color_probe_pending.insert(258usize, 1);
        runner.local_color_probe_pending.insert(7usize, 1);

        runner.flush_pending_focus_in();
        assert!(request_rx.try_recv().is_err());

        let (fg_index, fg_color) =
            TuiRunner::<TestBackend>::parse_color_cache_entry(b"\x1b]10;rgb:cece/cdcd/c3c3\x07")
                .expect("fg parsed");
        assert!(runner.consume_local_color_probe(fg_index));
        runner.observe_terminal_color_response(fg_index, fg_color);

        let (bg_index, bg_color) =
            TuiRunner::<TestBackend>::parse_color_cache_entry(b"\x1b]11;rgb:1010/0f0f/0f0f\x07")
                .expect("bg parsed");
        assert!(runner.consume_local_color_probe(bg_index));
        runner.observe_terminal_color_response(bg_index, bg_color);

        let (cursor_index, cursor_color) =
            TuiRunner::<TestBackend>::parse_color_cache_entry(b"\x1b]12;rgb:cece/cdcd/c3c3\x07")
                .expect("cursor parsed");
        assert!(runner.consume_local_color_probe(cursor_index));
        runner.observe_terminal_color_response(cursor_index, cursor_color);

        runner.flush_terminal_color_profile_updates();

        let profile_msg = request_rx.try_recv().expect("terminal profile publish");
        let msg = match profile_msg {
            TuiRequest::LuaMessage(msg) => msg,
            other => panic!("expected LuaMessage, got {other:?}"),
        };
        assert_eq!(
            msg.get("session_uuid").and_then(|value| value.as_str()),
            Some("sess-0")
        );

        let focus_msg = request_rx.try_recv().expect("deferred focus input");
        match focus_msg {
            TuiRequest::PtyInput { session_uuid, data } => {
                assert_eq!(session_uuid, "sess-0");
                assert_eq!(data, b"\x1b[I");
            }
            other => panic!("expected PtyInput, got {other:?}"),
        }
        assert!(runner.local_color_probe_pending.contains_key(&7usize));
    }

    #[test]
    fn local_probe_publish_happens_before_deferred_focus_in() {
        let (mut runner, mut request_rx) = create_test_runner();
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());
        runner.mode = "terminal".to_string();
        runner.pending_focus_in_session = Some("sess-0".to_string());
        runner.terminal_modes.on_focus_gained();
        let mut panel = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        panel.on_scrollback(b"");
        panel.on_output(b"\x1b[?1004h");
        runner.panel_pool.panels.insert("sess-0".to_string(), panel);
        runner.local_color_probe_pending.insert(256usize, 1);
        runner.local_color_probe_pending.insert(257usize, 1);
        runner.local_color_probe_pending.insert(258usize, 1);

        let (fg_index, fg_color) =
            TuiRunner::<TestBackend>::parse_color_cache_entry(b"\x1b]10;rgb:cece/cdcd/c3c3\x07")
                .expect("fg parsed");
        assert!(runner.consume_local_color_probe(fg_index));
        runner.observe_terminal_color_response(fg_index, fg_color);

        let (bg_index, bg_color) =
            TuiRunner::<TestBackend>::parse_color_cache_entry(b"\x1b]11;rgb:1010/0f0f/0f0f\x07")
                .expect("bg parsed");
        assert!(runner.consume_local_color_probe(bg_index));
        runner.observe_terminal_color_response(bg_index, bg_color);

        let (cursor_index, cursor_color) =
            TuiRunner::<TestBackend>::parse_color_cache_entry(b"\x1b]12;rgb:cece/cdcd/c3c3\x07")
                .expect("cursor parsed");
        assert!(runner.consume_local_color_probe(cursor_index));
        runner.observe_terminal_color_response(cursor_index, cursor_color);

        runner.flush_terminal_color_profile_updates();

        let first = request_rx.try_recv().expect("first request");
        let msg = match first {
            TuiRequest::LuaMessage(msg) => msg,
            other => panic!("expected LuaMessage first, got {other:?}"),
        };
        assert_eq!(
            msg.get("session_uuid").and_then(|value| value.as_str()),
            Some("sess-0")
        );
        let colors: std::collections::HashMap<usize, crate::terminal::Rgb> =
            serde_json::from_value(msg.get("colors").cloned().expect("colors payload"))
                .expect("decode colors");
        assert_eq!(
            colors.get(&257usize).copied(),
            Some(crate::terminal::Rgb::new(16, 15, 15))
        );

        let second = request_rx.try_recv().expect("second request");
        match second {
            TuiRequest::PtyInput { session_uuid, data } => {
                assert_eq!(session_uuid, "sess-0");
                assert_eq!(data, b"\x1b[I");
            }
            other => panic!("expected PtyInput second, got {other:?}"),
        }
    }

    #[test]
    fn non_local_osc_color_response_does_not_update_local_color_cache() {
        let (mut runner, _request_rx) = create_test_runner();
        if let Ok(mut cache) = runner.color_cache.lock() {
            cache.insert(257usize, crate::terminal::Rgb::new(0xff, 0xfc, 0xf0));
        }

        let response = b"\x1b]11;rgb:1010/0f0f/0f0f\x07";
        let (index, color) = TuiRunner::<TestBackend>::parse_color_cache_entry(response)
            .expect("color response parsed");

        assert!(!runner.consume_local_color_probe(index));

        let cache = runner.color_cache.lock().expect("runner color cache");
        assert_eq!(
            cache.get(&257usize).copied(),
            Some(crate::terminal::Rgb::new(0xff, 0xfc, 0xf0))
        );
        drop(cache);
        let _ = color;
    }

    #[test]
    fn local_probe_enqueues_full_palette_but_focus_sync_depends_only_on_defaults() {
        let (mut runner, _request_rx) = create_test_runner();
        runner.enqueue_local_color_probe();

        assert_eq!(runner.local_color_probe_pending.len(), 259);
        assert!(runner.has_pending_focus_sync_probe());

        for index in [256usize, 257usize, 258usize] {
            assert!(runner.consume_local_color_probe(index));
        }

        assert!(!runner.has_pending_focus_sync_probe());
        assert!(runner.local_color_probe_pending.contains_key(&7usize));
    }

    #[test]
    fn non_osc_input_is_not_buffered_as_color_response() {
        let (mut runner, _request_rx) = create_test_runner();

        let responses = runner.take_osc_color_responses(b"\x1b[A");

        assert!(responses.is_empty());
        assert!(runner.osc_color_response_buf.is_empty());
    }

    #[test]
    fn color_scheme_response_is_detected() {
        assert!(TuiRunner::<TestBackend>::is_color_scheme_response(
            b"\x1b[?997;1n"
        ));
        assert!(TuiRunner::<TestBackend>::is_color_scheme_response(
            b"\x1b[?997;2n"
        ));
        assert!(!TuiRunner::<TestBackend>::is_color_scheme_response(
            b"\x1b[?996n"
        ));
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
            None, // no wake pipe in tests
        );

        // Initialize mode from Lua (same as production boot path)
        let lua = make_test_layout_with_keybindings();
        runner.mode = lua.call_initial_mode();
        // Disable focus passthrough in tests — tests don't expect PtyInput focus events.
        runner.terminal_modes.on_focus_lost();

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
        let layout_source = "function render(s) return { type = 'empty' } end\nfunction render_overlay(s) return nil end\nfunction initial_mode() return 'list' end";
        let kb_source = include_str!("../../lua/ui/keybindings.lua");
        let actions_source = include_str!("../../lua/ui/actions.lua");
        let events_source = include_str!("../../lua/ui/events.lua");
        let mut lua = LayoutLua::new(layout_source).expect("test layout should load");
        // Bootstrap _tui_state (actions.lua and events.lua read from it)
        lua.load_extension(
            "_tui_state = _tui_state or { agents = {}, pending_fields = {}, available_worktrees = {}, available_agents = {}, mode = 'list', input_buffer = '', list_selected = 0 }",
            "_tui_state_init",
        ).expect("_tui_state bootstrap should succeed");
        lua.preload_module(
            "ui.workspace_helpers",
            include_str!("../../lua/ui/workspace_helpers.lua"),
        )
        .expect("workspace_helpers.lua should preload");
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
    fn process_key_with_lua(
        runner: &mut TuiRunner<TestBackend>,
        event: InputEvent,
        lua: &LayoutLua,
    ) {
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
    /// Stub overlay_list_actions and focused list widget for tests.
    ///
    /// In production, Lua renders the menu overlay and Rust extracts action
    /// strings from the render tree + focused widget IDs. Tests don't run
    /// the render pass, so we stub both the action cache and focused widget.
    fn stub_menu_actions(runner: &mut TuiRunner<TestBackend>) {
        runner.overlay_list_actions =
            vec!["new_agent".to_string(), "show_connection_code".to_string()];
        // Set up focused list widget so Rust handles list_up/list_down
        runner.focused_list_id = Some("menu".to_string());
        runner
            .widget_states
            .list_state("menu")
            .set_selectable_count(2);
    }

    /// Stub focused input widget for text entry tests.
    fn stub_input_focus(runner: &mut TuiRunner<TestBackend>, id: &str) {
        runner.focused_input_id = Some(id.to_string());
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
        lua.eval_usize("return _tui_state.list_selected")
            .unwrap_or(0)
    }

    /// Read `_tui_state.input_buffer` from Lua.
    fn lua_input_buffer(lua: &LayoutLua) -> String {
        lua.eval_string("return _tui_state.input_buffer")
            .unwrap_or_default()
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
    // PTY broadcast -> poll_pty_events() -> panel.on_output()

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

    /// Verifies Ctrl+P opens the menu from List mode.
    #[test]
    fn test_e2e_ctrl_p_opens_menu() {
        let (mut runner, _cmd_rx) = create_test_runner();

        assert_eq!(runner.mode(), "list");

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
        assert_eq!(runner.mode(), "list");
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

    /// Verifies plain keys in List mode go to PTY (via Lua returning nil).
    #[test]
    fn test_e2e_list_mode_keys_go_to_pty() {
        let lua = make_test_layout_with_keybindings();
        let context = KeyContext::default();

        // Plain 'q' should NOT match any binding in list mode -> nil -> PTY
        let result = lua.call_handle_key("q", "list", &context).unwrap();
        assert!(
            result.is_none(),
            "Plain 'q' should return nil (PTY forward)"
        );

        // Plain 'p' should NOT match any binding in list mode -> nil -> PTY
        let result = lua.call_handle_key("p", "list", &context).unwrap();
        assert!(
            result.is_none(),
            "Plain 'p' should return nil (PTY forward)"
        );
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
        assert_eq!(runner.mode(), "list");
    }

    /// Verifies the `restart_hub` menu action sends the right message and does
    /// NOT set `runner.quit`.
    ///
    /// # Bug context
    ///
    /// The original implementation returned `{ op = "quit" }` alongside the
    /// `restart_hub` send_msg. The TUI thread calls `shutdown.store(true)` as
    /// soon as `runner.quit` is set, which races with the hub's two-hop
    /// restart processing. If the shutdown flag fires first, the hub can exit
    /// before the restart request is committed, leaving session recovery in an
    /// inconsistent state.
    ///
    /// The fix removes `{ op = "quit" }` from the action, letting hub.quit = true
    /// propagate via the shared shutdown flag after the restart request completes.
    ///
    /// This test verifies:
    /// 1. `runner.quit` is NOT set after selecting restart_hub (no race).
    /// 2. A `restart_hub` message is sent to Hub.
    /// 3. The menu is closed (mode returns to list/terminal, not "menu").
    #[test]
    fn test_e2e_restart_hub_does_not_quit_tui() {
        let (mut runner, _output_tx, mut request_rx, shutdown) =
            create_test_runner_with_mock_client();
        let lua = make_test_layout_with_keybindings();

        // Open menu and inject restart_hub as a menu item.
        process_key_with_lua(&mut runner, make_key_ctrl('p'), &lua);
        assert_eq!(runner.mode(), "menu");

        runner.overlay_list_actions = vec!["restart_hub".to_string()];
        runner.focused_list_id = Some("menu".to_string());
        runner
            .widget_states
            .list_state("menu")
            .set_selectable_count(1);

        // Select restart_hub with Enter.
        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        // Give the responder thread a moment to forward the message.
        thread::sleep(Duration::from_millis(10));

        // 1. TUI quit flag must NOT be set — hub controls shutdown.
        assert!(
            !runner.quit,
            "restart_hub must not set runner.quit (would race with GracefulRestart)"
        );

        // 2. A restart_hub message must have been sent to Hub.
        let mut found_restart = false;
        while let Ok(req) = request_rx.try_recv() {
            let msg = unwrap_lua_msg(req);
            if let Some(data) = msg.get("data") {
                if data.get("type").and_then(|t| t.as_str()) == Some("restart_hub") {
                    found_restart = true;
                    break;
                }
            }
        }
        assert!(
            found_restart,
            "restart_hub action must send a restart_hub message to Hub"
        );

        // 3. Menu must be closed — mode exits "menu" so the operator sees a clean TUI
        //    while waiting for Hub to process the graceful restart.
        assert_ne!(
            runner.mode(),
            "menu",
            "menu must close after restart_hub (mode was \"{}\")",
            runner.mode()
        );

        shutdown.store(true, Ordering::Relaxed);
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
        let (mut runner, _output_tx, mut request_rx, shutdown) =
            create_test_runner_with_mock_client();
        let lua = make_test_layout_with_keybindings();

        // 1. Open menu and navigate to New Agent using cached overlay actions
        process_key_with_lua(&mut runner, make_key_ctrl('p'), &lua);
        stub_menu_actions(&mut runner);

        let new_agent_idx =
            find_action_index(&runner, "new_agent").expect("new_agent should be in menu");
        navigate_to_menu_index_with_lua(&mut runner, &lua, new_agent_idx);
        assert_eq!(lua_list_selected(&lua), new_agent_idx);

        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        // Small delay to let responder process messages
        thread::sleep(Duration::from_millis(10));

        assert_eq!(
            runner.mode(),
            "new_agent_select_target",
            "Should enter agent config selection"
        );

        // Simulate single agent config response (auto-skips to workspace selection)
        {
            let agent_config_event = serde_json::json!({ "agents": ["claude"] });
            let ctx = crate::tui::layout_lua::ActionContext::default();
            let ops = lua
                .call_on_hub_event("agent_config", &agent_config_event, &ctx)
                .unwrap()
                .unwrap();
            runner.execute_lua_ops(ops);
        }

        assert_eq!(
            runner.mode(),
            "new_agent_select_workspace",
            "Should auto-advance to workspace selection"
        );

        // 2a. Select "Create New Workspace" (index 0) → name input
        runner.focused_list_id = Some("workspace_list".to_string());
        runner
            .widget_states
            .list_state("workspace_list")
            .set_selectable_count(1);
        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        assert_eq!(
            runner.mode(),
            "new_workspace_name_input",
            "Should enter workspace name input"
        );

        // 2a-ii. Submit empty workspace name → worktree selection
        stub_input_focus(&mut runner, "workspace_name_input");
        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        assert_eq!(
            runner.mode(),
            "new_agent_select_worktree",
            "Should advance to worktree selection after workspace name"
        );

        // 2b. Select "Create new worktree" (index 1, after "Use Main Branch")
        // Set up worktree list focus (2 items: "Use Main Branch", "Create new worktree")
        runner.focused_list_id = Some("worktree_list".to_string());
        runner
            .widget_states
            .list_state("worktree_list")
            .set_selectable_count(2);
        process_key_with_lua(&mut runner, make_key_down(), &lua);
        assert_eq!(lua_list_selected(&lua), 1);
        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        assert_eq!(runner.mode(), "new_agent_create_worktree");

        // 3. Type issue name
        stub_input_focus(&mut runner, "worktree_input");
        for c in "issue-42".chars() {
            process_key_with_lua(&mut runner, make_key_char(c), &lua);
        }
        assert_eq!(lua_input_buffer(&lua), "issue-42");

        // 4. Submit issue name
        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        assert_eq!(runner.mode(), "new_agent_prompt");
        // pending_fields now live in Lua's _tui_state, verified by actions.lua tests

        // 5. Type prompt and submit
        stub_input_focus(&mut runner, "prompt_input");
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
                    assert_eq!(data.get("prompt").and_then(|v| v.as_str()), Some("Fix bug"));
                    found_create = true;
                    break;
                }
            }
        }
        assert!(found_create, "create_agent JSON message should be sent");

        // Modal closes after submit — stays in list mode until agent_created
        // event arrives and selects the agent (which sets terminal mode).
        assert_eq!(
            runner.mode(),
            "list",
            "Should be list mode until agent_created event selects the agent"
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
        let (mut runner, _output_tx, mut request_rx, shutdown) =
            create_test_runner_with_mock_client();
        let lua = make_test_layout_with_keybindings();

        // Pre-populate worktrees in _tui_state (normally delivered via worktree_list event)
        lua.load_extension(
            r#"_tui_state.available_worktrees = {
                { path = "/path/worktree-1", branch = "feature-branch" },
                { path = "/path/worktree-2", branch = "bugfix-branch" },
            }"#,
            "test_worktrees",
        )
        .unwrap();

        // Open menu and navigate to New Agent using cached overlay actions
        process_key_with_lua(&mut runner, make_key_ctrl('p'), &lua);
        stub_menu_actions(&mut runner);

        let new_agent_idx =
            find_action_index(&runner, "new_agent").expect("new_agent should be in menu");
        navigate_to_menu_index_with_lua(&mut runner, &lua, new_agent_idx);
        assert_eq!(lua_list_selected(&lua), new_agent_idx);

        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        thread::sleep(Duration::from_millis(10));
        assert_eq!(runner.mode(), "new_agent_select_target");

        {
            let target_event = serde_json::json!({
                "targets": [
                    { "id": "tgt_trybotster", "name": "trybotster", "path": "/tmp/trybotster", "current_branch": "main" }
                ]
            });
            let ctx = crate::tui::layout_lua::ActionContext::default();
            let ops = lua
                .call_on_hub_event("spawn_target_list", &target_event, &ctx)
                .unwrap()
                .unwrap();
            runner.execute_lua_ops(ops);
        }
        runner.focused_list_id = Some("spawn_target_list".to_string());
        runner
            .widget_states
            .list_state("spawn_target_list")
            .set_selectable_count(1);
        process_key_with_lua(&mut runner, make_key_enter(), &lua);
        assert_eq!(runner.mode(), "new_agent_select_agent");

        // Simulate single agent config response (auto-skips to workspace selection)
        {
            let agent_config_event = serde_json::json!({ "agents": ["claude"] });
            let ctx = crate::tui::layout_lua::ActionContext::default();
            let ops = lua
                .call_on_hub_event("agent_config", &agent_config_event, &ctx)
                .unwrap()
                .unwrap();
            runner.execute_lua_ops(ops);
        }
        assert_eq!(runner.mode(), "new_agent_select_workspace");

        // Select "Create New Workspace" (index 0) → name input
        runner.focused_list_id = Some("workspace_list".to_string());
        runner
            .widget_states
            .list_state("workspace_list")
            .set_selectable_count(1);
        process_key_with_lua(&mut runner, make_key_enter(), &lua);
        assert_eq!(runner.mode(), "new_workspace_name_input");

        // Submit empty workspace name → worktree selection
        stub_input_focus(&mut runner, "workspace_name_input");
        process_key_with_lua(&mut runner, make_key_enter(), &lua);
        assert_eq!(runner.mode(), "new_agent_select_worktree");

        // Navigate to first existing worktree (index 2, after "Use Main Branch" and "Create New Worktree")
        runner.overlay_list_actions = vec![
            "main".to_string(),
            "create_new".to_string(),
            "worktree_0".to_string(),
            "worktree_1".to_string(),
        ];
        runner.focused_list_id = Some("worktree_list".to_string());
        runner
            .widget_states
            .list_state("worktree_list")
            .set_selectable_count(4);
        process_key_with_lua(&mut runner, make_key_down(), &lua);
        process_key_with_lua(&mut runner, make_key_down(), &lua);
        assert_eq!(lua_list_selected(&lua), 2);

        // Select existing worktree
        process_key_with_lua(&mut runner, make_key_enter(), &lua);

        thread::sleep(Duration::from_millis(10));

        // Modal closes after selection — stays in list mode until agent_created event
        assert_eq!(
            runner.mode(),
            "list",
            "Should be list mode until agent_created event selects the agent"
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
        assert_eq!(runner.mode(), "list");

        // Cancel at issue input
        runner.mode = "new_agent_create_worktree".to_string();
        let _ = lua.exec(
            "_tui_state.mode = 'new_agent_create_worktree'; _tui_state.input_buffer = 'partial'",
        );
        process_key_with_lua(&mut runner, make_key_escape(), &lua);
        assert_eq!(runner.mode(), "list");
        assert!(
            lua_input_buffer(&lua).is_empty(),
            "Buffer should be cleared"
        );

        // Cancel at prompt
        runner.mode = "new_agent_prompt".to_string();
        let _ = lua.exec("_tui_state.mode = 'new_agent_prompt'");
        process_key_with_lua(&mut runner, make_key_escape(), &lua);
        assert_eq!(runner.mode(), "list");

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
        stub_input_focus(&mut runner, "worktree_input");

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

        // Stub overlay_list_actions and focused list for navigation test.
        runner.overlay_list_actions = vec![
            "main".to_string(),
            "create_new".to_string(),
            "worktree_0".to_string(),
            "worktree_1".to_string(),
        ];
        runner.mode = "new_agent_select_worktree".to_string();
        let _ =
            lua.exec("_tui_state.mode = 'new_agent_select_worktree'; _tui_state.list_selected = 0");
        runner.focused_list_id = Some("worktree_list".to_string());
        runner
            .widget_states
            .list_state("worktree_list")
            .set_selectable_count(4);

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
            .call_handle_key("shift+pageup", "list", &context)
            .unwrap();
        assert_eq!(
            result.as_ref().map(|a| a.action.as_str()),
            Some("scroll_half_up"),
            "Shift+PageUp should produce scroll_half_up"
        );

        // Shift+PageDown for scroll down
        let result = lua
            .call_handle_key("shift+pagedown", "list", &context)
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

        let result = lua.call_handle_key("ctrl+j", "list", &context).unwrap();
        assert_eq!(
            result.as_ref().map(|a| a.action.as_str()),
            Some("select_next"),
            "Ctrl+J should be select_next"
        );

        let result = lua.call_handle_key("ctrl+k", "list", &context).unwrap();
        assert_eq!(
            result.as_ref().map(|a| a.action.as_str()),
            Some("select_previous"),
            "Ctrl+K should be select_previous"
        );
    }

    /// Verifies Ctrl+] is unbound (no toggle_pty in single-PTY model).
    #[test]
    fn test_e2e_pty_toggle_keybinding() {
        let lua = make_test_layout_with_keybindings();
        let context = KeyContext::default();

        let result = lua.call_handle_key("ctrl+]", "list", &context).unwrap();
        assert_eq!(
            result.as_ref().map(|a| a.action.as_str()),
            None,
            "Ctrl+] should be unbound in single-PTY model"
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

    /// Verifies render in List mode succeeds without connection code.
    ///
    /// # Purpose
    ///
    /// In List mode, no connection code is needed. Render should succeed
    /// regardless of the cached connection code state.
    #[test]
    fn test_list_mode_render_succeeds_without_connection_code_fetch() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Stay in List mode
        assert_eq!(runner.mode, "list");

        // Render in List mode should succeed without any Hub calls
        let result = runner.render(None, None);
        assert!(result.is_ok(), "Render should succeed in List mode");
    }

    // =========================================================================
    // Resize Propagation Tests (TDD)
    // =========================================================================

    /// **BUG FIX TEST**: Verifies resize event updates the parser dimensions.
    ///
    /// # Bug Description (historical)
    ///
    /// When terminal is resized, `handle_resize()` previously only updated
    /// `terminal_dims` and sent the resize via Lua subscription, but never
    /// updated the local parser dimensions. This caused garbled display because
    /// the PTY sent output formatted for new dimensions while the parser
    /// interpreted it with old dimensions.
    ///
    /// # Expected Behavior
    ///
    /// `handle_resize()` invalidates panel dims. Panels are resized by
    /// `sync_widget_dims()` during the next render pass (matching the
    /// actual widget area). Each panel owns its `AlacrittyParser` directly.
    #[test]
    fn test_resize_invalidates_panel_dims() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Setup: create a panel with initial dims
        let mut panel = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        runner.panel_pool.panels.insert("sess-0".to_string(), panel);

        assert_eq!(
            runner.panel_pool.panels.get("sess-0").unwrap().dims(),
            (24, 80)
        );

        // Simulate resize event
        runner.handle_resize(40, 120);

        // Verify: terminal_dims updated, panel dims invalidated (0,0)
        assert_eq!(runner.panel_pool.terminal_dims, (40, 120));
        assert_eq!(
            runner.panel_pool.panels.get("sess-0").unwrap().dims(),
            (0, 0),
            "Panel dims should be invalidated so sync_widget_dims detects change"
        );
    }

    // =========================================================================
    // Agent Navigation & Resize Request Tests
    // =========================================================================
    //
    // These tests verify that TuiRunner sends the correct JSON messages
    // when navigating between agents and handling terminal resize events.
    //
    // Agent navigation now defers terminal subscribe to render-tree sync
    // (for correct widget dimensions), so tests validate deferred behavior.

    /// Verifies `focus_terminal` updates selection state and defers subscribe
    /// until render-tree subscription sync.
    ///
    /// # Scenario
    ///
    /// Given 3 agents, `focus_terminal` with agent-1 should update selection
    /// state immediately and defer subscribe to render sync.
    #[test]
    fn test_focus_terminal_updates_state_and_defers_subscribe() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Action: focus agent-1 (Lua provides session_uuid)
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
            "agent_id": "agent-1",
            "session_uuid": "sess-1",
        }));

        // Verify: no immediate subscribe message from focus op itself.
        assert!(
            request_rx.try_recv().is_err(),
            "focus op should defer subscribe to render subscription sync"
        );

        // Verify local state updated
        assert_eq!(runner.panel_pool.selected_agent.as_deref(), Some("agent-1"));
        assert_eq!(runner.panel_pool.current_session_uuid(), Some("sess-1"));
        assert_eq!(
            runner.panel_pool.current_terminal_sub_id(),
            Some("tui:sess-1")
        );

        // Verify panel created and left idle until render sync subscribes.
        use crate::tui::terminal_panel::PanelState;
        assert_eq!(
            runner.panel_pool.panels.get("sess-1").unwrap().state(),
            PanelState::Idle
        );
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

        // Setup: agent 0 selected with active panel
        runner.panel_pool.selected_agent = Some("agent-0".to_string());
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());
        runner.panel_pool.current_terminal_sub_id = Some("tui:sess-0".to_string());
        {
            let panel = runner
                .panel_pool
                .panels
                .entry("sess-0".to_string())
                .or_insert_with(|| crate::tui::terminal_panel::TerminalPanel::new(24, 80));
            panel.connect("sess-0"); // put in Connecting state
        }

        // Action: focus_terminal with no agent_id (clear selection)
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
        }));

        // Verify: unsubscribe sent
        match request_rx.try_recv() {
            Ok(req) => {
                let msg = unwrap_lua_msg(req);
                assert_eq!(
                    msg.get("type").and_then(|v| v.as_str()),
                    Some("unsubscribe")
                );
            }
            Err(_) => panic!("Expected unsubscribe message to be sent"),
        }

        // Verify selection cleared
        assert_eq!(runner.panel_pool.selected_agent(), None);
        assert_eq!(runner.panel_pool.current_session_uuid(), None);
        assert_eq!(runner.panel_pool.current_terminal_sub_id(), None);
    }

    /// Verifies `focus_terminal` sends unsubscribe for old and defers subscribe for new.
    ///
    /// # Scenario
    ///
    /// When switching from one agent to another, `focus_terminal` immediately
    /// unsubscribes from the old and defers subscribe for the new to render sync.
    #[test]
    fn test_focus_terminal_unsubscribes_old_on_switch() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Setup: agent 0 selected with active panel
        runner.panel_pool.selected_agent = Some("agent-0".to_string());
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());
        runner.panel_pool.current_terminal_sub_id = Some("tui:sess-0".to_string());
        {
            let panel = runner
                .panel_pool
                .panels
                .entry("sess-0".to_string())
                .or_insert_with(|| crate::tui::terminal_panel::TerminalPanel::new(24, 80));
            panel.connect("sess-0");
        }

        // Action: focus agent-1 (Lua provides session_uuid)
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
            "agent_id": "agent-1",
            "session_uuid": "sess-1",
        }));

        // Verify: unsubscribe sent for old terminal
        match request_rx.try_recv() {
            Ok(req) => {
                let msg = unwrap_lua_msg(req);
                assert_eq!(
                    msg.get("type").and_then(|v| v.as_str()),
                    Some("unsubscribe")
                );
                assert_eq!(
                    msg.get("subscriptionId").and_then(|v| v.as_str()),
                    Some("tui:sess-0")
                );
            }
            Err(_) => panic!("Expected unsubscribe message to be sent"),
        }

        // Verify: new subscribe is deferred (no second immediate message).
        assert!(
            request_rx.try_recv().is_err(),
            "subscribe for new target should be deferred to render subscription sync"
        );

        // Verify state
        assert_eq!(runner.panel_pool.selected_agent(), Some("agent-1"));
        assert_eq!(runner.panel_pool.current_session_uuid(), Some("sess-1"));
        assert_eq!(
            runner.panel_pool.current_terminal_sub_id(),
            Some("tui:sess-1")
        );

        use crate::tui::terminal_panel::PanelState;
        assert_eq!(
            runner.panel_pool.panels.get("sess-0").unwrap().state(),
            PanelState::Idle
        );
        assert_eq!(
            runner.panel_pool.panels.get("sess-1").unwrap().state(),
            PanelState::Idle
        );
    }

    /// Verifies `focus_terminal` with unknown agent_id is a no-op.
    ///
    /// # Scenario
    ///
    /// When the agent doesn't exist in the cache, focus_terminal should log
    /// a warning and not change any state.
    #[test]
    fn test_focus_terminal_missing_session_uuid_is_noop() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Action: focus agent without session_uuid (Lua bug or edge case)
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
            "agent_id": "nonexistent",
        }));

        // Verify: no request sent
        assert!(
            request_rx.try_recv().is_err(),
            "No JSON should be sent when session_uuid missing"
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

        // Setup: agent 0 already focused with an active panel subscription.
        runner.panel_pool.selected_agent = Some("agent-0".to_string());
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());
        runner.panel_pool.current_terminal_sub_id = Some("tui:sess-0".to_string());
        let mut panel = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        panel.on_scrollback(b"");
        runner.panel_pool.panels.insert("sess-0".to_string(), panel);

        // Action: focus same agent+session (Lua provides session_uuid)
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
            "agent_id": "agent-0",
            "session_uuid": "sess-0",
        }));

        // Verify: no messages sent (already focused)
        assert!(
            request_rx.try_recv().is_err(),
            "No JSON should be sent when already focused on target"
        );
    }

    /// Verifies `focus_terminal` keeps focused panel idle after transport loss.
    /// Re-subscribe is deferred to render-tree subscription sync.
    #[test]
    fn test_focus_terminal_same_target_keeps_idle_until_render_sync() {
        let (mut runner, mut request_rx) = create_test_runner();

        runner.panel_pool.selected_agent = Some("agent-0".to_string());
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());
        runner.panel_pool.current_terminal_sub_id = Some("tui:sess-0".to_string());

        let mut panel = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        panel.on_scrollback(b"");
        panel.mark_transport_disconnected();
        runner.panel_pool.panels.insert("sess-0".to_string(), panel);

        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
            "agent_id": "agent-0",
            "session_uuid": "sess-0",
        }));

        assert!(
            request_rx.try_recv().is_err(),
            "re-subscribe should be deferred to render subscription sync"
        );
        assert_eq!(
            runner.panel_pool.panels.get("sess-0").unwrap().state(),
            crate::tui::terminal_panel::PanelState::Idle
        );
    }

    /// Verifies `focus_terminal` preserves existing parser content.
    ///
    /// # Scenario
    ///
    /// When switching to an agent that has stale content, the parser is
    /// reused (not blanked). Stale content is visible until the scrollback
    /// snapshot arrives, avoiding a blank frame.
    #[test]
    fn test_focus_terminal_preserves_stale_parser() {
        let (mut runner, _request_rx) = create_test_runner();

        // Setup: pre-populate panel for agent 1.
        // Panel must be Connected, then disconnect so it's Idle when focus_terminal runs.
        let mut panel = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
        panel.connect("sess-1");
        panel.on_scrollback(b"");
        // Feed VT output so the parser has content
        panel.on_output(b"stale content");
        panel.disconnect("sess-1");
        runner.panel_pool.panels.insert("sess-1".to_string(), panel);
        // Simulate already viewing this agent (prevents stale-panel eviction)
        runner.panel_pool.selected_agent = Some("agent-1".to_string());

        // Action: focus agent-1
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
            "agent_id": "agent-1",
            "session_uuid": "sess-1",
        }));

        // Verify: panel is preserved (not blanked) — it retains its parser state
        let panel = runner.panel_pool.panels.get("sess-1").unwrap();
        let contents = panel.contents();
        assert!(
            contents.contains("stale"),
            "Panel should preserve stale content, got: {contents:?}"
        );
    }

    /// Verifies focused terminal subscribes with rendered-area dimensions.
    ///
    /// # Scenario
    ///
    /// Focus now defers subscribe to render-tree sync. The eventual subscribe
    /// should use the current rendered area dimensions.
    #[test]
    fn test_focus_terminal_subscribe_uses_rendered_area_dims() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Action: focus agent-1 (subscribe deferred until sync_subscriptions)
        runner.execute_focus_terminal(&serde_json::json!({
            "op": "focus_terminal",
            "agent_id": "agent-1",
            "session_uuid": "sess-1",
        }));

        let tree = crate::tui::render_tree::RenderNode::Widget {
            widget_type: crate::tui::render_tree::WidgetType::Terminal,
            id: None,
            block: None,
            custom_lines: None,
            props: Some(crate::tui::render_tree::WidgetProps::Terminal(
                crate::tui::render_tree::TerminalBinding {
                    session_uuid: Some("sess-1".to_string()),
                },
            )),
        };
        let mut areas = std::collections::HashMap::new();
        areas.insert(
            "sess-1".to_string(),
            crate::tui::render::WidgetArea {
                rect: ratatui::layout::Rect::new(0, 0, 60, 20),
                widget_type: "terminal".to_string(),
            },
        );
        let msgs = runner.panel_pool.sync_subscriptions(&tree, &areas);
        for msg in msgs {
            runner.send_msg(msg);
        }

        // Verify: subscribe uses rendered area dims (20x60)
        match request_rx.try_recv() {
            Ok(req) => {
                let msg = unwrap_lua_msg(req);
                let params = msg.get("params").expect("should have params");
                assert_eq!(params.get("rows").and_then(|v| v.as_u64()), Some(20));
                assert_eq!(params.get("cols").and_then(|v| v.as_u64()), Some(60));
            }
            Err(_) => panic!("Expected subscribe message"),
        }
    }

    #[test]
    fn test_bridge_reconnected_resets_to_attach_state() {
        use crate::tui::terminal_panel::PanelState;

        let (mut runner, _request_rx) = create_test_runner();
        runner.mode = "restarting".to_string();
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());
        runner.panel_pool.selected_agent = Some("agent-0".to_string());
        runner.overlay_list_actions = vec!["restart_hub".to_string()];
        runner.has_overlay = true;
        runner.focused_list_id = Some("menu".to_string());
        runner.focused_input_id = Some("prompt".to_string());
        runner.terminal_modes.on_kitty_changed(true);

        {
            let panel = runner.panel_pool.resolve_panel("sess-0");
            let _ = panel.connect("sess-0");
            panel.on_scrollback(b"");
            assert_eq!(panel.state(), PanelState::Connected);
        }

        runner.dispatch_hub_event(serde_json::json!({"type":"bridge_reconnected"}), None);

        assert_eq!(runner.mode, "restarting");
        assert!(!runner.panel_pool.panels().contains_key("sess-0"));
        assert_eq!(runner.panel_pool.current_session_uuid(), None);
        assert_eq!(runner.panel_pool.selected_agent(), None);
        assert_eq!(runner.panel_pool.current_terminal_sub_id(), None);
        assert!(!runner.has_overlay);
        assert!(runner.overlay_list_actions.is_empty());
        assert_eq!(runner.focused_list_id, None);
        assert_eq!(runner.focused_input_id, None);
        assert!(!runner.terminal_modes.inner_kitty_enabled());
        assert_eq!(
            runner.panel_pool.panels().len(),
            0,
            "bridge reconnect should hard-reset panel state"
        );
    }

    #[test]
    fn entity_frames_update_store_without_mutating_lua_state() {
        let (mut runner, _request_rx) = create_test_runner();
        let lua = make_test_layout_with_keybindings();

        runner.dispatch_hub_event(
            serde_json::json!({
                "type": "entity_snapshot",
                "entity_type": "session",
                "items": [{
                    "session_uuid": "sess-modern",
                    "display_name": "agent one",
                    "branch_name": "main",
                    "session_type": "agent"
                }],
                "snapshot_seq": 1
            }),
            Some(&lua),
        );

        let store = runner
            .entity_stores
            .store("session")
            .expect("session store should be created");
        assert_eq!(store.order, vec!["sess-modern".to_string()]);
        assert_eq!(
            store
                .by_id
                .get("sess-modern")
                .and_then(|session| session.get("display_name"))
                .and_then(serde_json::Value::as_str),
            Some("agent one")
        );
        assert_eq!(
            lua.eval_usize("return #_tui_state.agents").unwrap(),
            0,
            "entity envelopes should not maintain a second Lua entity cache"
        );
    }

    /// Verifies scrollback event clears parser before processing snapshot.
    ///
    /// # Scenario
    ///
    /// A parser pool entry has existing content and a non-zero scroll offset.
    /// When a Scrollback event arrives, the parser should be cleared and
    /// scroll reset to 0 before the snapshot data is processed.
    #[test]
    fn test_scrollback_clears_parser_before_processing() {
        let (mut runner, _request_rx) = create_test_runner();

        // Setup: panel in Connecting state with existing content and scroll offset
        let mut panel = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
        panel.connect("sess-0"); // Idle → Connecting (subscribe sent)
        for i in 0..30 {
            panel.on_output(format!("old line {i}\r\n").as_bytes());
        }
        panel.scroll_up(5);
        assert!(panel.is_scrolled(), "precondition: scrolled");
        runner.panel_pool.panels.insert("sess-0".to_string(), panel);
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());

        // Deliver empty scrollback (binary snapshot with no data replaces parser)
        runner.output_rx.close();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        runner.output_rx = rx;
        tx.send(TuiOutput::Scrollback {
            session_uuid: "sess-0".into(),
            rows: 24,
            cols: 80,
            data: vec![],
            kitty_enabled: false,
        })
        .unwrap();

        // Process the event
        runner.poll_pty_events(None);

        // Verify: old content gone, scroll reset, state transitions to Connected
        let panel = runner.panel_pool.panels.get("sess-0").unwrap();
        let contents = panel.contents();
        assert!(
            !contents.contains("old line"),
            "Old content should be cleared, got: {contents:?}"
        );
        assert!(!panel.is_scrolled(), "Scroll should be reset to bottom");
        assert_eq!(
            panel.state(),
            crate::tui::terminal_panel::PanelState::Connected
        );
    }

    #[test]
    fn test_scrollback_legacy_zero_dims_uses_panel_dims() {
        let (mut runner, _request_rx) = create_test_runner();

        let mut panel = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        runner.panel_pool.panels.insert("sess-0".to_string(), panel);
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());

        runner.output_rx.close();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        runner.output_rx = rx;
        tx.send(TuiOutput::Scrollback {
            session_uuid: "sess-0".into(),
            rows: 0,
            cols: 0,
            data: vec![],
            kitty_enabled: false,
        })
        .unwrap();

        runner.poll_pty_events(None);

        // When rows/cols are 0, panel falls back to its own cached dims
        let panel = runner.panel_pool.panels.get("sess-0").unwrap();
        assert_eq!(panel.dims(), (24, 80));
        assert_eq!(
            panel.state(),
            crate::tui::terminal_panel::PanelState::Connected
        );
    }

    /// Verifies `handle_resize()` invalidates panel dims for next render.
    ///
    /// # Scenario
    ///
    /// When terminal is resized, panel dims should be invalidated so
    /// `sync_widget_dims` detects changes on the next render pass.
    #[test]
    fn test_handle_resize_invalidates_panel_dims() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Set up connected state with a panel
        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());
        runner.panel_pool.current_terminal_sub_id = Some("tui:sess-0".to_string());
        let mut panel = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        runner.panel_pool.panels.insert("sess-0".to_string(), panel);

        // Drain the subscribe message
        let _ = request_rx.try_recv();

        // Action: resize to 40 rows x 120 cols
        runner.handle_resize(40, 120);

        // Verify: no messages sent (resize flows through sync_widget_dims on next render)
        assert!(
            request_rx.try_recv().is_err(),
            "handle_resize should not send any messages"
        );

        // Verify: local state updated
        assert_eq!(runner.panel_pool.terminal_dims, (40, 120));

        // Verify: panel dims invalidated (will trigger resize on next render)
        assert_eq!(
            runner.panel_pool.panels.get("sess-0").unwrap().dims(),
            (0, 0)
        );
    }

    /// Verifies `handle_resize()` updates state without sending messages
    /// when no terminal subscription is active.
    #[test]
    fn test_handle_resize_without_terminal_sub_updates_state_only() {
        let (mut runner, mut request_rx) = create_test_runner();

        // No terminal subscription (not connected to a PTY)
        runner.handle_resize(40, 120);

        // Verify: no messages sent (PTY resize handled by pty_clients via terminal channel)
        assert!(
            request_rx.try_recv().is_err(),
            "handle_resize should not send any messages"
        );

        // Verify: local state still updated
        assert_eq!(runner.panel_pool.terminal_dims, (40, 120));
    }

    // === Hot-Reload & Lua Reload ===

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
        assert!(
            result.is_ok(),
            "Old render function should still work after failed reload"
        );
    }

    fn make_test_render_context() -> super::super::render::RenderContext<'static> {
        // 'static requires leaked reference for the empty panels map
        let panels: &'static std::collections::HashMap<
            String,
            crate::tui::terminal_panel::TerminalPanel,
        > = Box::leak(Box::new(std::collections::HashMap::new()));
        super::super::render::RenderContext {
            error_message: None,
            connection_code: None,
            bundle_used: false,
            panels,
            scroll_offset: 0,
            is_scrolled: false,
            focused_session_uuid: None,
            is_terminal_mode: false,
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
            terminal_cols: 80,
            terminal_rows: 24,
            widget_areas: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }

    // === Subscription Sync Tests ===

    /// Verifies `sync_subscriptions` subscribes to a new terminal binding.
    #[test]
    fn test_sync_subscriptions_subscribes_new() {
        let (mut runner, mut request_rx) = create_test_runner();

        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());

        // Tree with a single terminal widget with explicit binding
        let tree = crate::tui::render_tree::RenderNode::Widget {
            widget_type: crate::tui::render_tree::WidgetType::Terminal,
            id: None,
            block: None,
            custom_lines: None,
            props: Some(crate::tui::render_tree::WidgetProps::Terminal(
                crate::tui::render_tree::TerminalBinding {
                    session_uuid: Some("sess-0".to_string()),
                },
            )),
        };

        let mut areas = std::collections::HashMap::new();
        areas.insert(
            "sess-0".to_string(),
            crate::tui::render::WidgetArea {
                rect: ratatui::layout::Rect::new(0, 0, 80, 24),
                widget_type: "terminal".to_string(),
            },
        );
        let msgs = runner.panel_pool.sync_subscriptions(&tree, &areas);
        for msg in msgs {
            runner.send_msg(msg);
        }

        // Should have sent a subscribe message for sess-0
        match request_rx.try_recv() {
            Ok(req) => {
                let msg = unwrap_lua_msg(req);
                assert_eq!(msg.get("type").and_then(|v| v.as_str()), Some("subscribe"));
                assert_eq!(
                    msg.get("subscriptionId").and_then(|v| v.as_str()),
                    Some("tui:sess-0")
                );
                let params = msg.get("params").expect("should have params");
                assert_eq!(
                    params.get("session_uuid").and_then(|v| v.as_str()),
                    Some("sess-0")
                );
            }
            Err(_) => panic!("Expected subscribe message"),
        }

        use crate::tui::terminal_panel::PanelState;
        assert_eq!(
            runner.panel_pool.panels.get("sess-0").unwrap().state(),
            PanelState::Connecting
        );
    }

    /// Verifies reconnect resubscribe uses current render-area dimensions.
    ///
    /// Regression: an idle panel can survive socket reconnect with stale cached
    /// dims from before restart. `sync_subscriptions` must refresh dims from
    /// `areas` before calling connect, or subscribe sends stale cols/rows and
    /// scrollback renders incorrectly until a full reattach.
    #[test]
    fn test_sync_subscriptions_refreshes_idle_panel_dims_before_subscribe() {
        let (mut runner, mut request_rx) = create_test_runner();

        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());
        runner.panel_pool.selected_agent = Some("agent-0".to_string());
        runner.panel_pool.panels.insert(
            "sess-0".to_string(),
            crate::tui::terminal_panel::TerminalPanel::new(57, 181),
        );

        let tree = crate::tui::render_tree::RenderNode::Widget {
            widget_type: crate::tui::render_tree::WidgetType::Terminal,
            id: None,
            block: None,
            custom_lines: None,
            props: Some(crate::tui::render_tree::WidgetProps::Terminal(
                crate::tui::render_tree::TerminalBinding {
                    session_uuid: Some("sess-0".to_string()),
                },
            )),
        };

        let mut areas = std::collections::HashMap::new();
        areas.insert(
            "sess-0".to_string(),
            crate::tui::render::WidgetArea {
                rect: ratatui::layout::Rect::new(0, 0, 148, 57),
                widget_type: "terminal".to_string(),
            },
        );

        let msgs = runner.panel_pool.sync_subscriptions(&tree, &areas);
        for msg in msgs {
            runner.send_msg(msg);
        }

        let msg = unwrap_lua_msg(request_rx.try_recv().expect("Expected subscribe message"));
        assert_eq!(msg.get("type").and_then(|v| v.as_str()), Some("subscribe"));
        let params = msg.get("params").expect("should have params");
        assert_eq!(params.get("rows").and_then(|v| v.as_u64()), Some(57));
        assert_eq!(params.get("cols").and_then(|v| v.as_u64()), Some(148));

        let panel = runner
            .panel_pool
            .panels
            .get("sess-0")
            .expect("panel exists");
        assert_eq!(panel.dims(), (57, 148));
        assert_eq!(
            panel.state(),
            crate::tui::terminal_panel::PanelState::Connecting
        );
    }

    /// Verifies `sync_subscriptions` unsubscribes when a binding is removed.
    #[test]
    fn test_sync_subscriptions_unsubscribes_removed() {
        let (mut runner, mut request_rx) = create_test_runner();

        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());

        // Pre-populate panels for two sessions (both connected)
        {
            let mut p0 = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
            p0.connect("sess-0");
            runner.panel_pool.panels.insert("sess-0".to_string(), p0);
            let mut p1 = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
            p1.connect("sess-1");
            runner.panel_pool.panels.insert("sess-1".to_string(), p1);
        }
        // Drain subscribe messages
        while request_rx.try_recv().is_ok() {}

        // Tree only has terminal for sess-0 — sess-1 should be unsubscribed
        let tree = crate::tui::render_tree::RenderNode::Widget {
            widget_type: crate::tui::render_tree::WidgetType::Terminal,
            id: None,
            block: None,
            custom_lines: None,
            props: Some(crate::tui::render_tree::WidgetProps::Terminal(
                crate::tui::render_tree::TerminalBinding {
                    session_uuid: Some("sess-0".to_string()),
                },
            )),
        };

        let msgs = runner
            .panel_pool
            .sync_subscriptions(&tree, &std::collections::HashMap::new());
        for msg in msgs {
            runner.send_msg(msg);
        }

        // Should have sent unsubscribe for sess-1
        let mut found_unsubscribe = false;
        while let Ok(req) = request_rx.try_recv() {
            let msg = unwrap_lua_msg(req);
            if msg.get("type").and_then(|v| v.as_str()) == Some("unsubscribe") {
                assert_eq!(
                    msg.get("subscriptionId").and_then(|v| v.as_str()),
                    Some("tui:sess-1")
                );
                found_unsubscribe = true;
            }
        }
        assert!(
            found_unsubscribe,
            "Should send unsubscribe for removed binding"
        );

        // Panel sess-0 still exists, sess-1 removed
        assert!(runner.panel_pool.panels.contains_key("sess-0"));
        assert!(!runner.panel_pool.panels.contains_key("sess-1"));
    }

    /// Verifies `sync_subscriptions` is idempotent for already-connected panels.
    #[test]
    fn test_sync_subscriptions_no_change_idempotent() {
        let (mut runner, mut request_rx) = create_test_runner();

        runner.panel_pool.current_session_uuid = Some("sess-0".to_string());

        // Pre-populate with a connected panel
        {
            let mut panel = crate::tui::terminal_panel::TerminalPanel::new(24, 80);
            panel.connect("sess-0");
            runner.panel_pool.panels.insert("sess-0".to_string(), panel);
        }
        // Drain subscribe message
        while request_rx.try_recv().is_ok() {}

        let tree = crate::tui::render_tree::RenderNode::Widget {
            widget_type: crate::tui::render_tree::WidgetType::Terminal,
            id: None,
            block: None,
            custom_lines: None,
            props: Some(crate::tui::render_tree::WidgetProps::Terminal(
                crate::tui::render_tree::TerminalBinding {
                    session_uuid: Some("sess-0".to_string()),
                },
            )),
        };

        let msgs = runner
            .panel_pool
            .sync_subscriptions(&tree, &std::collections::HashMap::new());
        for msg in msgs {
            runner.send_msg(msg);
        }

        // No messages should be sent (already connected)
        assert!(
            request_rx.try_recv().is_err(),
            "No messages should be sent when subscriptions unchanged"
        );
        assert_eq!(runner.panel_pool.panels.len(), 1);
    }

    // =========================================================================
    // Full Render Pipeline E2E Tests (no stubs)
    // =========================================================================

    /// Create a LayoutLua with ALL real Lua sources — no stubs.
    ///
    /// Uses the actual layout.lua, keybindings.lua, actions.lua, events.lua.
    /// This means render_overlay() returns real overlays based on _tui_state.mode.
    fn make_real_layout_lua() -> LayoutLua {
        let layout_source = include_str!("../../lua/ui/layout.lua");
        let kb_source = include_str!("../../lua/ui/keybindings.lua");
        let actions_source = include_str!("../../lua/ui/actions.lua");
        let events_source = include_str!("../../lua/ui/events.lua");
        let botster_source = include_str!("../../lua/ui/botster.lua");

        let mut lua = LayoutLua::new(layout_source).expect("layout.lua should load");
        lua.load_extension(
            "_tui_state = _tui_state or { agents = {}, pending_fields = {}, available_worktrees = {}, available_agents = {}, mode = 'list', input_buffer = '', list_selected = 0, selected_agent_index = nil }",
            "_tui_state_init",
        ).expect("_tui_state bootstrap should succeed");
        lua.preload_module(
            "ui.workspace_helpers",
            include_str!("../../lua/ui/workspace_helpers.lua"),
        )
        .expect("workspace_helpers.lua should preload");
        lua.load_keybindings(kb_source)
            .expect("keybindings.lua should load");
        lua.load_actions(actions_source)
            .expect("actions.lua should load");
        lua.load_events(events_source)
            .expect("events.lua should load");
        lua.load_extension(botster_source, "botster")
            .expect("botster.lua should load");
        lua
    }

    /// Helper: process a key AND run the render pass, just like production.
    ///
    /// In production, the loop is: poll_input → render → poll_wait.
    /// The render pass populates overlay_list_actions, focused_list_id,
    /// and focused_input_id from the actual Lua render tree.
    /// Tests that skip render() are testing with stale/missing widget state.
    fn press_key_and_render(
        runner: &mut TuiRunner<TestBackend>,
        event: InputEvent,
        lua: &LayoutLua,
    ) {
        runner.handle_raw_input_event(event, Some(lua));
        runner
            .render(Some(lua), None)
            .expect("render should succeed");
    }

    /// Full new-agent flow using real render pipeline — no stubs.
    ///
    /// Exercises the EXACT production code path:
    /// 1. Ctrl+P → menu overlay rendered → list actions + focused list extracted
    /// 2. Enter → list_select dispatched with real overlay_actions
    /// 3. Enter on worktree list → list_select with real worktree items
    /// 4. Type prompt → input_char handled via real focused_input_id
    /// 5. Enter → input_submit sends create_agent message
    ///
    /// This test catches bugs that stubbed tests miss, such as:
    /// - Render tree not producing the expected widget structure
    /// - extract_list_actions or extract_focused_widgets failing
    /// - Widget state not syncing between render and input handling
    #[test]
    fn test_e2e_full_render_new_agent_main_branch() {
        let (mut runner, _output_tx, mut request_rx, shutdown) =
            create_test_runner_with_mock_client();
        let lua = make_real_layout_lua();

        // Initial render to establish baseline state
        runner
            .render(Some(&lua), None)
            .expect("initial render should succeed");

        assert_eq!(runner.mode(), "list");
        assert!(!runner.has_overlay, "no overlay in list mode");

        // === Step 1: Ctrl+P opens menu ===
        press_key_and_render(&mut runner, make_key_ctrl('p'), &lua);

        assert_eq!(runner.mode(), "menu", "Ctrl+P should open menu");
        assert!(runner.has_overlay, "menu should produce overlay");
        assert!(
            !runner.overlay_list_actions.is_empty(),
            "menu overlay should have list actions, got: {:?}",
            runner.overlay_list_actions
        );
        assert!(
            runner.focused_list_id.is_some(),
            "menu overlay should have focused list widget"
        );
        // Find "new_agent" in the real overlay actions
        let new_agent_idx = runner
            .overlay_list_actions
            .iter()
            .position(|a| a == "new_agent")
            .expect("new_agent should be in overlay_list_actions");

        // === Step 2: Navigate to New Agent and select ===
        for _ in 0..new_agent_idx {
            press_key_and_render(&mut runner, make_key_down(), &lua);
        }
        press_key_and_render(&mut runner, make_key_enter(), &lua);

        // Small delay to let responder process messages
        thread::sleep(Duration::from_millis(10));

        assert_eq!(
            runner.mode(),
            "new_agent_select_target",
            "Selecting New Agent should enter agent config selection"
        );

        // Simulate single agent config response (auto-skips to workspace selection)
        {
            let agent_config_event = serde_json::json!({ "agents": ["claude"] });
            let ctx = crate::tui::layout_lua::ActionContext::default();
            let ops = lua
                .call_on_hub_event("agent_config", &agent_config_event, &ctx)
                .unwrap()
                .unwrap();
            runner.execute_lua_ops(ops);
        }
        runner
            .render(Some(&lua), None)
            .expect("render after agent config auto-select");

        assert_eq!(
            runner.mode(),
            "new_agent_select_workspace",
            "Should auto-advance to workspace selection"
        );
        assert!(
            runner.focused_list_id.is_some(),
            "workspace list should be focused after render, got focused_list_id={:?}",
            runner.focused_list_id
        );

        // === Step 3a: Select "Create New Workspace" (index 0) → name input ===
        press_key_and_render(&mut runner, make_key_enter(), &lua);

        assert_eq!(
            runner.mode(),
            "new_workspace_name_input",
            "Should enter workspace name input"
        );
        assert!(
            runner.focused_input_id.is_some(),
            "workspace name input should be focused after render, got focused_input_id={:?}",
            runner.focused_input_id
        );

        // === Step 3a-ii: Submit empty name → worktree selection ===
        press_key_and_render(&mut runner, make_key_enter(), &lua);

        assert_eq!(
            runner.mode(),
            "new_agent_select_worktree",
            "Should advance to worktree selection after workspace name"
        );
        assert!(
            runner.focused_list_id.is_some(),
            "worktree list should be focused after render, got focused_list_id={:?}",
            runner.focused_list_id
        );

        // === Step 3b: Select "Use Main Branch" (index 0 — first item) ===
        press_key_and_render(&mut runner, make_key_enter(), &lua);

        assert_eq!(
            runner.mode(),
            "new_agent_prompt",
            "Selecting Use Main Branch should enter prompt mode"
        );
        assert!(
            runner.focused_input_id.is_some(),
            "prompt input should be focused after render, got focused_input_id={:?}",
            runner.focused_input_id
        );

        // === Step 4: Type prompt ===
        for c in "test prompt".chars() {
            press_key_and_render(&mut runner, make_key_char(c), &lua);
        }

        // Verify input was captured (check Lua state directly)
        let buffer = lua
            .eval_string("return _tui_state.input_buffer")
            .expect("should read input_buffer");
        assert_eq!(
            buffer, "test prompt",
            "typed text should be in input_buffer"
        );

        // === Step 5: Submit prompt ===
        press_key_and_render(&mut runner, make_key_enter(), &lua);

        // Wait for responder
        thread::sleep(Duration::from_millis(10));

        // Verify create_agent message was sent
        let mut found_create = false;
        while let Ok(req) = request_rx.try_recv() {
            let msg = unwrap_lua_msg(req);
            if let Some(data) = msg.get("data") {
                if data.get("type").and_then(|t| t.as_str()) == Some("create_agent") {
                    assert_eq!(
                        data.get("prompt").and_then(|v| v.as_str()),
                        Some("test prompt"),
                        "prompt should match typed text"
                    );
                    // Main branch mode: issue_or_branch should be absent or null
                    assert!(
                        data.get("issue_or_branch").is_none()
                            || data.get("issue_or_branch").unwrap().is_null(),
                        "main branch mode should have nil issue_or_branch"
                    );
                    found_create = true;
                    break;
                }
            }
        }
        assert!(
            found_create,
            "create_agent message should be sent through real render pipeline"
        );

        // Mode should return to list after submit
        assert_eq!(
            runner.mode(),
            "list",
            "Should return to list mode after agent creation"
        );

        shutdown.store(true, Ordering::Relaxed);
    }

    /// Full new-agent flow with new worktree (branch name) — no stubs.
    ///
    /// Tests the path: menu → new agent → create new worktree → type branch → prompt → submit.
    #[test]
    fn test_e2e_full_render_new_agent_new_worktree() {
        let (mut runner, _output_tx, mut request_rx, shutdown) =
            create_test_runner_with_mock_client();
        let lua = make_real_layout_lua();

        // Initial render
        runner
            .render(Some(&lua), None)
            .expect("initial render should succeed");

        // Step 1: Ctrl+P → menu
        press_key_and_render(&mut runner, make_key_ctrl('p'), &lua);
        assert_eq!(runner.mode(), "menu");

        // Step 2: Navigate to New Agent and select
        let new_agent_idx = runner
            .overlay_list_actions
            .iter()
            .position(|a| a == "new_agent")
            .expect("new_agent in actions");
        for _ in 0..new_agent_idx {
            press_key_and_render(&mut runner, make_key_down(), &lua);
        }
        press_key_and_render(&mut runner, make_key_enter(), &lua);
        thread::sleep(Duration::from_millis(10));
        assert_eq!(runner.mode(), "new_agent_select_target");

        {
            let target_event = serde_json::json!({
                "targets": [
                    { "id": "tgt_trybotster", "name": "trybotster", "path": "/tmp/trybotster", "current_branch": "main" }
                ]
            });
            let ctx = crate::tui::layout_lua::ActionContext::default();
            let ops = lua
                .call_on_hub_event("spawn_target_list", &target_event, &ctx)
                .unwrap()
                .unwrap();
            runner.execute_lua_ops(ops);
        }
        runner
            .render(Some(&lua), None)
            .expect("render after spawn target list");
        runner.focused_list_id = Some("spawn_target_list".to_string());
        runner
            .widget_states
            .list_state("spawn_target_list")
            .set_selectable_count(1);
        press_key_and_render(&mut runner, make_key_enter(), &lua);
        assert_eq!(runner.mode(), "new_agent_select_agent");

        // Simulate single agent config response (auto-skips to workspace selection)
        {
            let agent_config_event = serde_json::json!({ "agents": ["claude"] });
            let ctx = crate::tui::layout_lua::ActionContext::default();
            let ops = lua
                .call_on_hub_event("agent_config", &agent_config_event, &ctx)
                .unwrap()
                .unwrap();
            runner.execute_lua_ops(ops);
        }
        runner
            .render(Some(&lua), None)
            .expect("render after agent config auto-select");
        assert_eq!(runner.mode(), "new_agent_select_workspace");

        // Step 3a: Select "Create New Workspace" (index 0) → name input
        press_key_and_render(&mut runner, make_key_enter(), &lua);
        assert_eq!(runner.mode(), "new_workspace_name_input");

        // Step 3a-ii: Submit empty name → worktree selection
        press_key_and_render(&mut runner, make_key_enter(), &lua);
        assert_eq!(runner.mode(), "new_agent_select_worktree");

        // Step 3b: Navigate to "Create New Worktree" (index 1) and select
        press_key_and_render(&mut runner, make_key_down(), &lua);
        press_key_and_render(&mut runner, make_key_enter(), &lua);

        assert_eq!(
            runner.mode(),
            "new_agent_create_worktree",
            "Should enter create worktree mode"
        );
        assert!(
            runner.focused_input_id.is_some(),
            "worktree input should be focused"
        );

        // Step 4: Type branch name
        for c in "fix-123".chars() {
            press_key_and_render(&mut runner, make_key_char(c), &lua);
        }
        let buffer = lua
            .eval_string("return _tui_state.input_buffer")
            .expect("should read input_buffer");
        assert_eq!(buffer, "fix-123");

        // Step 5: Submit branch name → should go to prompt
        press_key_and_render(&mut runner, make_key_enter(), &lua);
        assert_eq!(
            runner.mode(),
            "new_agent_prompt",
            "Should enter prompt mode after branch name"
        );
        assert!(
            runner.focused_input_id.is_some(),
            "prompt input should be focused"
        );

        // Step 6: Type prompt and submit
        for c in "fix the bug".chars() {
            press_key_and_render(&mut runner, make_key_char(c), &lua);
        }
        press_key_and_render(&mut runner, make_key_enter(), &lua);
        thread::sleep(Duration::from_millis(10));

        // Verify create_agent message
        let mut found_create = false;
        while let Ok(req) = request_rx.try_recv() {
            let msg = unwrap_lua_msg(req);
            if let Some(data) = msg.get("data") {
                if data.get("type").and_then(|t| t.as_str()) == Some("create_agent") {
                    assert_eq!(
                        data.get("issue_or_branch").and_then(|v| v.as_str()),
                        Some("fix-123"),
                        "branch name should match"
                    );
                    assert_eq!(
                        data.get("prompt").and_then(|v| v.as_str()),
                        Some("fix the bug"),
                        "prompt should match"
                    );
                    found_create = true;
                    break;
                }
            }
        }
        assert!(found_create, "create_agent with worktree should be sent");
        assert_eq!(runner.mode(), "list");

        shutdown.store(true, Ordering::Relaxed);
    }

    /// Escape cancels at every stage — real render pipeline.
    #[test]
    fn test_e2e_full_render_escape_cancels() {
        let (mut runner, _output_tx, _request_rx, shutdown) = create_test_runner_with_mock_client();
        let lua = make_real_layout_lua();

        runner.render(Some(&lua), None).expect("initial render");

        // Open menu, then escape
        press_key_and_render(&mut runner, make_key_ctrl('p'), &lua);
        assert_eq!(runner.mode(), "menu");
        press_key_and_render(&mut runner, make_key_escape(), &lua);
        assert_eq!(
            runner.mode(),
            "list",
            "Escape from menu should return to list"
        );

        // Open menu → New Agent → escape from agent config selection
        press_key_and_render(&mut runner, make_key_ctrl('p'), &lua);
        let new_agent_idx = runner
            .overlay_list_actions
            .iter()
            .position(|a| a == "new_agent")
            .expect("new_agent in actions");
        for _ in 0..new_agent_idx {
            press_key_and_render(&mut runner, make_key_down(), &lua);
        }
        press_key_and_render(&mut runner, make_key_enter(), &lua);
        thread::sleep(Duration::from_millis(10));
        assert_eq!(runner.mode(), "new_agent_select_target");
        press_key_and_render(&mut runner, make_key_escape(), &lua);
        assert_eq!(
            runner.mode(),
            "list",
            "Escape from agent config selection should return to list"
        );

        shutdown.store(true, Ordering::Relaxed);
    }

    // Rust guideline compliant 2026-02
}
