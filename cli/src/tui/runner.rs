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
//! ├── command_tx  - send commands to Hub
//! ├── hub_event_rx  - receive broadcasts from Hub
//! └── pty_rx  - receive PTY output for selected agent
//! ```
//!
//! # Event Loop
//!
//! The TuiRunner event loop:
//! 1. Polls for keyboard/mouse input
//! 2. Polls for Hub broadcast events
//! 3. Polls for PTY output (if agent selected)
//! 4. Renders the UI
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
//! - [`super::runner_handlers`] - `handle_tui_action()`, `handle_hub_event()`
//! - [`super::runner_agent`] - Agent navigation (`request_select_next()`, etc.)
//! - [`super::runner_input`] - Input handlers (`handle_menu_select()`, etc.)

// Rust guideline compliant 2026-01

use std::io::Stdout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event};
use ratatui::backend::Backend;
use ratatui::Terminal;
use tokio::sync::broadcast;
use vt100::Parser;

use ratatui::backend::CrosstermBackend;

use crate::agent::pty::PtyEvent;
use crate::app::AppMode;
use crate::client::TuiClient;
use crate::constants;
use crate::hub::{AgentHandle, Hub, HubCommandSender, HubEvent, HubHandle};
use crate::relay::{browser, AgentInfo};
use crate::tui::layout::terminal_widget_inner_area;

use super::actions::InputResult;
use super::events::CreationStage;
use super::input::{process_event, InputContext};

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
/// TuiRunner owns a TuiClient instance and delegates shared state to it:
/// - selected_agent, active_pty_view, pty_rx, pty_handle -> TuiClient
/// - mode, menu_selected, terminal, vt100_parser -> TuiRunner (TUI-specific)
///
/// This avoids state duplication between the two types.
pub struct TuiRunner<B: Backend> {
    // === Client (owns shared state) ===
    /// The TuiClient instance that implements the Client trait.
    ///
    /// Owns: selected_agent, active_pty_view, pty_event_rx, current_pty_handle.
    /// TuiRunner delegates to client for these fields.
    pub(super) client: TuiClient,

    // === Terminal ===
    /// VT100 parser for terminal emulation.
    ///
    /// Receives PTY output from selected agent and maintains screen state.
    /// Shared with TuiClient via Arc.
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

    /// Current connection URL for QR code display.
    pub(super) connection_url: Option<String>,

    /// Error message to display in Error mode.
    pub(super) error_message: Option<String>,

    /// Whether the QR image has been displayed (to avoid re-rendering every frame).
    pub(super) qr_image_displayed: bool,

    /// Agent creation progress (identifier, stage).
    pub(super) creating_agent: Option<(String, CreationStage)>,

    /// Issue or branch name for new agent creation (stored between modes).
    pub(super) pending_issue_or_branch: Option<String>,

    // === Agent State ===
    /// Cached agent list (updated via Hub broadcasts).
    pub(super) agents: Vec<AgentInfo>,

    // === Channels ===
    /// Command sender to Hub (generic client interface).
    pub(super) command_tx: HubCommandSender,

    /// Hub event receiver (broadcasts).
    hub_event_rx: broadcast::Receiver<HubEvent>,

