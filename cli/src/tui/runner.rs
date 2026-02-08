//! TUI Runner - independent TUI thread with its own event loop.
//!
//! The TuiRunner owns all TUI state and runs in its own thread, communicating
//! with the Hub via channels. This isolates terminal handling from hub logic.
//!
//! # Architecture
//!
//! ```text
//! TuiRunner (TUI thread)
//! ├── vt100_parser: Arc<Mutex<Parser>>  - terminal emulation
//! ├── terminal: Terminal<CrosstermBackend>  - ratatui terminal
//! ├── mode, menu_selected, input_buffer  - UI state
//! ├── agents, selected_agent  - agent state cache
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
//! Handler methods are split across several modules for maintainability:
//! - [`super::runner_handlers`] - `handle_tui_action()`, `handle_lua_message()`
//! - [`super::runner_agent`] - Agent navigation (`request_select_next()`, etc.)
//! - [`super::runner_input`] - Input handlers (`handle_menu_select()`, etc.)
//!
//! # Event Flow
//!
//! Agent lifecycle events flow through Lua (`broadcast_hub_event()` in
//! `connections.lua`) and arrive as `TuiOutput::Message` JSON. TuiRunner
//! processes these in `handle_lua_message()` to update its cached state.

// Rust guideline compliant 2026-02

use std::io::Stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event};
use ratatui::backend::Backend;
use ratatui::Terminal;
use vt100::Parser;

use ratatui::backend::CrosstermBackend;

use crate::agent::PtyView;
use crate::app::AppMode;
use crate::client::TuiOutput;
use crate::hub::Hub;
use crate::relay::AgentInfo;
use crate::tui::layout::terminal_widget_inner_area;

use super::actions::InputResult;
use super::events::CreationStage;
use super::input::{process_event, InputContext};
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
/// TuiRunner sends JSON messages through `request_tx`. Hub routes all messages
/// through Lua `client.lua` — the same protocol as browser clients.
pub struct TuiRunner<B: Backend> {
    // === Terminal ===
    /// VT100 parser for terminal emulation.
    ///
    /// Receives PTY output via output_rx channel and maintains screen state.
    pub(super) vt100_parser: Arc<Mutex<Parser>>,

    /// Ratatui terminal for rendering.
    terminal: Terminal<B>,

    // === UI State (TuiRunner-specific) ===
    /// Current application mode (Normal, Menu, etc.).
    pub(super) mode: AppMode,

    /// Currently selected menu item index.
    pub(super) menu_selected: usize,

    /// Text input buffer for text entry modes.
    pub(super) input_buffer: String,

    /// Currently selected worktree index in selection modal.
    pub(super) worktree_selected: usize,

    /// Available worktrees for agent creation.
    pub(super) available_worktrees: Vec<(String, String)>,

    /// Current connection code data (URL + QR ASCII) for display.
    pub(super) connection_code: Option<ConnectionCodeData>,

    /// Error message to display in Error mode.
    pub(super) error_message: Option<String>,

    /// Agent creation progress (identifier, stage).
    pub(super) creating_agent: Option<(String, CreationStage)>,

    /// Issue or branch name for new agent creation (stored between modes).
    pub(super) pending_issue_or_branch: Option<String>,

    // === Agent State ===
    /// Cached agent list (updated via Lua event callbacks).
    pub(super) agents: Vec<AgentInfo>,

    // === Channels ===
    /// Request sender to Hub.
    ///
    /// All TUI operations flow through this channel as JSON messages,
    /// routed through Lua `client.lua` — the same path as browser clients.
    pub(super) request_tx: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,

    // === Selection State ===
    /// Currently selected agent ID.
    ///
    /// The agent ID (session key) of the currently selected agent.
    pub(super) selected_agent: Option<String>,

    /// Active PTY view (CLI or Server).
    ///
    /// Tracks which PTY view is displayed. TuiRunner owns this state.
    pub(super) active_pty_view: PtyView,

    /// Index of the agent currently being viewed/interacted with.
    ///
    /// Used for Lua subscribe/unsubscribe operations.
    pub(super) current_agent_index: Option<usize>,

    /// Index of the PTY currently being viewed/interacted with.
    ///
    /// 0 = CLI PTY, 1 = Server PTY. This tracks which PTY receives keyboard
    /// input and is displayed in the terminal widget.
    pub(super) current_pty_index: Option<usize>,

    /// Active terminal subscription ID for the Lua subscribe/unsubscribe protocol.
    ///
    /// Tracks the current terminal subscription so we can unsubscribe before
    /// switching agents or toggling PTY views. Uses the same subscription
    /// protocol as browser clients.
    pub(super) current_terminal_sub_id: Option<String>,

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
}