    /// Full agent handle (for accessing both CLI and Server PTY).
    ///
    /// Stored to enable PTY view toggling between CLI and Server.
    /// This is TuiRunner-specific because TuiClient only stores a single PtyHandle.
    pub(super) agent_handle: Option<AgentHandle>,

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
            .field("selected_agent", &self.client.selected_agent())
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
    /// * `hub_handle` - Handle for Hub communication
    /// * `command_tx` - Sender for commands to Hub
    /// * `hub_event_rx` - Receiver for Hub broadcasts
    /// * `shutdown` - Shared shutdown flag
    /// * `terminal_dims` - Initial terminal dimensions (rows, cols)
    ///
    /// # Returns
    ///
    /// A new TuiRunner ready to run.
    pub fn new(
        terminal: Terminal<B>,
        hub_handle: HubHandle,
        command_tx: HubCommandSender,
        hub_event_rx: broadcast::Receiver<HubEvent>,
        shutdown: Arc<AtomicBool>,
        terminal_dims: (u16, u16),
    ) -> Self {
        let (rows, cols) = terminal_dims;
        let parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
        let vt100_parser = Arc::new(Mutex::new(parser));

        // Create TuiClient sharing the same vt100 parser
        let client = TuiClient::with_parser(hub_handle, Arc::clone(&vt100_parser), cols, rows);

        Self {
            client,
            vt100_parser,
            terminal,
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: String::new(),
            worktree_selected: 0,
            available_worktrees: Vec::new(),
            connection_url: None,
            error_message: None,
            qr_image_displayed: false,
            creating_agent: None,
            pending_issue_or_branch: None,
            agents: Vec::new(),
            command_tx,
            hub_event_rx,
            agent_handle: None,
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
        self.client.selected_agent()
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

        while !self.should_quit() {
            // 1. Handle keyboard/mouse input
            self.poll_input()?;

            if self.should_quit() {
                break;
            }

            // 2. Poll Hub events (broadcasts)
            self.poll_hub_events();

            // 3. Poll PTY events (if agent selected)
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
    fn poll_input(&mut self) -> Result<()> {
        if event::poll(Duration::from_millis(10))? {
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
            menu_count: constants::MENU_ITEMS.len(),
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

    /// Handle PTY input (send to connected agent).
    fn handle_pty_input(&mut self, data: &[u8]) {
        if let Err(e) = self.client.send_input(data) {
            log::error!("Failed to send input to PTY: {}", e);
        }
    }

    /// Handle resize event.
    fn handle_resize(&mut self, rows: u16, cols: u16) {
        self.terminal_dims = (rows, cols);
        // Update client dimensions (handles parser resize and PTY notification)
        self.client.update_dims(cols, rows);
    }

    /// Poll Hub broadcast events.
    fn poll_hub_events(&mut self) {
        // Process up to 100 events per tick
        for _ in 0..100 {
            match self.hub_event_rx.try_recv() {
                Ok(event) => self.handle_hub_event(event),
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    log::warn!("TUI lagged {} hub events", n);
                    continue;
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    log::info!("Hub event channel closed, quitting");
                    self.quit = true;
                    break;
                }
            }
        }
    }

    /// Poll PTY events and feed to parser.
    fn poll_pty_events(&mut self) {
        // Process up to 100 events per tick
        for _ in 0..100 {
            match self.client.poll_pty_events() {
                Ok(Some(PtyEvent::Output(data))) => {
                    self.client.on_output(&data);
                }
                Ok(Some(PtyEvent::Resized { rows, cols })) => {
                    log::debug!("PTY resized to {}x{}", cols, rows);
                    self.client.on_resized(rows, cols);
                }
                Ok(Some(PtyEvent::ProcessExited { exit_code })) => {
                    log::info!("PTY process exited with code {:?}", exit_code);
                    self.client.on_process_exit(exit_code);
                }
                Ok(Some(PtyEvent::OwnerChanged { new_owner })) => {
                    log::debug!("PTY owner changed to {:?}", new_owner);
                    self.client.on_owner_changed(new_owner);
                }
                Ok(None) => break,
                Err(broadcast::error::RecvError::Closed) => {
                    log::debug!("PTY channel closed");
                    self.client.disconnect_from_pty();
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    log::warn!("TUI lagged {} PTY events", n);
                    continue;
                }
            }
        }
    }

    /// Render the TUI.
    fn render(&mut self) -> Result<()> {
        use super::render::{render, AgentRenderInfo, RenderContext};
        use crate::tunnel::TunnelStatus;

        // Build agent render info from cached agents
        let agent_render_info: Vec<AgentRenderInfo> = self
            .agents
            .iter()
            .map(|info| AgentRenderInfo {
                key: info.id.clone(),
                repo: info.repo.clone().unwrap_or_default(),
                issue_number: info.issue_number.map(|n| n as u32),
                branch_name: info.branch_name.clone().unwrap_or_default(),
                tunnel_port: info.tunnel_port,
                server_running: info.server_running.unwrap_or(false),
                has_server_pty: info.has_server_pty.unwrap_or(false),
            })
            .collect();

        // Calculate selected agent index
        let selected_agent_index = self
            .client
            .selected_agent()
            .and_then(|key| self.agents.iter().position(|a| a.id == key))
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

        // Build render context from TuiRunner state
        let ctx = RenderContext {
            // UI State
            mode: self.mode,
            menu_selected: self.menu_selected,
            input_buffer: &self.input_buffer,
            worktree_selected: self.worktree_selected,
            available_worktrees: &self.available_worktrees,
            error_message: self.error_message.as_deref(),
            qr_image_displayed: self.qr_image_displayed,
            creating_agent: creating_agent_ref,
            connection_url: self.connection_url.as_deref(),
            bundle_used: false, // TuiRunner doesn't track this - would need from Hub

            // Agent State
            agent_ids: &[], // Not needed for rendering
            agents: &agent_render_info,
            selected_agent_index,

            // Terminal State - use TuiRunner's local parser
            active_parser: Some(self.parser_handle()),
            active_pty_view: self.client.active_pty_view(),
            scroll_offset,
            is_scrolled,

            // Status Indicators - TuiRunner doesn't track these, use defaults
            polling_enabled: true,
            seconds_since_poll: 0,
            poll_interval: 10,
            tunnel_status: TunnelStatus::Disconnected,
            vpn_status: None,
        };

        // Render and handle QR image state
        let result = render(&mut self.terminal, &ctx, None)?;

        // Update qr_image_displayed if we wrote one
        if result.qr_image_written {
            self.qr_image_displayed = true;
        }

        Ok(())
    }

    /// Set the connection URL (called from Hub).
    pub fn set_connection_url(&mut self, url: Option<String>) {
        self.connection_url = url;
        self.qr_image_displayed = false;
    }

    /// Set available worktrees (called from Hub).
    pub fn set_available_worktrees(&mut self, worktrees: Vec<(String, String)>) {
        self.available_worktrees = worktrees;
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

    /// Update agent list cache.
    pub fn update_agents(&mut self, agents: Vec<AgentInfo>) {
        self.agents = agents;
    }

    /// Set creation progress indicator.
    pub fn set_creating_agent(&mut self, identifier: Option<(String, CreationStage)>) {
        self.creating_agent = identifier;
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

    // Create TuiRunner with Hub's channel infrastructure
    let hub_handle = hub.handle();
    let command_tx = hub.command_sender();
    let hub_event_rx = hub.subscribe_events();
    let shutdown = Arc::new(AtomicBool::new(false));
    let tui_shutdown = Arc::clone(&shutdown);

    let mut tui_runner = TuiRunner::new(
        terminal,
        hub_handle,
        command_tx,
        hub_event_rx,
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

    // Main thread: Hub tick loop for non-TUI operations
    while !hub.quit && !shutdown_flag.load(Ordering::SeqCst) {
        // 1. Process commands from TuiRunner and other clients
        // (This is already called in tick(), but we call it here too for responsiveness)
        hub.process_commands();

        // Check quit after command processing (TuiRunner may have sent Quit)
        if hub.quit {
            break;
        }

        // 2. Poll and handle browser events (HubRelay - hub-level commands)
        browser::poll_events_headless(hub)?;

        // 3. Poll pending agents and progress events
        hub.poll_pending_agents();
        hub.poll_progress_events();

        // 4. Periodic tasks (polling, heartbeat, notifications, command processing)
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
    //! # Blocking Calls
    //!
    //! Some menu selections call blocking Hub operations (e.g., `list_worktrees_blocking`).
    //! For flows requiring these, we use `MockHubResponder` which spawns a thread to
    //! respond to commands. Tests that bypass this are explicitly documented.
    //!
    //! # M-DESIGN-FOR-AI Compliance
    //!
    //! Tests follow MS Rust guidelines with canonical documentation format.

    use super::*;
    use crate::hub::agent_handle::PtyHandle;
    use crate::hub::{CreateAgentRequest, DeleteAgentRequest, HubAction, HubCommand};
    use crate::tui::actions::TuiAction;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use tokio::sync::mpsc;

    // =========================================================================
    // Test Infrastructure
    // =========================================================================

    /// Creates a `TuiRunner` with a `TestBackend` for unit testing.
    ///
    /// Returns the runner and command receiver. The receiver allows verifying
    /// what commands were sent without an actual Hub.
    ///
    /// # Note
    ///
    /// This setup does NOT respond to blocking calls like `list_worktrees_blocking`.
    /// Use `create_test_runner_with_mock_hub` for flows requiring Hub responses.
    fn create_test_runner() -> (TuiRunner<TestBackend>, mpsc::Receiver<HubCommand>) {
        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).expect("Failed to create test terminal");

        let (cmd_tx, cmd_rx) = mpsc::channel::<HubCommand>(16);
        let command_sender = HubCommandSender::new(cmd_tx);

        let (_hub_tx, hub_rx) = broadcast::channel::<HubEvent>(16);
        let shutdown = Arc::new(AtomicBool::new(false));

        let runner = TuiRunner::new(
            terminal,
            HubHandle::mock(),
            command_sender,
            hub_rx,
            shutdown,
            (24, 80), // rows, cols
        );

        (runner, cmd_rx)
    }

    /// Mock Hub responder configuration.
    ///
    /// Specifies what the mock Hub should respond with for various commands.
    #[derive(Default, Clone)]
    struct MockHubConfig {
        /// Worktrees to return for `ListWorktrees` command.
        worktrees: Vec<(String, String)>,
    }

    /// Creates a `TuiRunner` with a mock Hub that responds to blocking calls.
    ///
    /// The mock Hub runs in a background thread and responds to commands
    /// according to the provided configuration.
    ///
    /// # Returns
    ///
    /// - `TuiRunner` ready for testing
    /// - `mpsc::Receiver` for inspecting commands (after mock has handled them)
    /// - `Arc<AtomicBool>` to signal shutdown to the mock thread
    fn create_test_runner_with_mock_hub(
        config: MockHubConfig,
    ) -> (
        TuiRunner<TestBackend>,
        mpsc::Receiver<HubCommand>,
        Arc<AtomicBool>,
    ) {
        let backend = TestBackend::new(80, 24);
        let terminal = Terminal::new(backend).expect("Failed to create test terminal");

        // Use a larger buffer to handle both mock responses and test verification
        let (cmd_tx, mut mock_rx) = mpsc::channel::<HubCommand>(32);
        let (passthrough_tx, passthrough_rx) = mpsc::channel::<HubCommand>(32);
        let command_sender = HubCommandSender::new(cmd_tx);

        let (_hub_tx, hub_rx) = broadcast::channel::<HubEvent>(16);
        let shutdown = Arc::new(AtomicBool::new(false));
        let mock_shutdown = Arc::clone(&shutdown);

        // Spawn mock Hub responder thread
        thread::spawn(move || {
            while !mock_shutdown.load(Ordering::Relaxed) {
                match mock_rx.try_recv() {
                    Ok(cmd) => {
                        // Handle the command - match by value to consume oneshot senders
                        match cmd {
                            HubCommand::ListWorktrees { response_tx } => {
                                let _ = response_tx.send(config.worktrees.clone());
                                // Create a placeholder to pass through for verification
                                let (placeholder_tx, _) = tokio::sync::oneshot::channel();
                                let _ = passthrough_tx.blocking_send(HubCommand::ListWorktrees {
                                    response_tx: placeholder_tx,
                                });
                            }
                            HubCommand::CreateAgent {
                                response_tx,
                                request,
                            } => {
                                // Return success with mock AgentInfo
                                let info = AgentInfo {
                                    id: format!("mock-agent-{}", request.issue_or_branch),
                                    repo: None,
                                    issue_number: None,
                                    branch_name: Some(request.issue_or_branch.clone()),
                                    name: None,
                                    status: Some("Running".to_string()),
                                    tunnel_port: None,
                                    server_running: None,
                                    has_server_pty: None,
                                    active_pty_view: None,
                                    scroll_offset: None,
                                    hub_identifier: None,
                                };
                                let _ = response_tx.send(Ok(info));
                                // Create a placeholder to pass through for verification
                                let (placeholder_tx, _) = tokio::sync::oneshot::channel();
                                let _ = passthrough_tx.blocking_send(HubCommand::CreateAgent {
                                    request,
                                    response_tx: placeholder_tx,
                                });
                            }
                            HubCommand::DeleteAgent {
                                response_tx,
                                request,
                            } => {
                                let _ = response_tx.send(Ok(()));
                                let (placeholder_tx, _) = tokio::sync::oneshot::channel();
                                let _ = passthrough_tx.blocking_send(HubCommand::DeleteAgent {
                                    request,
                                    response_tx: placeholder_tx,
                                });
                            }
                            other => {
                                // Pass through unhandled commands
                                let _ = passthrough_tx.blocking_send(other);
                            }
                        }
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
        });

        let runner = TuiRunner::new(
            terminal,
            HubHandle::mock(),
            command_sender,
            hub_rx,
            Arc::clone(&shutdown),
            (24, 80),
        );

        (runner, passthrough_rx, shutdown)
    }

    /// Builds an `InputContext` from the current runner state.
    fn runner_input_context(runner: &TuiRunner<TestBackend>) -> InputContext {
        InputContext {
            terminal_rows: runner.terminal_dims.0,
            menu_selected: runner.menu_selected,
            menu_count: constants::MENU_ITEMS.len(),
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
            has_server_pty: false,
            active_pty: PtyView::Cli,
            polling_enabled: true,
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
        assert_eq!(
            get_action_for_selection(&menu, 2),
            Some(MenuAction::TogglePolling)
        );

        // Menu with agent selected - should have Agent and Hub sections
        let ctx_with_agent = MenuContext {
            has_agent: true,
            has_server_pty: false,
            active_pty: PtyView::Cli,
            polling_enabled: true,
        };
        let menu = build_menu(&ctx_with_agent);

        // First selectable should be Close Agent (after Agent header)
        assert_eq!(
            get_action_for_selection(&menu, 0),
            Some(MenuAction::CloseAgent)
        );
        // Then Hub items
        assert_eq!(
            get_action_for_selection(&menu, 1),
            Some(MenuAction::NewAgent)
        );

        // Menu with agent and server PTY - should have Toggle PTY View
        let ctx_with_server = MenuContext {
            has_agent: true,
            has_server_pty: true,
            active_pty: PtyView::Cli,
            polling_enabled: true,
        };
        let menu = build_menu(&ctx_with_server);

        // First should be Toggle PTY View, then Close Agent
        assert_eq!(
            get_action_for_selection(&menu, 0),
            Some(MenuAction::TogglePtyView)
        );
        assert_eq!(
            get_action_for_selection(&menu, 1),
            Some(MenuAction::CloseAgent)
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
    #[test]
    fn test_pty_output_feeds_parser() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Create PTY channel and connect
        let (event_tx, _) = broadcast::channel::<PtyEvent>(16);
        let (cmd_tx, _) = mpsc::channel(16);
        let pty_handle = PtyHandle::new(event_tx.clone(), cmd_tx);
        runner.client.connect_to_pty("test-agent", pty_handle);

        // Send output
        event_tx
            .send(PtyEvent::output(b"Hello, World!".to_vec()))
            .unwrap();

        // Poll and verify
        runner.poll_pty_events();

        let parser = runner.vt100_parser.lock().unwrap();
        let contents = parser.screen().contents();
        assert!(
            contents.contains("Hello, World!"),
            "Parser should contain output, got: {}",
            contents.trim()
        );
    }

    /// Verifies multiple PTY outputs are processed in sequence.
    #[test]
    fn test_pty_multiple_outputs_processed() {
        let (mut runner, _cmd_rx) = create_test_runner();

        let (event_tx, _) = broadcast::channel::<PtyEvent>(16);
        let (cmd_tx, _) = mpsc::channel(16);
        let pty_handle = PtyHandle::new(event_tx.clone(), cmd_tx);
        runner.client.connect_to_pty("test-agent", pty_handle);

        // Send multiple outputs
        event_tx
            .send(PtyEvent::output(b"Line 1\r\n".to_vec()))
            .unwrap();
        event_tx
            .send(PtyEvent::output(b"Line 2\r\n".to_vec()))
            .unwrap();
        event_tx
            .send(PtyEvent::output(b"Line 3\r\n".to_vec()))
            .unwrap();

        runner.poll_pty_events();

        let parser = runner.vt100_parser.lock().unwrap();
        let contents = parser.screen().contents();
        assert!(contents.contains("Line 1"));
        assert!(contents.contains("Line 2"));
        assert!(contents.contains("Line 3"));
    }

    /// Verifies `poll_pty_events()` is safe without a connected PTY.
    #[test]
    fn test_pty_poll_without_connection_is_safe() {
        let (mut runner, _cmd_rx) = create_test_runner();

        assert!(!runner.client.is_pty_connected());
        runner.poll_pty_events(); // Should not panic

        let parser = runner.vt100_parser.lock().unwrap();
        assert!(parser.screen().contents().trim().is_empty());
    }

    /// Verifies polling an empty channel does not block.
    #[test]
    fn test_pty_poll_empty_channel_nonblocking() {
        let (mut runner, _cmd_rx) = create_test_runner();

        let (event_tx, _) = broadcast::channel::<PtyEvent>(16);
        let (cmd_tx, _) = mpsc::channel(16);
        let pty_handle = PtyHandle::new(event_tx, cmd_tx);
        runner.client.connect_to_pty("test-agent", pty_handle);

        // No events sent
        runner.poll_pty_events(); // Should return immediately

        let parser = runner.vt100_parser.lock().unwrap();
        assert!(parser.screen().contents().trim().is_empty());
    }

    /// Verifies disconnect is handled gracefully.
    #[test]
    fn test_pty_disconnect_handled_gracefully() {
        let (mut runner, _cmd_rx) = create_test_runner();

        let (event_tx, _) = broadcast::channel::<PtyEvent>(16);
        let (cmd_tx, _) = mpsc::channel(16);
        let pty_handle = PtyHandle::new(event_tx, cmd_tx);
        runner.client.connect_to_pty("test-agent", pty_handle);

        runner.client.disconnect_from_pty();
        assert!(!runner.client.is_pty_connected());

        runner.poll_pty_events(); // Should not panic
        assert!(!runner.client.is_pty_connected());
    }

    /// Verifies non-output events do not affect parser content.
    #[test]
    fn test_pty_non_output_events_ignored() {
        let (mut runner, _cmd_rx) = create_test_runner();

        let (event_tx, _) = broadcast::channel::<PtyEvent>(16);
        let (cmd_tx, _) = mpsc::channel(16);
        let pty_handle = PtyHandle::new(event_tx.clone(), cmd_tx);
        runner.client.connect_to_pty("test-agent", pty_handle);

        // Send non-output events
        event_tx.send(PtyEvent::resized(30, 100)).unwrap();
        event_tx.send(PtyEvent::process_exited(Some(0))).unwrap();
        event_tx.send(PtyEvent::owner_changed(None)).unwrap();

        runner.poll_pty_events();

        let parser = runner.vt100_parser.lock().unwrap();
        assert!(
            parser.screen().contents().trim().is_empty(),
            "Non-output events should not add content"
        );
    }

    /// Verifies mixed output and non-output events are handled correctly.
    #[test]
    fn test_pty_mixed_events_only_output_processed() {
        let (mut runner, _cmd_rx) = create_test_runner();

        let (event_tx, _) = broadcast::channel::<PtyEvent>(16);
        let (cmd_tx, _) = mpsc::channel(16);
        let pty_handle = PtyHandle::new(event_tx.clone(), cmd_tx);
        runner.client.connect_to_pty("test-agent", pty_handle);

        // Mixed events
        event_tx.send(PtyEvent::output(b"Start".to_vec())).unwrap();
        event_tx.send(PtyEvent::resized(30, 100)).unwrap();
        event_tx
            .send(PtyEvent::output(b" Middle".to_vec()))
            .unwrap();
        event_tx.send(PtyEvent::process_exited(Some(0))).unwrap();
        event_tx.send(PtyEvent::output(b" End".to_vec())).unwrap();

        runner.poll_pty_events();

        let parser = runner.vt100_parser.lock().unwrap();
        let contents = parser.screen().contents();
        assert!(contents.contains("Start Middle End"));
    }

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

        // Navigate down
        process_key(&mut runner, make_key(KeyCode::Down));
        assert_eq!(runner.menu_selected, 1);

        process_key(&mut runner, make_key(KeyCode::Down));
        assert_eq!(runner.menu_selected, 2);

        // Navigate up
        process_key(&mut runner, make_key(KeyCode::Up));
        assert_eq!(runner.menu_selected, 1);

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
    #[test]
    fn test_e2e_connection_code_full_flow() {
        use crate::tui::menu::MenuAction;

        let (mut runner, mut cmd_rx) = create_test_runner();

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

        // 4. Press 'r' to regenerate
        process_key(&mut runner, make_key(KeyCode::Char('r')));

        // Verify regenerate command sent
        let cmd = cmd_rx.try_recv().expect("Expected regenerate command");
        assert!(matches!(
            cmd,
            HubCommand::DispatchAction(HubAction::RegenerateConnectionCode)
        ));

        // 5. Close with Escape
        process_key(&mut runner, make_key(KeyCode::Esc));
        assert_eq!(runner.mode(), AppMode::Normal);
    }

    // =========================================================================
    // E2E Agent Deletion Flow Tests
    // =========================================================================

    /// Verifies close agent flow: menu -> confirm -> Y (keep worktree).
    ///
    /// Uses `find_menu_action_index` to dynamically locate the Close Agent action.
    #[test]
    fn test_e2e_close_agent_keep_worktree() {
        use crate::tui::menu::MenuAction;

        let (mut runner, mut cmd_rx) = create_test_runner();

        // Setup: Select an agent
        runner.client.set_selected_agent(Some("test-agent"));

        // 1. Open menu
        process_key(&mut runner, make_key_ctrl(KeyCode::Char('p')));

        // 2. Find and navigate to Close Agent using dynamic menu lookup
        let close_idx = find_menu_action_index(&runner, MenuAction::CloseAgent)
            .expect("CloseAgent should be in menu when agent is selected");
        navigate_to_menu_index(&mut runner, close_idx);
        assert_eq!(runner.menu_selected, close_idx);

        // 3. Select with Enter
        process_key(&mut runner, make_key(KeyCode::Enter));
        assert_eq!(runner.mode(), AppMode::CloseAgentConfirm);

        // 4. Confirm with 'y' (keep worktree)
        process_key(&mut runner, make_key(KeyCode::Char('y')));

        // Verify command
        let cmd = cmd_rx.try_recv().expect("Expected DeleteAgent command");
        match cmd {
            HubCommand::DeleteAgent { request, .. } => {
                assert_eq!(request.agent_id, "test-agent");
                assert!(!request.delete_worktree, "Y should keep worktree");
            }
            other => panic!("Expected DeleteAgent, got {:?}", other),
        }

        assert_eq!(runner.mode(), AppMode::Normal);
    }

    /// Verifies close agent flow: confirm -> D (delete worktree).
    #[test]
    fn test_e2e_close_agent_delete_worktree() {
        use crate::tui::menu::MenuAction;

        let (mut runner, mut cmd_rx) = create_test_runner();

        runner.client.set_selected_agent(Some("test-agent"));

        // Find the menu selection index for CloseAgent using dynamic lookup
        let close_idx = find_menu_action_index(&runner, MenuAction::CloseAgent)
            .expect("CloseAgent should be in menu when agent is selected");
        runner.handle_menu_select(close_idx);
        assert_eq!(runner.mode(), AppMode::CloseAgentConfirm);

        // Press 'd' to delete with worktree
        process_key(&mut runner, make_key(KeyCode::Char('d')));

        let cmd = cmd_rx.try_recv().expect("Expected DeleteAgent command");
        match cmd {
            HubCommand::DeleteAgent { request, .. } => {
                assert!(request.delete_worktree, "D should delete worktree");
            }
            other => panic!("Expected DeleteAgent, got {:?}", other),
        }
    }

    /// Verifies close agent cancel with Escape.
    #[test]
    fn test_e2e_close_agent_cancel_with_escape() {
        use crate::tui::menu::MenuAction;

        let (mut runner, mut cmd_rx) = create_test_runner();

        runner.client.set_selected_agent(Some("test-agent"));

        // Find the menu selection index for CloseAgent using dynamic lookup
        let close_idx = find_menu_action_index(&runner, MenuAction::CloseAgent)
            .expect("CloseAgent should be in menu when agent is selected");
        runner.handle_menu_select(close_idx);
        assert_eq!(runner.mode(), AppMode::CloseAgentConfirm);

        // Cancel with Escape
        process_key(&mut runner, make_key(KeyCode::Esc));

        assert_eq!(runner.mode(), AppMode::Normal);
        assert!(cmd_rx.try_recv().is_err(), "No command on cancel");
    }

    /// Verifies close agent is not available without selected agent.
    ///
    /// When no agent is selected, the CloseAgent menu item doesn't appear in the
    /// dynamic menu. This test verifies that the dynamic menu correctly omits
    /// CloseAgent when no agent is selected.
    #[test]
    fn test_e2e_close_agent_requires_selection() {
        use crate::tui::menu::MenuAction;

        let (mut runner, mut cmd_rx) = create_test_runner();

        assert!(runner.client.selected_agent().is_none());

        // CloseAgent should NOT be in the menu when no agent is selected
        let close_idx = find_menu_action_index(&runner, MenuAction::CloseAgent);
        assert!(
            close_idx.is_none(),
            "CloseAgent should not be in menu without selected agent"
        );

        // Attempting to select an invalid index is a no-op (falls through to Normal mode)
        runner.handle_menu_select(99); // Invalid index

        // Should stay in Normal mode
        assert_eq!(runner.mode(), AppMode::Normal);
        assert!(cmd_rx.try_recv().is_err());
    }

    // =========================================================================
    // E2E Agent Creation Flow Tests (with Mock Hub)
    // =========================================================================

    /// Verifies full agent creation flow: menu -> worktree select -> issue input -> prompt -> create.
    ///
    /// Uses `find_menu_action_index` to dynamically locate the New Agent action.
    #[test]
    fn test_e2e_new_agent_full_flow_with_mock_hub() {
        use crate::tui::menu::MenuAction;

        let config = MockHubConfig {
            worktrees: vec![("/path/worktree-1".to_string(), "feature-1".to_string())],
        };
        let (mut runner, mut cmd_rx, shutdown) = create_test_runner_with_mock_hub(config);

        // 1. Open menu and navigate to New Agent using dynamic lookup
        process_key(&mut runner, make_key_ctrl(KeyCode::Char('p')));

        let new_agent_idx = find_menu_action_index(&runner, MenuAction::NewAgent)
            .expect("NewAgent should be in menu");
        navigate_to_menu_index(&mut runner, new_agent_idx);
        assert_eq!(runner.menu_selected, new_agent_idx);

        process_key(&mut runner, make_key(KeyCode::Enter));

        // Small delay to let mock respond to ListWorktrees
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

        // Wait for mock to process
        thread::sleep(Duration::from_millis(10));

        // Verify CreateAgent command (skip ListWorktrees)
        let mut found_create = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            if let HubCommand::CreateAgent { request, .. } = cmd {
                assert_eq!(request.issue_or_branch, "issue-42");
                assert_eq!(request.prompt, Some("Fix bug".to_string()));
                assert!(request.from_worktree.is_none());
                found_create = true;
                break;
            }
        }
        assert!(found_create, "CreateAgent command should be sent");

        assert_eq!(runner.mode(), AppMode::Normal);

        // Cleanup
        shutdown.store(true, Ordering::Relaxed);
    }

    /// Verifies selecting an existing worktree skips prompt and creates agent immediately.
    ///
    /// Uses `find_menu_action_index` to dynamically locate the New Agent action.
    #[test]
    fn test_e2e_reopen_existing_worktree_with_mock_hub() {
        use crate::tui::menu::MenuAction;

        let config = MockHubConfig {
            worktrees: vec![
                ("/path/worktree-1".to_string(), "feature-branch".to_string()),
                ("/path/worktree-2".to_string(), "bugfix-branch".to_string()),
            ],
        };
        let (mut runner, mut cmd_rx, shutdown) = create_test_runner_with_mock_hub(config);

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

        // Should return to Normal immediately (no prompt for existing worktree)
        assert_eq!(runner.mode(), AppMode::Normal);

        // Verify CreateAgent with from_worktree
        let mut found_create = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            if let HubCommand::CreateAgent { request, .. } = cmd {
                assert_eq!(request.issue_or_branch, "feature-branch");
                assert_eq!(
                    request.from_worktree,
                    Some(std::path::PathBuf::from("/path/worktree-1"))
                );
                found_create = true;
                break;
            }
        }
        assert!(found_create, "CreateAgent command should be sent");

        shutdown.store(true, Ordering::Relaxed);
    }

    /// Verifies empty issue name is rejected.
    #[test]
    fn test_e2e_empty_issue_name_rejected() {
        let (mut runner, _cmd_rx) = create_test_runner();

        // Bypass to NewAgentCreateWorktree mode
        // (Cannot go through menu without mock Hub for list_worktrees_blocking)
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
    #[test]
    fn test_agent_navigation_empty_list() {
        let (mut runner, _cmd_rx) = create_test_runner();

        assert!(runner.agents.is_empty());

        // Should not panic
        runner.request_select_next();
        runner.request_select_previous();

        assert!(runner.client.selected_agent().is_none());
    }

    // =========================================================================
    // E2E PTY View Toggle Tests
    // =========================================================================

    /// Verifies PTY toggle without agent is no-op.
    #[test]
    fn test_pty_toggle_without_agent_is_noop() {
        use crate::agent::PtyView;

        let (mut runner, _cmd_rx) = create_test_runner();

        assert!(runner.client.selected_agent().is_none());
        assert!(runner.agent_handle.is_none());

        runner.handle_tui_action(TuiAction::TogglePtyView);

        // Should remain on CLI view
        assert_eq!(runner.client.active_pty_view(), PtyView::Cli);
    }

    /// Verifies PTY toggle without server PTY stays on CLI.
    #[test]
    fn test_pty_toggle_without_server_pty_stays_cli() {
        use crate::agent::PtyView;

        let (mut runner, _cmd_rx) = create_test_runner();

        // Create handle with only CLI PTY
        let (cli_event_tx, _) = broadcast::channel::<PtyEvent>(16);
        let (cli_cmd_tx, _) = mpsc::channel(16);

        let info = crate::relay::types::AgentInfo {
            id: "test-agent".to_string(),
            repo: None,
            issue_number: None,
            branch_name: None,
            name: None,
            status: None,
            tunnel_port: None,
            server_running: None,
            has_server_pty: None,
            active_pty_view: None,
            scroll_offset: None,
            hub_identifier: None,
        };

        let handle = AgentHandle::new(
            "test-agent",
            info,
            cli_event_tx,
            cli_cmd_tx,
            None, // No server PTY
            None,
        );

        runner.agent_handle = Some(handle);
        runner.client.set_selected_agent(Some("test-agent"));

        runner.handle_tui_action(TuiAction::TogglePtyView);

        assert_eq!(
            runner.client.active_pty_view(),
            PtyView::Cli,
            "Should stay on CLI without server PTY"
        );
    }

    /// Verifies PTY toggle with server PTY switches views.
    #[test]
    fn test_pty_toggle_with_server_pty_switches_views() {
        use crate::agent::PtyView;

        let (mut runner, _cmd_rx) = create_test_runner();

        // Create handle with both PTYs
        let (cli_event_tx, _) = broadcast::channel::<PtyEvent>(16);
        let (cli_cmd_tx, _) = mpsc::channel(16);
        let (server_event_tx, _) = broadcast::channel::<PtyEvent>(16);
        let (server_cmd_tx, _) = mpsc::channel(16);

        let info = crate::relay::types::AgentInfo {
            id: "test-agent".to_string(),
            repo: None,
            issue_number: None,
            branch_name: None,
            name: None,
            status: None,
            tunnel_port: None,
            server_running: Some(true),
            has_server_pty: Some(true),
            active_pty_view: None,
            scroll_offset: None,
            hub_identifier: None,
        };

        let handle = AgentHandle::new(
            "test-agent",
            info,
            cli_event_tx,
            cli_cmd_tx,
            Some(server_event_tx),
            Some(server_cmd_tx),
        );

        runner.agent_handle = Some(handle);
        runner.client.set_selected_agent(Some("test-agent"));

        // Initial state
        assert_eq!(runner.client.active_pty_view(), PtyView::Cli);

        // Toggle to Server
        runner.handle_tui_action(TuiAction::TogglePtyView);
        assert_eq!(runner.client.active_pty_view(), PtyView::Server);

        // Toggle back to CLI
        runner.handle_tui_action(TuiAction::TogglePtyView);
        assert_eq!(runner.client.active_pty_view(), PtyView::Cli);
    }

    // =========================================================================
    // Misc Action Tests
    // =========================================================================

    /// Verifies Quit action sets the quit flag and sends Quit command.
    #[test]
    fn test_quit_action() {
        let (mut runner, _cmd_rx) = create_test_runner();

        assert!(!runner.quit);

        runner.handle_tui_action(TuiAction::Quit);

        assert!(runner.quit, "Quit should set quit flag");
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

    /// Verifies Toggle Polling sends command and returns to Normal.
    #[test]
    fn test_toggle_polling_from_menu() {
        use crate::tui::menu::MenuAction;

        let (mut runner, mut cmd_rx) = create_test_runner();

        // Find the menu selection index for TogglePolling using dynamic lookup
        let toggle_idx = find_menu_action_index(&runner, MenuAction::TogglePolling)
            .expect("TogglePolling should always be in menu");
        runner.handle_menu_select(toggle_idx);

        let cmd = cmd_rx.try_recv().expect("Expected TogglePolling command");
        match cmd {
            HubCommand::DispatchAction(action) => {
                assert!(matches!(action, HubAction::TogglePolling));
            }
            other => panic!("Expected DispatchAction, got {:?}", other),
        }

        assert_eq!(runner.mode(), AppMode::Normal);
    }

    // =========================================================================
    // Edge Case Tests
    // =========================================================================

    /// Verifies confirm close is safe even without selected agent (edge case).
    ///
    /// This tests the guard in `handle_confirm_close_agent` that prevents
    /// sending a command if no agent is selected (should not happen in
    /// normal flow but tests robustness).
    #[test]
    fn test_confirm_close_without_agent_is_safe() {
        let (mut runner, mut cmd_rx) = create_test_runner();

        // Force mode without going through normal flow
        runner.mode = AppMode::CloseAgentConfirm;
        runner.client.set_selected_agent(None);

        runner.handle_tui_action(TuiAction::ConfirmCloseAgent);

        assert_eq!(runner.mode(), AppMode::Normal);
        assert!(cmd_rx.try_recv().is_err(), "No command without agent");
    }

    // =========================================================================
    // Error Handling Tests
    // =========================================================================

    /// Verifies that `list_worktrees_blocking` failure results in empty worktree list.
    ///
    /// When selecting "New Agent" from the menu, the TUI calls `list_worktrees_blocking`.
    /// If the Hub command channel is closed or times out, the error is logged and
    /// `available_worktrees` is set to an empty Vec, allowing graceful degradation.
    #[test]
    fn test_list_worktrees_failure_graceful_handling() {
        use crate::tui::menu::MenuAction;

        // Create runner but drop the receiver to simulate Hub being unavailable
        let (mut runner, cmd_rx) = create_test_runner();
        drop(cmd_rx); // Simulate Hub shutdown

        // Find the menu selection index for NewAgent using dynamic lookup
        let new_agent_idx = find_menu_action_index(&runner, MenuAction::NewAgent)
            .expect("NewAgent should always be in menu");

        // Select "New Agent" which calls list_worktrees_blocking
        runner.handle_menu_select(new_agent_idx);

        // Mode should still transition (the call fails but doesn't panic)
        assert_eq!(
            runner.mode(),
            AppMode::NewAgentSelectWorktree,
            "Should enter worktree selection even if list fails"
        );

        // Worktree list should be empty due to error
        assert!(
            runner.available_worktrees.is_empty(),
            "Worktrees should be empty on error"
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
    #[test]
    fn test_delete_agent_channel_closed_graceful() {
        let (mut runner, cmd_rx) = create_test_runner();
        runner.client.set_selected_agent(Some("test-agent"));
        runner.mode = AppMode::CloseAgentConfirm;
        drop(cmd_rx);

        // Confirm should attempt to send command but fail gracefully
        runner.handle_tui_action(TuiAction::ConfirmCloseAgent);

        // Should return to Normal despite error
        assert_eq!(runner.mode(), AppMode::Normal);
    }

    // Rust guideline compliant 2026-01
}