impl<B: Backend> std::fmt::Debug for TuiRunner<B>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiRunner")
            .field("mode", &self.mode)
            .field("selected_agent", &self.selected_agent)
            .field("agents_count", &self.agents.len())
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
        request_tx: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
        output_rx: tokio::sync::mpsc::UnboundedReceiver<TuiOutput>,
        shutdown: Arc<AtomicBool>,
        terminal_dims: (u16, u16),
    ) -> Self {
        let (rows, cols) = terminal_dims;
        let parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
        let vt100_parser = Arc::new(Mutex::new(parser));

        Self {
            vt100_parser,
            terminal,
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: String::new(),
            worktree_selected: 0,
            available_worktrees: Vec::new(),
            connection_code: None,
            error_message: None,
            creating_agent: None,
            pending_issue_or_branch: None,
            agents: Vec::new(),
            request_tx,
            selected_agent: None,
            active_pty_view: PtyView::default(),
            current_agent_index: None,
            current_pty_index: None,
            current_terminal_sub_id: None,
            output_rx,
            shutdown,
            quit: false,
            terminal_dims,
        }
    }

    /// Get the VT100 parser handle.
    ///
    /// Used for rendering the terminal content.
    #[must_use]
    pub fn parser_handle(&self) -> Arc<Mutex<Parser>> {
        Arc::clone(&self.vt100_parser)
    }

    /// Get the current mode.
    #[must_use]
    pub fn mode(&self) -> AppMode {
        self.mode
    }

    /// Get the selected agent key.
    #[must_use]
    pub fn selected_agent(&self) -> Option<&str> {
        self.selected_agent.as_deref()
    }

    /// Get the agent list.
    #[must_use]
    pub fn agents(&self) -> &[AgentInfo] {
        &self.agents
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
            self.poll_input()?;

            if self.should_quit() {
                break;
            }

            // 2. Poll PTY output and Lua events (via Hub output channel)
            self.poll_pty_events();

            // 4. Render
            self.render()?;

            // Small sleep to prevent CPU spinning (60 FPS max)
            std::thread::sleep(Duration::from_millis(16));
        }

        log::info!("TuiRunner event loop exiting");
        Ok(())
    }

    /// Poll for keyboard/mouse input and handle it.
    ///
    /// Drains all available events per frame to prevent scroll stall when
    /// rapid input (e.g., mouse wheel) queues events faster than render rate.
    fn poll_input(&mut self) -> Result<()> {
        // Drain all available events (0ms timeout = non-blocking check)
        while event::poll(Duration::from_millis(0))? {
            let ev = event::read()?;
            self.handle_input_event(&ev);
        }
        Ok(())
    }

    /// Handle a terminal input event.
    fn handle_input_event(&mut self, event: &Event) {
        // Build input context
        let context = InputContext {
            terminal_rows: self.terminal_dims.0,
            menu_selected: self.menu_selected,
            menu_count: {
                let ctx = self.build_menu_context();
                crate::tui::menu::selectable_count(&crate::tui::menu::build_menu(&ctx))
            },
            worktree_selected: self.worktree_selected,
            worktree_count: self.available_worktrees.len() + 1, // +1 for "Create New"
        };

        // Convert event to input result
        let result = process_event(event, &self.mode, &context);

        match result {
            InputResult::Action(action) => self.handle_tui_action(action),
            InputResult::PtyInput(data) => self.handle_pty_input(&data),
            InputResult::Resize { rows, cols } => self.handle_resize(rows, cols),
            InputResult::None => {}
        }
    }

    /// Handle PTY input (send to connected agent via Lua terminal subscription).
    fn handle_pty_input(&mut self, data: &[u8]) {
        if let Some(ref sub_id) = self.current_terminal_sub_id {
            self.send_msg(serde_json::json!({
                "subscriptionId": sub_id,
                "data": {
                    "type": "input",
                    "data": String::from_utf8_lossy(data),
                }
            }));
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

        // Resize the vt100 parser to match new terminal dimensions.
        // This is critical - without this, PTY output formatted for new dimensions
        // would be interpreted with old dimensions, causing garbled display.
        {
            let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
            parser.screen_mut().set_size(rows, cols);
        }

        // Propagate resize to the connected PTY via Lua terminal subscription.
        if let Some(ref sub_id) = self.current_terminal_sub_id {
            self.send_msg(serde_json::json!({
                "subscriptionId": sub_id,
                "data": {
                    "type": "resize",
                    "rows": rows,
                    "cols": cols,
                }
            }));
        }

        // Also update client-level dims via hub subscription so
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
    fn poll_pty_events(&mut self) {
        use tokio::sync::mpsc::error::TryRecvError;

        // Process up to 100 events per tick
        for _ in 0..100 {
            match self.output_rx.try_recv() {
                Ok(TuiOutput::Scrollback(data)) => {
                    // Feed historical output to TuiRunner's vt100 parser
                    let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
                    parser.process(&data);
                    log::debug!("Processed {} bytes of scrollback", data.len());
                }
                Ok(TuiOutput::Output(data)) => {
                    // Feed ongoing output to TuiRunner's vt100 parser
                    let mut parser = self.vt100_parser.lock().expect("parser lock poisoned");
                    parser.process(&data);
                }
                Ok(TuiOutput::ProcessExited { exit_code }) => {
                    log::info!("PTY process exited with code {:?}", exit_code);
                    // Process exited - we remain connected for any final output
                }
                Ok(TuiOutput::Message(value)) => {
                    self.handle_lua_message(value);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    log::debug!("PTY output channel disconnected");
                    // Channel closed - Hub was dropped or terminated.
                    // Unsubscribe from current terminal if connected.
                    if let Some(ref sub_id) = self.current_terminal_sub_id {
                        self.send_msg(serde_json::json!({
                            "type": "unsubscribe",
                            "subscriptionId": sub_id,
                        }));
                    }
                    self.current_terminal_sub_id = None;
                    self.current_agent_index = None;
                    self.current_pty_index = None;
                    self.selected_agent = None;
                    break;
                }
            }
        }
    }

    /// Render the TUI.
    fn render(&mut self) -> Result<()> {
        use super::render::{render, AgentRenderInfo, RenderContext};

        // Build agent render info from cached agents
        let agent_render_info: Vec<AgentRenderInfo> = self
            .agents
            .iter()
            .map(|info| AgentRenderInfo {
                key: info.id.clone(),
                repo: info.repo.clone().unwrap_or_default(),
                issue_number: info.issue_number.map(|n| n as u32),
                branch_name: info.branch_name.clone().unwrap_or_default(),
                port: info.port,
                server_running: info.server_running.unwrap_or(false),
            })
            .collect();

        // Calculate selected agent index
        let selected_agent_index = self
            .selected_agent
            .as_ref()
            .and_then(|key| self.agents.iter().position(|a| a.id == *key))
            .unwrap_or(0);

        // Build creating_agent reference
        let creating_agent_ref = self
            .creating_agent
            .as_ref()
            .map(|(id, stage)| (id.as_str(), *stage));

        // Check scroll state from parser
        let (scroll_offset, is_scrolled) = {
            let parser = self.vt100_parser.lock().expect("parser lock poisoned");
            let offset = parser.screen().scrollback();
            (offset, offset > 0)
        };

        // Connection code is cached from Lua responses (requested on ShowConnectionCode action)

        // Build render context from TuiRunner state
        let ctx = RenderContext {
            // UI State
            mode: self.mode,
            menu_selected: self.menu_selected,
            input_buffer: &self.input_buffer,
            worktree_selected: self.worktree_selected,
            available_worktrees: &self.available_worktrees,
            error_message: self.error_message.as_deref(),
            creating_agent: creating_agent_ref,
            connection_code: self.connection_code.as_ref(),
            bundle_used: false, // TuiRunner doesn't track this - would need from Hub

            // Agent State
            agent_ids: &[], // Not needed for rendering
            agents: &agent_render_info,
            selected_agent_index,

            // Terminal State - use TuiRunner's local parser
            active_parser: Some(self.parser_handle()),
            active_pty_view: self.active_pty_view,
            scroll_offset,
            is_scrolled,

            // Status Indicators - TuiRunner doesn't track these, use defaults
            seconds_since_poll: 0,
            poll_interval: 10,
            vpn_status: None,
        };

        render(&mut self.terminal, &ctx, None)?;

        Ok(())
    }

    /// Send a JSON message to Hub via the Lua client protocol.
    ///
    /// Hub routes these to `lua.call_tui_message()` which processes them
    /// through the same `Client:on_message()` path as browser clients.
    ///
    /// This is the sole method for TuiRunner to communicate with Hub.
    pub(super) fn send_msg(&self, msg: serde_json::Value) {
        if let Err(e) = self.request_tx.send(msg) {
            log::error!("Failed to send Lua message: {}", e);
        }
    }

    /// Show an error message.
    pub fn show_error(&mut self, message: impl Into<String>) {
        self.error_message = Some(message.into());
        self.mode = AppMode::Error;
    }

    /// Clear the error and return to normal mode.
    pub fn clear_error(&mut self) {
        self.error_message = None;
        self.mode = AppMode::Normal;
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

    // Create JSON channel for TuiRunner -> Hub communication
    let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

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

#[cfg(test)]
mod tests {
    //! TuiRunner tests - comprehensive end-to-end tests through the input chain.
    //!
    //! # Test Philosophy
    //!
    //! Tests in this module exercise real code paths via:
    //! 1. Keyboard events through `process_event()` -> `handle_input_event()` -> `handle_tui_action()`
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
    //!    `JSON` messages for inspection. Pre-populate `runner.agents`,
    //!    `runner.available_worktrees`, or `runner.connection_code` directly
    //!    in tests (these are normally delivered via Lua `TuiOutput::Message` events).
    //!
    //! # M-DESIGN-FOR-AI Compliance
    //!
    //! Tests follow MS Rust guidelines with canonical documentation format.

    use super::*;
    use crate::client::{CreateAgentRequest, DeleteAgentRequest};
    use crate::tui::actions::TuiAction;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
    /// This setup does NOT respond to messages. Pre-populate cached state
    /// (e.g., `runner.available_worktrees`) directly for tests needing data.
    /// Use `create_test_runner_with_mock_client` for flows requiring a
    /// responder thread.
    fn create_test_runner() -> (TuiRunner<TestBackend>, mpsc::UnboundedReceiver<serde_json::Value>) {
        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).expect("Failed to create test terminal");

        let (request_tx, request_rx) = mpsc::unbounded_channel::<serde_json::Value>();

        // Create output channel (Hub would send here, but we don't have one in tests)
        let (_output_tx, output_rx) = tokio::sync::mpsc::unbounded_channel();
        let shutdown = Arc::new(AtomicBool::new(false));

        let runner = TuiRunner::new(
            terminal,
            request_tx,
            output_rx,
            shutdown,
            (24, 80), // rows, cols
        );

        (runner, request_rx)
    }

    /// Creates a `TuiRunner` with a mock Hub responder for testing.
    ///
    /// Spawns a responder thread that passthroughs all `JSON` messages
    /// for verification. All request-response patterns have been migrated to
    /// Lua events — pre-populate `runner.available_worktrees`, `runner.agents`,
    /// or `runner.connection_code` directly in tests.
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
        mpsc::UnboundedReceiver<serde_json::Value>,
        Arc<AtomicBool>,
    ) {
        // Create our own request channel that we control
        // TuiRunner sends requests here, and the responder handles them
        let (request_tx, mut request_rx) = mpsc::unbounded_channel::<serde_json::Value>();
        let (passthrough_tx, passthrough_rx) = mpsc::unbounded_channel::<serde_json::Value>();

        let shutdown = Arc::new(AtomicBool::new(false));
        let responder_shutdown = Arc::clone(&shutdown);

        // Spawn request responder thread that passthroughs all JSON messages
        // for test verification. All TUI operations are now JSON messages
        // routed through the Lua client protocol.
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

        let runner = TuiRunner::new(
            terminal,
            request_tx,
            output_rx,
            Arc::clone(&shutdown),
            (24, 80),
        );

        (runner, output_tx, passthrough_rx, shutdown)
    }

    /// Builds an `InputContext` from the current runner state.
    fn runner_input_context(runner: &TuiRunner<TestBackend>) -> InputContext {
        InputContext {
            terminal_rows: runner.terminal_dims.0,
            menu_selected: runner.menu_selected,
            menu_count: {
                let ctx = runner.build_menu_context();
                crate::tui::menu::selectable_count(&crate::tui::menu::build_menu(&ctx))
            },
            worktree_selected: runner.worktree_selected,
            worktree_count: runner.available_worktrees.len() + 1,
        }
    }

    /// Creates a key event without modifiers.
    fn make_key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Creates a key event with Ctrl modifier.
    fn make_key_ctrl(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::CONTROL))
    }

    /// Creates a key event with Shift modifier.
    fn make_key_shift(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::SHIFT))
    }

    /// Processes a keyboard event through the full input pipeline.
    ///
    /// This exercises: `Event` -> `process_event()` -> `handle_input_event()` -> state change
    fn process_key(runner: &mut TuiRunner<TestBackend>, event: Event) {
        let context = runner_input_context(runner);
        let result = process_event(&event, &runner.mode(), &context);

        match result {
            InputResult::Action(action) => {
                runner.handle_tui_action(action);
            }
            InputResult::PtyInput(_) => {
                // PTY input goes to agent - not testing that path here
            }
            InputResult::Resize { rows, cols } => {
                runner.handle_resize(rows, cols);
            }
            InputResult::None => {}
        }
    }

    // =========================================================================
    // Display & Property Tests
    // =========================================================================

    /// Verifies `CreationStage` implements `Display` correctly.
    #[test]
    fn test_creation_stage_display() {
        assert_eq!(
            format!("{}", CreationStage::CreatingWorktree),
            "Creating worktree..."
        );
        assert_eq!(format!("{}", CreationStage::Ready), "Ready");
    }

    /// Verifies `CloseAgentConfirm` mode has correct properties.
    #[test]
    fn test_close_agent_confirm_mode_properties() {
        let mode = AppMode::CloseAgentConfirm;

        assert!(mode.is_modal(), "CloseAgentConfirm should be a modal");
        assert!(
            !mode.accepts_text_input(),
            "CloseAgentConfirm should not accept text input"
        );
        assert_eq!(mode.display_name(), "Confirm Close");
    }

    /// Verifies `ConnectionCode` mode has correct properties.
    #[test]
    fn test_connection_code_mode_properties() {
        let mode = AppMode::ConnectionCode;

        assert!(mode.is_modal(), "ConnectionCode should be a modal");
        assert!(
            !mode.accepts_text_input(),
            "ConnectionCode should not accept text input"
        );
    }

    /// Verifies new agent mode properties for each stage.
    #[test]
    fn test_new_agent_mode_properties() {
        assert!(AppMode::NewAgentSelectWorktree.is_modal());
        assert!(AppMode::NewAgentCreateWorktree.is_modal());
        assert!(AppMode::NewAgentPrompt.is_modal());

        assert!(!AppMode::NewAgentSelectWorktree.accepts_text_input());
        assert!(AppMode::NewAgentCreateWorktree.accepts_text_input());
        assert!(AppMode::NewAgentPrompt.accepts_text_input());
    }

    /// Verifies dynamic menu builds correctly for different contexts.
    ///
    /// Tests that the menu structure adapts based on context (agent selected,
    /// server PTY available, etc.) and that actions can be correctly retrieved
    /// by selection index.
    #[test]
    fn test_dynamic_menu_builds_correctly() {
        use crate::agent::PtyView;
        use crate::tui::menu::{build_menu, get_action_for_selection, MenuAction, MenuContext};

        // Menu without agent selected - should have Hub items only
        let ctx_no_agent = MenuContext {
            has_agent: false,
            active_pty: PtyView::Cli,
        };
        let menu = build_menu(&ctx_no_agent);

        // First selectable should be New Agent (after Hub header)
        assert_eq!(
            get_action_for_selection(&menu, 0),
            Some(MenuAction::NewAgent)
        );
        assert_eq!(
            get_action_for_selection(&menu, 1),
            Some(MenuAction::ShowConnectionCode)
        );

        // Menu with agent selected - always has View Server / View Agent toggle
        let ctx_with_agent = MenuContext {
            has_agent: true,
            active_pty: PtyView::Cli,
        };
        let menu = build_menu(&ctx_with_agent);

        // First selectable should be View Server (PTY toggle), then Close Agent
        assert_eq!(
            get_action_for_selection(&menu, 0),
            Some(MenuAction::TogglePtyView)
        );
        assert_eq!(
            get_action_for_selection(&menu, 1),
            Some(MenuAction::CloseAgent)
        );
        // Then Hub items
        assert_eq!(
            get_action_for_selection(&menu, 2),
            Some(MenuAction::NewAgent)
        );
    }

    /// Find the selection index for a given `MenuAction` in the current dynamic menu.
    ///
    /// The menu structure changes based on context (agent selected, server PTY available,
    /// etc.), so tests must use this helper instead of assuming fixed indices.
    ///
    /// # Arguments
    ///
    /// * `runner` - The `TuiRunner` whose state determines the menu context
    /// * `target_action` - The action to find in the menu
    ///
    /// # Returns
    ///
    /// The selection index (0-based among selectable items) if the action exists,
    /// or `None` if the action is not in the current menu configuration.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let idx = find_menu_action_index(&runner, MenuAction::NewAgent).unwrap();
    /// navigate_to_menu_index(&mut runner, idx);
    /// process_key(&mut runner, make_key(KeyCode::Enter));
    /// ```
    fn find_menu_action_index(
        runner: &TuiRunner<TestBackend>,
        target_action: crate::tui::menu::MenuAction,
    ) -> Option<usize> {
        use crate::tui::menu::{build_menu, get_action_for_selection, selectable_count};

        let menu_context = runner.build_menu_context();
        let menu_items = build_menu(&menu_context);
        let count = selectable_count(&menu_items);

        for idx in 0..count {
            if get_action_for_selection(&menu_items, idx) == Some(target_action) {
                return Some(idx);
            }
        }
        None
    }

    /// Navigate to a specific menu selection index from index 0.
    ///
    /// Presses Down the required number of times to reach the target index.
    /// Assumes the menu is already open and `menu_selected` is 0.
    ///
    /// # Arguments
    ///
    /// * `runner` - The TuiRunner to navigate
    /// * `target_idx` - The target selection index to navigate to
    fn navigate_to_menu_index(runner: &mut TuiRunner<TestBackend>, target_idx: usize) {
        for _ in 0..target_idx {
            process_key(runner, make_key(KeyCode::Down));
        }
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

    /// Verifies `TuiAction` confirm close variants are distinct.
    #[test]
    fn test_tui_action_confirm_close_variants() {
        let keep = TuiAction::ConfirmCloseAgent;
        let delete = TuiAction::ConfirmCloseAgentDeleteWorktree;

        assert_ne!(keep, delete, "Confirm variants should be distinct");
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

        assert_eq!(runner.mode(), AppMode::Normal);

        process_key(&mut runner, make_key_ctrl(KeyCode::Char('p')));

        assert_eq!(runner.mode(), AppMode::Menu, "Ctrl+P should open menu");
    }

    /// Verifies menu navigation with arrow keys.
    #[test]
    fn test_e2e_menu_arrow_navigation() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Open menu
        process_key(&mut runner, make_key_ctrl(KeyCode::Char('p')));
        assert_eq!(runner.mode(), AppMode::Menu);
        assert_eq!(runner.menu_selected, 0);

        // Navigate down (menu has 2 items: New Agent, Connection Code)
        process_key(&mut runner, make_key(KeyCode::Down));
        assert_eq!(runner.menu_selected, 1);

        // Should clamp at max (1)
        process_key(&mut runner, make_key(KeyCode::Down));
        assert_eq!(runner.menu_selected, 1);

        // Navigate up
        process_key(&mut runner, make_key(KeyCode::Up));
        assert_eq!(runner.menu_selected, 0);

        // Close with Escape
        process_key(&mut runner, make_key(KeyCode::Esc));
        assert_eq!(runner.mode(), AppMode::Normal);
    }

    /// Verifies menu up does not go below zero.
    #[test]
    fn test_e2e_menu_up_clamps_at_zero() {
        let (mut runner, _cmd_rx) = create_test_runner();

        process_key(&mut runner, make_key_ctrl(KeyCode::Char('p')));
        assert_eq!(runner.menu_selected, 0);

        process_key(&mut runner, make_key(KeyCode::Up));
        assert_eq!(runner.menu_selected, 0, "Should not go below 0");
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
        use crate::tui::menu::MenuAction;

        let (mut runner, _cmd_rx) = create_test_runner();

        // Open menu (no agent selected)
        process_key(&mut runner, make_key_ctrl(KeyCode::Char('p')));
        assert_eq!(runner.mode(), AppMode::Menu);

        // Find what action is at index 1 (which corresponds to pressing '2')
        let action_at_1 = find_menu_action_index(&runner, MenuAction::ShowConnectionCode);
        assert_eq!(
            action_at_1,
            Some(1),
            "ShowConnectionCode should be at index 1 when no agent selected"
        );

        // Press '2' to select the item at index 1
        process_key(&mut runner, make_key(KeyCode::Char('2')));

        assert_eq!(
            runner.mode(),
            AppMode::ConnectionCode,
            "Number shortcut '2' should select ShowConnectionCode"
        );
    }

    /// Verifies Ctrl+Q triggers quit.
    #[test]
    fn test_e2e_ctrl_q_quits() {
        let (mut runner, _cmd_rx) = create_test_runner();

        assert!(!runner.quit);

        process_key(&mut runner, make_key_ctrl(KeyCode::Char('q')));

        assert!(runner.quit, "Ctrl+Q should set quit flag");
    }

    /// Verifies plain keys in Normal mode go to PTY, not actions.
    #[test]
    fn test_e2e_normal_mode_keys_go_to_pty() {
        let (runner, _cmd_rx) = create_test_runner();
        let context = runner_input_context(&runner);

        // Plain 'q' should go to PTY (not quit)
        let result = process_event(&make_key(KeyCode::Char('q')), &runner.mode(), &context);
        assert!(
            matches!(result, InputResult::PtyInput(_)),
            "Plain 'q' should go to PTY"
        );

        // Plain 'p' should go to PTY (not open menu)
        let result = process_event(&make_key(KeyCode::Char('p')), &runner.mode(), &context);
        assert!(
            matches!(result, InputResult::PtyInput(_)),
            "Plain 'p' should go to PTY"
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
        use crate::tui::menu::MenuAction;

        let (mut runner, _cmd_rx) = create_test_runner();

        // 1. Open menu
        process_key(&mut runner, make_key_ctrl(KeyCode::Char('p')));
        assert_eq!(runner.mode(), AppMode::Menu);

        // 2. Find and navigate to Connection Code using dynamic menu lookup
        let connection_idx = find_menu_action_index(&runner, MenuAction::ShowConnectionCode)
            .expect("ShowConnectionCode should be in menu");
        navigate_to_menu_index(&mut runner, connection_idx);
        assert_eq!(runner.menu_selected, connection_idx);

        // 3. Select with Enter
        process_key(&mut runner, make_key(KeyCode::Enter));
        assert_eq!(runner.mode(), AppMode::ConnectionCode);

        // 4. Close with Escape
        process_key(&mut runner, make_key(KeyCode::Esc));
        assert_eq!(runner.mode(), AppMode::Normal);
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
        use crate::tui::menu::MenuAction;

        let (mut runner, _output_tx, mut request_rx, shutdown) = create_test_runner_with_mock_client();

        // Pre-populate worktrees (normally delivered via Lua worktree_list event)
        runner.available_worktrees = vec![("/path/worktree-1".to_string(), "feature-1".to_string())];

        // 1. Open menu and navigate to New Agent using dynamic lookup
        process_key(&mut runner, make_key_ctrl(KeyCode::Char('p')));

        let new_agent_idx = find_menu_action_index(&runner, MenuAction::NewAgent)
            .expect("NewAgent should be in menu");
        navigate_to_menu_index(&mut runner, new_agent_idx);
        assert_eq!(runner.menu_selected, new_agent_idx);

        process_key(&mut runner, make_key(KeyCode::Enter));

        // Small delay to let responder process messages
        thread::sleep(Duration::from_millis(10));

        assert_eq!(
            runner.mode(),
            AppMode::NewAgentSelectWorktree,
            "Should enter worktree selection"
        );

        // 2. Select "Create new worktree" (index 0)
        assert_eq!(runner.worktree_selected, 0);
        process_key(&mut runner, make_key(KeyCode::Enter));

        assert_eq!(runner.mode(), AppMode::NewAgentCreateWorktree);

        // 3. Type issue name
        for c in "issue-42".chars() {
            process_key(&mut runner, make_key(KeyCode::Char(c)));
        }
        assert_eq!(runner.input_buffer, "issue-42");

        // 4. Submit issue name
        process_key(&mut runner, make_key(KeyCode::Enter));

        assert_eq!(runner.mode(), AppMode::NewAgentPrompt);
        assert_eq!(runner.pending_issue_or_branch, Some("issue-42".to_string()));

        // 5. Type prompt and submit
        for c in "Fix bug".chars() {
            process_key(&mut runner, make_key(KeyCode::Char(c)));
        }
        process_key(&mut runner, make_key(KeyCode::Enter));

        // Wait for responder to process
        thread::sleep(Duration::from_millis(10));

        // Verify create_agent JSON message (skip list_worktrees request)
        let mut found_create = false;
        while let Ok(msg) = request_rx.try_recv() {
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

        // Modal closes immediately after submit - progress shown in sidebar
        assert_eq!(
            runner.mode(),
            AppMode::Normal,
            "Modal should close immediately after submit"
        );

        // creating_agent should be set to indicate pending creation (shown in sidebar)
        assert!(
            runner.creating_agent.is_some(),
            "creating_agent should be set to track pending creation"
        );
        assert_eq!(
            runner.creating_agent.as_ref().map(|(id, _)| id.as_str()),
            Some("issue-42"),
            "creating_agent should track the correct identifier"
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
        use crate::tui::menu::MenuAction;

        let (mut runner, _output_tx, mut request_rx, shutdown) = create_test_runner_with_mock_client();

        // Pre-populate worktrees (normally delivered via Lua worktree_list event)
        runner.available_worktrees = vec![
            ("/path/worktree-1".to_string(), "feature-branch".to_string()),
            ("/path/worktree-2".to_string(), "bugfix-branch".to_string()),
        ];

        // Open menu and navigate to New Agent using dynamic lookup
        process_key(&mut runner, make_key_ctrl(KeyCode::Char('p')));

        let new_agent_idx = find_menu_action_index(&runner, MenuAction::NewAgent)
            .expect("NewAgent should be in menu");
        navigate_to_menu_index(&mut runner, new_agent_idx);
        assert_eq!(runner.menu_selected, new_agent_idx);

        process_key(&mut runner, make_key(KeyCode::Enter));

        thread::sleep(Duration::from_millis(10));
        assert_eq!(runner.mode(), AppMode::NewAgentSelectWorktree);

        // Navigate to first existing worktree (index 1)
        process_key(&mut runner, make_key(KeyCode::Down));
        assert_eq!(runner.worktree_selected, 1);

        // Select existing worktree
        process_key(&mut runner, make_key(KeyCode::Enter));

        thread::sleep(Duration::from_millis(10));

        // Modal closes immediately after selection - progress shown in sidebar
        assert_eq!(
            runner.mode(),
            AppMode::Normal,
            "Modal should close immediately after worktree selection"
        );

        // creating_agent should be set to indicate pending creation (shown in sidebar)
        assert!(
            runner.creating_agent.is_some(),
            "creating_agent should be set to track pending creation"
        );
        assert_eq!(
            runner.creating_agent.as_ref().map(|(id, _)| id.as_str()),
            Some("feature-branch"),
            "creating_agent should track the correct identifier"
        );

        // Verify reopen_worktree JSON message with path
        let mut found_create = false;
        while let Ok(msg) = request_rx.try_recv() {
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

        // Bypass to NewAgentCreateWorktree mode directly
        runner.mode = AppMode::NewAgentCreateWorktree;

        // Submit empty input
        process_key(&mut runner, make_key(KeyCode::Enter));

        // Should stay in same mode
        assert_eq!(
            runner.mode(),
            AppMode::NewAgentCreateWorktree,
            "Empty issue name should be rejected"
        );
    }

    /// Verifies cancel at each stage returns to Normal.
    #[test]
    fn test_e2e_cancel_agent_creation_at_each_stage() {
        let (mut runner, mut cmd_rx) = create_test_runner();

        // Cancel at worktree selection
        runner.mode = AppMode::NewAgentSelectWorktree;
        process_key(&mut runner, make_key(KeyCode::Esc));
        assert_eq!(runner.mode(), AppMode::Normal);

        // Cancel at issue input
        runner.mode = AppMode::NewAgentCreateWorktree;
        runner.input_buffer = "partial".to_string();
        process_key(&mut runner, make_key(KeyCode::Esc));
        assert_eq!(runner.mode(), AppMode::Normal);
        assert!(runner.input_buffer.is_empty(), "Buffer should be cleared");

        // Cancel at prompt
        runner.mode = AppMode::NewAgentPrompt;
        runner.pending_issue_or_branch = Some("issue-123".to_string());
        process_key(&mut runner, make_key(KeyCode::Esc));
        assert_eq!(runner.mode(), AppMode::Normal);

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

        runner.mode = AppMode::NewAgentCreateWorktree;

        // Type characters
        process_key(&mut runner, make_key(KeyCode::Char('a')));
        process_key(&mut runner, make_key(KeyCode::Char('b')));
        process_key(&mut runner, make_key(KeyCode::Char('c')));
        assert_eq!(runner.input_buffer, "abc");

        // Backspace
        process_key(&mut runner, make_key(KeyCode::Backspace));
        assert_eq!(runner.input_buffer, "ab");

        process_key(&mut runner, make_key(KeyCode::Backspace));
        process_key(&mut runner, make_key(KeyCode::Backspace));
        assert_eq!(runner.input_buffer, "");

        // Backspace on empty is safe
        process_key(&mut runner, make_key(KeyCode::Backspace));
        assert_eq!(runner.input_buffer, "");
    }

    /// Verifies worktree navigation with arrow keys.
    #[test]
    fn test_e2e_worktree_navigation() {
        let (mut runner, _cmd_rx) = create_test_runner();

        runner.available_worktrees = vec![
            ("/path/1".to_string(), "branch-1".to_string()),
            ("/path/2".to_string(), "branch-2".to_string()),
        ];
        runner.mode = AppMode::NewAgentSelectWorktree;
        runner.worktree_selected = 0;

        // Navigate down
        process_key(&mut runner, make_key(KeyCode::Down));
        assert_eq!(runner.worktree_selected, 1);

        process_key(&mut runner, make_key(KeyCode::Down));
        assert_eq!(runner.worktree_selected, 2);

        // Should not exceed max
        process_key(&mut runner, make_key(KeyCode::Down));
        assert_eq!(runner.worktree_selected, 2);

        // Navigate up
        process_key(&mut runner, make_key(KeyCode::Up));
        assert_eq!(runner.worktree_selected, 1);

        process_key(&mut runner, make_key(KeyCode::Up));
        assert_eq!(runner.worktree_selected, 0);

        // Should not go below 0
        process_key(&mut runner, make_key(KeyCode::Up));
        assert_eq!(runner.worktree_selected, 0);
    }

    // =========================================================================
    // E2E Scroll Tests
    // =========================================================================

    /// Verifies scroll key bindings produce correct actions.
    #[test]
    fn test_e2e_scroll_keys() {
        let (runner, _cmd_rx) = create_test_runner();
        let context = runner_input_context(&runner);

        // Shift+PageUp for scroll up
        let result = process_event(&make_key_shift(KeyCode::PageUp), &runner.mode(), &context);
        assert!(
            matches!(result, InputResult::Action(TuiAction::ScrollUp(_))),
            "Shift+PageUp should scroll up"
        );

        // Shift+PageDown for scroll down
        let result = process_event(&make_key_shift(KeyCode::PageDown), &runner.mode(), &context);
        assert!(
            matches!(result, InputResult::Action(TuiAction::ScrollDown(_))),
            "Shift+PageDown should scroll down"
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

    /// Verifies Ctrl+J/K produce SelectNext/SelectPrevious actions.
    #[test]
    fn test_e2e_agent_navigation_keybindings() {
        let (runner, _cmd_rx) = create_test_runner();
        let context = runner_input_context(&runner);

        let result = process_event(&make_key_ctrl(KeyCode::Char('j')), &runner.mode(), &context);
        assert_eq!(
            result,
            InputResult::Action(TuiAction::SelectNext),
            "Ctrl+J should be SelectNext"
        );

        let result = process_event(&make_key_ctrl(KeyCode::Char('k')), &runner.mode(), &context);
        assert_eq!(
            result,
            InputResult::Action(TuiAction::SelectPrevious),
            "Ctrl+K should be SelectPrevious"
        );
    }

    /// Verifies Ctrl+] produces TogglePtyView action.
    #[test]
    fn test_e2e_pty_toggle_keybinding() {
        let (runner, _cmd_rx) = create_test_runner();
        let context = runner_input_context(&runner);

        let result = process_event(&make_key_ctrl(KeyCode::Char(']')), &runner.mode(), &context);
        assert_eq!(
            result,
            InputResult::Action(TuiAction::TogglePtyView),
            "Ctrl+] should be TogglePtyView"
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
        let (mut runner, mut cmd_rx) = create_test_runner();

        assert!(!runner.quit);

        runner.handle_tui_action(TuiAction::Quit);

        // Local quit flag stops TUI event loop
        assert!(runner.quit, "Quit should set local quit flag");

        // JSON message tells Hub to quit via Lua
        match cmd_rx.try_recv() {
            Ok(msg) => {
                assert_eq!(
                    msg.get("subscriptionId").and_then(|v| v.as_str()),
                    Some("tui_hub"),
                    "Should target tui_hub subscription"
                );
                assert_eq!(
                    msg.get("data")
                        .and_then(|d| d.get("type"))
                        .and_then(|t| t.as_str()),
                    Some("quit"),
                    "Should send quit command"
                );
            }
            Err(_) => panic!("Should send quit JSON message to Hub"),
        }
    }

    /// Verifies None action is a no-op.
    #[test]
    fn test_none_action_is_noop() {
        let (mut runner, _cmd_rx) = create_test_runner();

        let mode_before = runner.mode();
        let selected_before = runner.menu_selected;

        runner.handle_tui_action(TuiAction::None);

        assert_eq!(runner.mode(), mode_before);
        assert_eq!(runner.menu_selected, selected_before);
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

    /// Verifies that empty cached worktree list is handled gracefully.
    ///
    /// When selecting "New Agent" from the menu, the TUI uses the cached
    /// worktree list (populated by Lua events). If no worktrees have been
    /// received yet, the list is empty, allowing graceful degradation.
    #[test]
    fn test_list_worktrees_empty_cache_graceful_handling() {
        use crate::tui::menu::MenuAction;

        let (mut runner, _cmd_rx) = create_test_runner();

        // Find the menu selection index for NewAgent using dynamic lookup
        let new_agent_idx = find_menu_action_index(&runner, MenuAction::NewAgent)
            .expect("NewAgent should always be in menu");

        // Select "New Agent" which uses cached worktrees (empty)
        runner.handle_menu_select(new_agent_idx);

        // Mode should transition to worktree selection
        assert_eq!(
            runner.mode(),
            AppMode::NewAgentSelectWorktree,
            "Should enter worktree selection even with empty cache"
        );

        // Worktree list should be empty (no events received yet)
        assert!(
            runner.available_worktrees.is_empty(),
            "Worktrees should be empty before events arrive"
        );
    }

    /// Verifies that closing command channel during agent creation is handled gracefully.
    ///
    /// If the Hub channel closes during `create_agent_blocking`, the TUI should
    /// not panic and should return to Normal mode.
    #[test]
    fn test_create_agent_channel_closed_graceful() {
        let (mut runner, cmd_rx) = create_test_runner();
        drop(cmd_rx);

        // Setup state as if we're about to create agent
        runner.mode = AppMode::NewAgentPrompt;
        runner.pending_issue_or_branch = Some("test-issue".to_string());
        runner.input_buffer = "test prompt".to_string();

        // Submit should attempt to send command but fail gracefully
        runner.handle_tui_action(TuiAction::InputSubmit);

        // Should return to Normal (the call fails but mode still transitions)
        assert_eq!(runner.mode(), AppMode::Normal);
    }

    /// Verifies that closing command channel during agent deletion is handled gracefully.

    // =========================================================================
    // TDD Tests - Expected to FAIL until bugs are fixed
    // =========================================================================
    //
    // These tests expose bugs in the current agent creation flow:
    //
    // 1. Response channel is ignored (`_rx` dropped) - fire-and-forget pattern
    //    - See runner_input.rs:167 and :218 - `let (cmd, _rx) = ...`
    //
    // 2. Mode transitions to Normal regardless of command success
    //    - See runner_input.rs:171 and :223 - immediate `self.mode = AppMode::Normal`

    /// Verifies modal closes immediately after submit, with progress tracked in sidebar.
    ///
    /// # Design
    ///
    /// When user submits agent creation, the modal closes immediately for better UX.
    /// The `creating_agent` field tracks the pending creation and is displayed in
    /// the sidebar. When the agent is created or an error occurs, the TUI updates
    /// accordingly via Lua event callbacks.
    ///
    /// This is the correct behavior because:
    /// 1. User doesn't need to stare at a frozen modal
    /// 2. Progress is visible in the sidebar ("Creating worktree...")
    /// 3. Errors are shown in error mode
    #[test]
    fn test_creation_modal_closes_immediately() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Setup state for creation
        runner.mode = AppMode::NewAgentPrompt;
        runner.pending_issue_or_branch = Some("fail-branch".to_string());
        runner.input_buffer.clear();

        // Submit the creation
        runner.handle_tui_action(TuiAction::InputSubmit);

        // Modal closes immediately - progress tracked via creating_agent
        assert_eq!(
            runner.mode(),
            AppMode::Normal,
            "Modal should close immediately after submit"
        );

        // creating_agent should be set to track the pending creation
        assert!(
            runner.creating_agent.is_some(),
            "creating_agent should be set to track pending creation"
        );
        assert_eq!(
            runner.creating_agent.as_ref().map(|(id, _)| id.as_str()),
            Some("fail-branch"),
            "creating_agent should track the correct identifier"
        );
    }

    /// **FAILING TEST**: Verifies TUI shows "creating" state during async creation.
    ///
    /// # Why This Should Fail
    ///
    /// The proper UX for async agent creation should be:
    /// 1. User submits creation request
    /// 2. TUI enters a "Creating Agent..." state (not Normal)
    /// 3. TUI shows progress updates
    /// 4. TUI transitions to Normal only after AgentCreated or Error
    ///
    /// Current behavior:
    /// 1. User submits creation request
    /// 2. Command sent, `_rx` dropped, mode = Normal immediately
    /// 3. User thinks it's done
    /// 4. (In background) Agent actually gets created
    /// 5. AgentCreated event arrives but user already moved on
    ///
    /// # Bug Exposed
    ///
    /// Fire-and-forget pattern provides no feedback during async operations.
    #[test]
    fn test_creating_state_shown_during_async_creation() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Simulate starting creation
        runner.mode = AppMode::NewAgentPrompt;
        runner.pending_issue_or_branch = Some("test-issue".to_string());
        runner.input_buffer = "test prompt".to_string();

        // Submit creation
        runner.handle_tui_action(TuiAction::InputSubmit);

        // BUG: Mode is already Normal, should be something like AppMode::Creating
        // or we should have creating_agent set to show progress
        //
        // This assertion FAILS because mode transitions to Normal immediately
        // in handle_input_submit() without waiting for any confirmation.
        assert!(
            runner.mode() != AppMode::Normal || runner.creating_agent.is_some(),
            "TUI should indicate creation is in progress. \
             Mode: {:?}, creating_agent: {:?}. \
             BUG: Mode transitions to Normal immediately, no 'creating' indicator.",
            runner.mode(),
            runner.creating_agent
        );
    }

    // =========================================================================
    // Connection Code Tests
    // =========================================================================

    /// Verifies ConnectionCode mode renders gracefully when no connection code is cached.
    ///
    /// # Purpose
    ///
    /// When displaying the QR code modal (AppMode::ConnectionCode), the TUI uses
    /// the cached `self.connection_code` (populated via Lua event responses). If
    /// no code is available yet (e.g., Hub hasn't responded), render should still
    /// complete without panicking.
    #[test]
    fn test_connection_code_mode_renders_without_panic_on_hub_error() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Set mode to ConnectionCode with no cached code
        runner.mode = AppMode::ConnectionCode;
        runner.connection_code = None;

        // Render should not panic even without cached connection code
        let result = runner.render();
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
        assert_eq!(runner.mode, AppMode::Normal);

        // Render in Normal mode should succeed without any Hub calls
        let result = runner.render();
        assert!(result.is_ok(), "Render should succeed in Normal mode");
    }

    /// Verifies that pressing 'R' in ConnectionCode mode stays in that mode.
    ///
    /// # Purpose
    ///
    /// When refreshing the connection code, the modal should stay open
    /// and a regeneration request should be sent.
    #[test]
    fn test_regenerate_connection_code_stays_in_mode() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Setup: in ConnectionCode mode
        runner.mode = AppMode::ConnectionCode;

        // Action: regenerate connection code
        runner.handle_tui_action(TuiAction::RegenerateConnectionCode);

        // Verify: we stay in ConnectionCode mode
        assert_eq!(
            runner.mode,
            AppMode::ConnectionCode,
            "Should stay in ConnectionCode mode after refresh"
        );
    }

    /// Verifies that refresh sends a JSON message via Lua client protocol.
    ///
    /// # Purpose
    ///
    /// The TUI must remain responsive during refresh. The regeneration request
    /// is sent as a JSON message through the same Lua client.lua protocol as
    /// browser clients, using `send_msg()` (fire-and-forget).
    #[test]
    fn test_regenerate_sends_lua_json_message() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Setup: in ConnectionCode mode
        runner.mode = AppMode::ConnectionCode;

        // Action: regenerate connection code
        runner.handle_tui_action(TuiAction::RegenerateConnectionCode);

        // Verify: JSON message sent via Lua client protocol
        match request_rx.try_recv() {
            Ok(msg) => {
                let data = msg.get("data").expect("Should have data field");
                assert_eq!(
                    data.get("type").and_then(|t| t.as_str()),
                    Some("regenerate_connection_code"),
                    "Should send regenerate_connection_code via Lua"
                );
                assert_eq!(
                    msg.get("subscriptionId").and_then(|s| s.as_str()),
                    Some("tui_hub"),
                    "Should target tui_hub subscription"
                );
            }
            Err(mpsc::error::TryRecvError::Empty) => {
                panic!("Should send regenerate_connection_code JSON message");
            }
            Err(e) => {
                panic!("Channel error: {:?}", e);
            }
        }
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

    /// Helper to create test `AgentInfo` entries for navigation tests.
    ///
    /// Returns a Vec of `AgentInfo` with unique IDs based on the given count.
    fn make_test_agents(count: usize) -> Vec<AgentInfo> {
        (0..count)
            .map(|i| AgentInfo {
                id: format!("agent-{}", i),
                repo: None,
                issue_number: None,
                branch_name: Some(format!("branch-{}", i)),
                name: None,
                status: Some("Running".to_string()),
                port: None,
                server_running: None,
                has_server_pty: None,
                active_pty_view: None,
                scroll_offset: None,
                hub_identifier: None,
            })
            .collect()
    }

    /// Verifies `request_select_next()` sends a subscribe message for agent 1 when agent 0 is selected.
    ///
    /// # Scenario
    ///
    /// Given 3 agents with agent 0 currently selected, pressing "next" should
    /// advance to agent 1. The subscribe message is sent via `JSON::Message`
    /// using the Lua protocol (same as browser clients).
    #[test]
    fn test_select_next_agent_sends_subscribe() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Setup: 3 agents, agent 0 selected
        runner.agents = make_test_agents(3);
        runner.selected_agent = Some("agent-0".to_string());

        // Action: select next agent
        runner.request_select_next();

        // Verify: subscribe message sent for agent index 1
        match request_rx.try_recv() {
            Ok(msg) => {
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
        assert_eq!(runner.current_terminal_sub_id, Some("tui_term".to_string()));
    }

    /// Verifies `request_select_previous()` wraps from agent 0 to last agent (index 2).
    ///
    /// # Scenario
    ///
    /// Given 3 agents with agent 0 selected, pressing "previous" should wrap
    /// around to agent 2 (the last agent).
    #[test]
    fn test_select_previous_agent_wraps_around() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Setup: 3 agents, agent 0 selected
        runner.agents = make_test_agents(3);
        runner.selected_agent = Some("agent-0".to_string());

        // Action: select previous (should wrap to last)
        runner.request_select_previous();

        // Verify: subscribe message sent for agent index 2
        match request_rx.try_recv() {
            Ok(msg) => {
                assert_eq!(msg.get("type").and_then(|v| v.as_str()), Some("subscribe"));
                let params = msg.get("params").expect("should have params");
                assert_eq!(params.get("agent_index").and_then(|v| v.as_u64()), Some(2));
            }
            Err(_) => panic!("Expected subscribe message to be sent"),
        }

        // Verify local state updated
        assert_eq!(runner.selected_agent.as_deref(), Some("agent-2"));
        assert_eq!(runner.current_agent_index, Some(2));
    }

    /// Verifies `request_select_next()` wraps from last agent (index 2) to first (index 0).
    ///
    /// # Scenario
    ///
    /// Given 3 agents with agent 2 (last) selected, pressing "next" should wrap
    /// around to agent 0 (the first agent).
    #[test]
    fn test_select_next_wraps_around() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Setup: 3 agents, last agent selected
        runner.agents = make_test_agents(3);
        runner.selected_agent = Some("agent-2".to_string());

        // Action: select next (should wrap to first)
        runner.request_select_next();

        // Verify: subscribe message sent for agent index 0
        match request_rx.try_recv() {
            Ok(msg) => {
                assert_eq!(msg.get("type").and_then(|v| v.as_str()), Some("subscribe"));
                let params = msg.get("params").expect("should have params");
                assert_eq!(params.get("agent_index").and_then(|v| v.as_u64()), Some(0));
            }
            Err(_) => panic!("Expected subscribe message to be sent"),
        }

        assert_eq!(runner.selected_agent.as_deref(), Some("agent-0"));
        assert_eq!(runner.current_agent_index, Some(0));
    }

    /// Verifies `request_select_next()` sends unsubscribe before subscribe when already subscribed.
    ///
    /// # Scenario
    ///
    /// When switching from one agent to another, the TUI must unsubscribe from
    /// the current terminal before subscribing to the new one.
    #[test]
    fn test_select_agent_unsubscribes_then_subscribes() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Setup: 3 agents, agent 0 selected with active subscription
        runner.agents = make_test_agents(3);
        runner.selected_agent = Some("agent-0".to_string());
        runner.current_terminal_sub_id = Some("tui_term".to_string());

        // Action: select next agent
        runner.request_select_next();

        // Verify: first message is unsubscribe
        match request_rx.try_recv() {
            Ok(msg) => {
                assert_eq!(
                    msg.get("type").and_then(|v| v.as_str()),
                    Some("unsubscribe"),
                    "First message should be unsubscribe"
                );
                assert_eq!(
                    msg.get("subscriptionId").and_then(|v| v.as_str()),
                    Some("tui_term")
                );
            }
            Err(_) => panic!("Expected unsubscribe message to be sent"),
        }

        // Verify: second message is subscribe
        match request_rx.try_recv() {
            Ok(msg) => {
                assert_eq!(
                    msg.get("type").and_then(|v| v.as_str()),
                    Some("subscribe"),
                    "Second message should be subscribe"
                );
                assert_eq!(msg.get("channel").and_then(|v| v.as_str()), Some("terminal"));
            }
            Err(_) => panic!("Expected subscribe message to be sent"),
        }
    }

    /// Verifies `request_select_next()` is a no-op when agent list is empty.
    ///
    /// # Scenario
    ///
    /// With 0 agents, navigation should short-circuit without sending any
    /// JSON. This avoids index-out-of-bounds and unnecessary channel traffic.
    #[test]
    fn test_select_agent_with_empty_list_is_noop() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Setup: no agents
        assert!(runner.agents.is_empty());

        // Action: select next with no agents
        runner.request_select_next();

        // Verify: no request sent (early return in request_select_next)
        assert!(
            request_rx.try_recv().is_err(),
            "No JSON should be sent when agent list is empty"
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
    /// 2. Resize the vt100 parser
    /// 3. Send resize via terminal subscription JSON message
    /// 4. Send client-level resize via hub subscription
    #[test]
    fn test_handle_resize_sends_resize_via_lua() {
        let (mut runner, mut request_rx) = create_test_runner();

        // Set up connected state with terminal subscription
        runner.current_agent_index = Some(0);
        runner.current_pty_index = Some(0);
        runner.current_terminal_sub_id = Some("tui_term".to_string());

        // Action: resize to 40 rows x 120 cols
        runner.handle_resize(40, 120);

        // Verify: terminal resize JSON message sent
        match request_rx.try_recv() {
            Ok(msg) => {
                assert_eq!(msg["subscriptionId"], "tui_term");
                let data = &msg["data"];
                assert_eq!(data["type"], "resize");
                assert_eq!(data["rows"], 40);
                assert_eq!(data["cols"], 120);
            }
            Err(_) => panic!("Expected resize message to be sent"),
        }

        // Verify: hub-level resize also sent
        match request_rx.try_recv() {
            Ok(msg) => {
                assert_eq!(msg["subscriptionId"], "tui_hub");
                assert_eq!(msg["data"]["type"], "resize");
            }
            Err(_) => panic!("Expected hub resize message to be sent"),
        }

        // Verify: local state also updated
        assert_eq!(runner.terminal_dims, (40, 120));
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
            Ok(msg) => {
                assert_eq!(msg["subscriptionId"], "tui_hub");
                assert_eq!(msg["data"]["type"], "resize");
            }
            _ => panic!("Expected hub resize message"),
        }

        // Verify: no terminal resize sent (order: terminal first, then hub)
        // Since no terminal sub, the hub resize was the first and only message
        assert!(
            request_rx.try_recv().is_err(),
            "No additional messages should be sent"
        );

        // Verify: local state still updated
        assert_eq!(runner.terminal_dims, (40, 120));
    }

    // Rust guideline compliant 2026-02
}


