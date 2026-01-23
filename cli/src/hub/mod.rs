//! Hub - Central orchestrator for agent management.
//!
//! The Hub is the core of botster-hub, owning all state and running the main
//! event loop. It follows the centralized state store pattern where TUI and
//! Relay adapters query the Hub for state rather than owning it themselves.
//!
//! # Architecture
//!
//! ```text
//!            ┌──────────────────────┐
//!            │        Hub           │
//!            │  - Owns all state    │
//!            │  - Runs event loop   │
//!            │  - Source of truth   │
//!            └──────────┬───────────┘
//!                       │
//!        ┌──────────────┼──────────────┐
//!        │              │              │
//!        ▼              ▼              ▼
//!      TUI           Server         Relay
//!   (renders)     (Rails API)    (Browser WS)
//! ```
//!
//! # Module Structure
//!
//! - `client_routing`: TUI selection helpers, client communication
//! - `server_comms`: Message polling, server registration, heartbeat
//! - `actions`: Hub action dispatch
//! - `lifecycle`: Agent spawn/close operations
//! - `polling`: Server polling utilities
//! - `registration`: Device and hub registration
//!
//! # Usage
//!
//! ```ignore
//! let hub = Hub::new(config)?;
//! hub.run()?;  // Starts event loop with TUI
//! // or
//! hub.run_headless()?;  // Starts event loop without TUI
//! ```

// Rust guideline compliant 2025-01

pub mod actions;
pub mod agent_handle;
mod client_routing;
pub mod commands;
pub mod events;
pub mod hub_handle;
pub mod lifecycle;
pub mod polling;
pub mod registration;
pub mod run;
mod server_comms;
pub mod state;
pub mod workers;

pub use crate::agents::AgentSpawnConfig;
pub use actions::{HubAction, ScrollDirection};
pub use agent_handle::AgentHandle;
pub use commands::{
    CreateAgentRequest, CreateAgentResult, DeleteAgentRequest, HubCommand, HubCommandSender,
};
pub use events::HubEvent;
pub use hub_handle::HubHandle;
pub use lifecycle::{close_agent, spawn_agent, SpawnResult};
pub use state::{HubState, SharedHubState};

use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Instant;

use reqwest::blocking::Client;

use sha2::{Digest, Sha256};

use crate::app::AppMode;
use crate::channel::{ActionCableChannel, Channel, ChannelConfig};
use crate::client::{ClientId, ClientRegistry, TuiClient};
use crate::config::Config;
use crate::device::Device;
use crate::git::WorktreeManager;
use crate::tunnel::TunnelManager;

/// Progress event during agent creation.
///
/// Sent from background thread to main loop to report creation progress.
#[derive(Debug, Clone)]
pub struct AgentProgressEvent {
    /// The client that requested the agent creation.
    pub client_id: ClientId,
    /// The branch or issue identifier being created.
    pub identifier: String,
    /// Current creation stage.
    pub stage: crate::relay::AgentCreationStage,
}

/// Result of a background agent creation task.
///
/// Sent from the background thread to the main loop when agent creation completes.
#[derive(Debug)]
pub struct PendingAgentResult {
    /// The client that requested the agent creation.
    pub client_id: ClientId,
    /// The result of the spawn operation.
    pub result: Result<SpawnResult, String>,
    /// The spawn config used (for error reporting).
    pub config: AgentSpawnConfig,
}

/// Generate a stable hub_identifier from a repo path.
///
/// Uses SHA256 hash of the absolute path to ensure the same repo
/// always gets the same hub_id, even across CLI restarts.
#[must_use]
pub fn hub_id_for_repo(repo_path: &std::path::Path) -> String {
    let canonical = repo_path
        .canonicalize()
        .unwrap_or_else(|_| repo_path.to_path_buf());

    let hash = Sha256::digest(canonical.to_string_lossy().as_bytes());

    // Use first 16 bytes as hex (32 chars) - enough uniqueness, shorter than UUID
    hash[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Central orchestrator for the botster-hub application.
///
/// The Hub owns all application state and coordinates between the TUI,
/// server polling, and browser relay components. It can run in either
/// TUI mode (with terminal rendering) or headless mode (for CI/daemon use).
pub struct Hub {
    // === Core State ===
    /// Core agent and worktree state (shared for thread-safe access).
    pub state: SharedHubState,
    /// Application configuration.
    pub config: Config,
    /// HTTP client for server communication.
    pub client: Client,
    /// Device identity for E2E encryption.
    pub device: Device,

    // === Runtime ===
    /// HTTP tunnel manager for dev server forwarding.
    pub tunnel_manager: Arc<TunnelManager>,
    /// Local identifier for this hub session (used for config directories).
    pub hub_identifier: String,
    /// Server-assigned ID for server communication (set after registration).
    pub botster_id: Option<String>,
    /// Async runtime for relay and tunnel operations.
    pub tokio_runtime: tokio::runtime::Runtime,

    // === Control Flags ===
    /// Whether the hub should quit.
    pub quit: bool,
    /// Whether server polling is enabled.
    pub polling_enabled: bool,

    // === Timing ===
    /// Last time we polled for messages.
    pub last_poll: Instant,
    /// Last time we sent a heartbeat.
    pub last_heartbeat: Instant,

    // === Terminal/TUI State ===
    /// Current terminal dimensions (rows, cols).
    pub terminal_dims: (u16, u16),
    /// Current application mode (Normal, Menu, etc.).
    pub mode: AppMode,
    /// Currently selected menu item.
    pub menu_selected: usize,
    /// Text input buffer.
    pub input_buffer: String,
    /// Currently selected worktree index.
    pub worktree_selected: usize,
    /// Current connection URL for clipboard copying.
    pub connection_url: Option<String>,
    /// Error message to display in Error mode.
    pub error_message: Option<String>,
    /// Whether the QR image has been displayed (to avoid re-rendering every frame).
    pub qr_image_displayed: bool,

    // === Browser Relay ===
    /// Browser connection state and communication.
    pub browser: crate::relay::BrowserState,

    // === Client Registry ===
    /// Registry of all connected clients (TUI, browsers).
    /// Each client can independently select and view different agents.
    pub clients: ClientRegistry,

    // === Background Task Channels ===
    /// Sender for pending agent creation results (cloned for each background task).
    pub pending_agent_tx: std_mpsc::Sender<PendingAgentResult>,
    /// Receiver for pending agent creation results (polled in main loop).
    pub pending_agent_rx: std_mpsc::Receiver<PendingAgentResult>,
    /// Sender for agent creation progress updates (cloned for each background task).
    pub progress_tx: std_mpsc::Sender<AgentProgressEvent>,
    /// Receiver for agent creation progress updates (polled in main loop).
    pub progress_rx: std_mpsc::Receiver<AgentProgressEvent>,

    // === TUI Creation Progress ===
    /// Current agent creation in progress for TUI display (identifier, stage).
    /// Cleared when agent is created or creation fails.
    pub creating_agent: Option<(String, crate::relay::AgentCreationStage)>,

    // === Background Workers ===
    /// Background worker for message polling (non-blocking).
    pub polling_worker: Option<workers::PollingWorker>,
    /// Background worker for heartbeat sending (non-blocking).
    pub heartbeat_worker: Option<workers::HeartbeatWorker>,
    /// Background worker for notification sending (non-blocking).
    pub notification_worker: Option<workers::NotificationWorker>,
    /// Last agent count sent to heartbeat worker (for change detection).
    last_heartbeat_agent_count: usize,

    // === Command Channel (Actor Pattern) ===
    /// Sender for Hub commands (cloned for each client).
    command_tx: tokio::sync::mpsc::Sender<HubCommand>,
    /// Receiver for Hub commands (owned by Hub, polled in event loop).
    command_rx: tokio::sync::mpsc::Receiver<HubCommand>,

    // === Event Broadcast ===
    /// Sender for Hub events (clients subscribe via `subscribe_events()`).
    event_tx: tokio::sync::broadcast::Sender<HubEvent>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("state", &self.state)
            .field("hub_identifier", &self.hub_identifier)
            .field("quit", &self.quit)
            .field("polling_enabled", &self.polling_enabled)
            .field("terminal_dims", &self.terminal_dims)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl Hub {
    /// Create a new Hub with the given configuration and terminal dimensions.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The tokio runtime cannot be created
    /// - The HTTP client cannot be created
    /// - Device identity cannot be loaded
    pub fn new(config: Config, terminal_dims: (u16, u16)) -> anyhow::Result<Self> {
        use std::sync::RwLock;
        use std::time::Duration;

        let state = Arc::new(RwLock::new(HubState::new(config.worktree_base.clone())));
        let tokio_runtime = tokio::runtime::Runtime::new()?;

        // Generate stable hub_identifier: env var > repo path hash
        // NO FALLBACKS - if we can't determine a stable identifier, fail explicitly
        let hub_identifier = if let Ok(id) = std::env::var("BOTSTER_HUB_ID") {
            log::info!("Hub identifier (from env): {}...", &id[..id.len().min(8)]);
            id
        } else {
            match WorktreeManager::detect_current_repo() {
                Ok((repo_path, _)) => {
                    let id = hub_id_for_repo(&repo_path);
                    log::info!("Hub identifier (from repo): {}...", &id[..8]);
                    id
                }
                Err(e) => {
                    // No silent fallback to random UUID - that causes session mismatches
                    // Either set BOTSTER_HUB_ID env var or run from a git repository
                    anyhow::bail!(
                        "Cannot determine hub identifier. Either:\n\
                         1. Run from within a git repository, or\n\
                         2. Set BOTSTER_HUB_ID environment variable\n\
                         \n\
                         Error: {e}"
                    );
                }
            }
        };

        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        // Load or create device identity for E2E encryption
        let device = Device::load_or_create()?;
        log::info!("Device fingerprint: {}", device.fingerprint);

        // Create tunnel manager
        let tunnel_manager = Arc::new(TunnelManager::new(
            hub_identifier.clone(),
            config.get_api_key().to_string(),
            config.server_url.clone(),
        ));

        // Initialize timestamps to past to trigger immediate poll/heartbeat on first tick
        let past = Instant::now() - std::time::Duration::from_secs(3600);

        // Create channel for background agent creation results
        let (pending_agent_tx, pending_agent_rx) = std_mpsc::channel();
        // Create channel for progress updates during agent creation
        let (progress_tx, progress_rx) = std_mpsc::channel();

        // Create command channel for actor pattern
        let (command_tx, command_rx) = tokio::sync::mpsc::channel(256);
        let command_sender = HubCommandSender::new(command_tx.clone());
        let hub_handle_for_tui = hub_handle::HubHandle::new(command_sender);

        // Create event broadcast channel for pub/sub
        let (event_tx, _) = tokio::sync::broadcast::channel(64);

        Ok(Self {
            state,
            config,
            client,
            device,
            tunnel_manager,
            hub_identifier,
            botster_id: None,
            tokio_runtime,
            quit: false,
            polling_enabled: true,
            last_poll: past,
            last_heartbeat: past,
            terminal_dims,
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: String::new(),
            worktree_selected: 0,
            connection_url: None,
            error_message: None,
            qr_image_displayed: false,
            browser: crate::relay::BrowserState::default(),
            clients: {
                let mut registry = ClientRegistry::new();
                registry.register(Box::new(TuiClient::new(hub_handle_for_tui)));
                registry
            },
            pending_agent_tx,
            pending_agent_rx,
            progress_tx,
            progress_rx,
            creating_agent: None,
            // Workers are started later via start_background_workers() after registration
            polling_worker: None,
            heartbeat_worker: None,
            notification_worker: None,
            last_heartbeat_agent_count: 0,
            command_tx,
            command_rx,
            event_tx,
        })
    }

    /// Start background workers for non-blocking network I/O.
    ///
    /// Call this after hub registration completes and `botster_id` is set.
    /// Workers handle polling, heartbeat, and notifications in background threads.
    pub fn start_background_workers(&mut self) {
        // Detect repo for workers
        let repo_name = match WorktreeManager::detect_current_repo() {
            Ok((_, name)) => name,
            Err(e) => {
                log::warn!("Cannot start background workers: not in a git repo: {e}");
                return;
            }
        };

        let worker_config = workers::WorkerConfig {
            server_url: self.config.server_url.clone(),
            api_key: self.config.get_api_key().to_string(),
            server_hub_id: self.server_hub_id().to_string(),
            poll_interval: self.config.poll_interval,
            repo_name,
            device_id: self.device.device_id,
        };

        // Start polling worker
        if self.polling_worker.is_none() {
            log::info!("Starting background polling worker");
            self.polling_worker = Some(workers::PollingWorker::new(worker_config.clone()));
        }

        // Start heartbeat worker
        if self.heartbeat_worker.is_none() {
            log::info!("Starting background heartbeat worker");
            self.heartbeat_worker = Some(workers::HeartbeatWorker::new(worker_config.clone()));
        }

        // Start notification worker
        if self.notification_worker.is_none() {
            log::info!("Starting background notification worker");
            self.notification_worker = Some(workers::NotificationWorker::new(worker_config));
        }
    }

    /// Shutdown all background workers gracefully.
    pub fn shutdown_background_workers(&mut self) {
        if let Some(worker) = self.polling_worker.take() {
            worker.shutdown();
        }
        if let Some(worker) = self.heartbeat_worker.take() {
            worker.shutdown();
        }
        if let Some(worker) = self.notification_worker.take() {
            worker.shutdown();
        }
    }

    /// Get a shared reference to the hub state for thread-safe access.
    ///
    /// Clients can clone this to access agent state without going through
    /// Hub commands. The RwLock allows multiple readers without blocking.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let shared_state = hub.shared_state();
    ///
    /// // In client code (possibly different thread):
    /// let state = shared_state.read().unwrap();
    /// let agents = state.get_agents_info();
    /// ```
    #[must_use]
    pub fn shared_state(&self) -> SharedHubState {
        Arc::clone(&self.state)
    }

    /// Get the current terminal dimensions.
    #[must_use]
    pub fn terminal_dims(&self) -> (u16, u16) {
        self.terminal_dims
    }

    /// Get the hub ID to use for server communication.
    ///
    /// Returns the server-assigned `botster_id` if available (after registration),
    /// otherwise falls back to local `hub_identifier`.
    #[must_use]
    pub fn server_hub_id(&self) -> &str {
        self.botster_id.as_deref().unwrap_or(&self.hub_identifier)
    }

    /// Set terminal dimensions.
    pub fn set_terminal_dims(&mut self, rows: u16, cols: u16) {
        self.terminal_dims = (rows, cols);
    }

    /// Show an error message to the user.
    ///
    /// Sets the error message and switches to Error mode.
    /// The user can dismiss the error with Esc/Enter/q.
    pub fn show_error(&mut self, message: impl Into<String>) {
        self.error_message = Some(message.into());
        self.mode = AppMode::Error;
    }

    /// Clear the error message and return to Normal mode.
    pub fn clear_error(&mut self) {
        self.error_message = None;
        self.mode = AppMode::Normal;
    }

    /// Connect an agent's channels (terminal and optionally preview).
    ///
    /// NOTE: PtySession.connect_channel was removed in the client refactor.
    /// Terminal relay is now handled through different architecture (PtySession broadcasts
    /// events, browser clients subscribe via relay module).
    ///
    /// This method now only connects the preview channel for HTTP proxying.
    ///
    /// # Arguments
    ///
    /// * `session_key` - The agent's session key
    /// * `agent_index` - Index of the agent for channel routing
    pub fn connect_agent_channels(&mut self, session_key: &str, agent_index: usize) {
        // Check if crypto service is available
        let Some(crypto_service) = self.browser.crypto_service.clone() else {
            log::debug!(
                "No crypto service available, deferring channel connection for {}",
                session_key
            );
            return;
        };

        let hub_id = self.server_hub_id().to_string();
        let server_url = self.config.server_url.clone();
        let api_key = self.config.get_api_key().to_string();

        // Get tunnel_port from agent
        let tunnel_port = self
            .state
            .read()
            .unwrap()
            .agents
            .get(session_key)
            .and_then(|a| a.tunnel_port);

        log::info!(
            "Connecting channels for agent {} (index={})",
            session_key,
            agent_index
        );

        // Terminal channels are now managed by PtySession broadcasting + browser relay.
        // See relay/browser.rs for PTY event routing to browsers.

        // Connect preview channel if tunnel_port is set (owned by agent)
        if let Some(port) = tunnel_port {
            let mut preview_channel =
                ActionCableChannel::encrypted(crypto_service, server_url, api_key);

            let preview_result = self.tokio_runtime.block_on(async {
                preview_channel
                    .connect(ChannelConfig {
                        channel_name: "PreviewChannel".into(),
                        hub_id,
                        agent_index: Some(agent_index),
                        pty_index: None, // Preview channel doesn't use PTY index
                        encrypt: true,
                        compression_threshold: Some(4096),
                    })
                    .await
            });

            if let Err(e) = preview_result {
                log::error!(
                    "Failed to connect preview channel for {}: {}",
                    session_key,
                    e
                );
                return;
            }

            // Store preview channel on agent
            if let Some(agent) = self.state.write().unwrap().agents.get_mut(session_key) {
                agent.preview_channel = Some(preview_channel);
                log::info!(
                    "Preview channel connected for {} (port {})",
                    session_key,
                    port
                );
            }
        }
    }

    /// Get the number of active agents.
    #[must_use]
    pub fn agent_count(&self) -> usize {
        self.state.read().unwrap().agent_count()
    }

    /// Check if the hub should quit.
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Request the hub to quit.
    pub fn request_quit(&mut self) {
        self.quit = true;
    }

    /// Handle a hub action.
    ///
    /// This is the central dispatch point for all actions. TUI input,
    /// browser events, and server messages all eventually become actions
    /// that are processed here.
    ///
    /// Delegates to `actions::dispatch()` for the actual processing.
    pub fn handle_action(&mut self, action: HubAction) {
        actions::dispatch(self, action);
    }

    /// Load available worktrees for the selection UI.
    ///
    /// Delegates to `HubState::load_available_worktrees()`.
    pub fn load_available_worktrees(&mut self) -> anyhow::Result<()> {
        self.state.write().unwrap().load_available_worktrees()
    }

    /// Toggle server polling on/off.
    pub fn toggle_polling(&mut self) {
        self.polling_enabled = !self.polling_enabled;
    }

    /// Check if polling is enabled.
    #[must_use]
    pub fn is_polling_enabled(&self) -> bool {
        self.polling_enabled
    }

    /// Get seconds since last poll.
    #[must_use]
    pub fn seconds_since_poll(&self) -> u64 {
        self.last_poll.elapsed().as_secs()
    }

    /// Mark that a poll just occurred.
    pub fn mark_poll(&mut self) {
        self.last_poll = Instant::now();
    }

    /// Get seconds since last heartbeat.
    #[must_use]
    pub fn seconds_since_heartbeat(&self) -> u64 {
        self.last_heartbeat.elapsed().as_secs()
    }

    /// Mark that a heartbeat was just sent.
    pub fn mark_heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
    }

    // === Event Loop ===

    /// Perform all initial setup steps.
    pub fn setup(&mut self) {
        self.register_device();
        self.register_hub_with_server();
        self.start_tunnel();
        self.connect_hub_relay();

        // Start background workers for non-blocking network I/O
        // Must be called after register_hub_with_server() sets botster_id
        self.start_background_workers();
    }

    /// Run the Hub event loop without TUI.
    ///
    /// For TUI mode, use `crate::tui::run_with_hub()` instead - the TUI
    /// module now owns TuiRunner instantiation.
    ///
    /// For headless mode, use `hub::run::run_headless_loop()`.
    pub fn run_headless(
        &mut self,
        shutdown_flag: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        run::run_headless_loop(self, shutdown_flag)
    }

    /// Send shutdown notification to server and cleanup resources.
    pub fn shutdown(&mut self) {
        // Shutdown background workers first (allows pending notifications to drain)
        self.shutdown_background_workers();

        // Notify server of shutdown
        registration::shutdown(
            &self.client,
            &self.config.server_url,
            self.server_hub_id(),
            self.config.get_api_key(),
        );
    }

    // === Command Channel (Actor Pattern) ===

    /// Get a command sender for clients to communicate with the Hub.
    ///
    /// Clients use this to send commands (create agent, delete agent, etc.)
    /// to the Hub. The Hub processes commands in its event loop via
    /// `process_commands()`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let sender = hub.command_sender();
    /// // Pass to TuiRunner or other clients
    /// let runner = TuiRunner::new(terminal, sender, ...);
    /// ```
    #[must_use]
    pub fn command_sender(&self) -> HubCommandSender {
        HubCommandSender::new(self.command_tx.clone())
    }

    /// Get a `HubHandle` for thread-safe client communication.
    ///
    /// `HubHandle` provides a simplified, blocking API for clients running
    /// in their own threads. It wraps the command channel and provides
    /// convenient methods for querying agents and sending commands.
    ///
    /// # Thread Safety
    ///
    /// The returned handle is `Clone + Send + Sync` and can be freely
    /// passed to other threads.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let handle = hub.handle();
    ///
    /// // Pass to client thread
    /// std::thread::spawn(move || {
    ///     let agents = handle.get_agents();
    ///     if let Some(agent) = handle.get_agent(0) {
    ///         // Use agent handle...
    ///     }
    /// });
    /// ```
    #[must_use]
    pub fn handle(&self) -> hub_handle::HubHandle {
        hub_handle::HubHandle::new(self.command_sender())
    }

    /// Subscribe to Hub events.
    ///
    /// Returns a receiver that will receive all Hub events:
    /// - `AgentCreated` - New agent was created
    /// - `AgentDeleted` - Agent was deleted
    /// - `AgentStatusChanged` - Agent status changed
    /// - `Shutdown` - Hub is shutting down
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut rx = hub.subscribe_events();
    /// tokio::spawn(async move {
    ///     while let Ok(event) = rx.recv().await {
    ///         match event {
    ///             HubEvent::AgentCreated { agent_id, .. } => {
    ///                 println!("Agent created: {}", agent_id);
    ///             }
    ///             // ...
    ///         }
    ///     }
    /// });
    /// ```
    #[must_use]
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<HubEvent> {
        self.event_tx.subscribe()
    }

    /// Broadcast an event to all subscribers.
    ///
    /// Events are sent to all clients that have called `subscribe_events()`.
    /// If no subscribers exist, the event is silently dropped.
    ///
    /// # Example
    ///
    /// ```ignore
    /// hub.broadcast(HubEvent::agent_created("agent-123", info));
    /// ```
    pub fn broadcast(&self, event: HubEvent) {
        // Ignore send errors - they just mean no subscribers exist
        let _ = self.event_tx.send(event);
    }

    /// Process pending commands from clients.
    ///
    /// Call this in the event loop to handle commands sent via `command_sender()`.
    /// Non-blocking - processes all available commands without waiting.
    ///
    /// Returns the number of commands processed.
    pub fn process_commands(&mut self) -> usize {
        let mut processed = 0;

        while let Ok(cmd) = self.command_rx.try_recv() {
            self.handle_command(cmd);
            processed += 1;
        }

        processed
    }

    /// Handle a single Hub command.
    fn handle_command(&mut self, cmd: HubCommand) {
        match cmd {
            HubCommand::CreateAgent {
                request,
                response_tx,
            } => {
                // Agent creation is async - dispatch to background worker
                // For now, respond that the request was received
                log::info!(
                    "Received CreateAgent command for: {}",
                    request.issue_or_branch
                );

                // Convert from commands::CreateAgentRequest to client::CreateAgentRequest
                let client_request = crate::client::CreateAgentRequest {
                    issue_or_branch: request.issue_or_branch,
                    prompt: request.prompt,
                    from_worktree: request.from_worktree,
                };

                // Dispatch to background creation
                let action = HubAction::CreateAgentForClient {
                    client_id: ClientId::Tui, // TODO: Track requesting client
                    request: client_request,
                };
                self.handle_action(action);

                // Can't return result immediately for async operation
                // Client will be notified via HubEvent::AgentCreated
                drop(response_tx);
            }

            HubCommand::DeleteAgent {
                request,
                response_tx,
            } => {
                log::info!("Received DeleteAgent command for: {}", request.agent_id);

                // Convert from commands::DeleteAgentRequest to client::DeleteAgentRequest
                let client_request = crate::client::DeleteAgentRequest {
                    agent_id: request.agent_id,
                    delete_worktree: request.delete_worktree,
                };

                let action = HubAction::DeleteAgentForClient {
                    client_id: ClientId::Tui, // TODO: Track requesting client
                    request: client_request,
                };
                self.handle_action(action);
                let _ = response_tx.send(Ok(()));
            }

            HubCommand::ListAgents { response_tx } => {
                let agents = self.state.read().unwrap().get_agents_info();
                let _ = response_tx.send(agents);
            }

            HubCommand::GetAgent {
                agent_id,
                response_tx,
            } => {
                let result = self
                    .state
                    .read()
                    .unwrap()
                    .get_agent_handle_by_id(&agent_id)
                    .ok_or_else(|| format!("Agent not found: {}", agent_id));
                let _ = response_tx.send(result);
            }

            HubCommand::GetAgentByIndex { index, response_tx } => {
                let handle = self.state.read().unwrap().get_agent_handle(index);
                let _ = response_tx.send(handle);
            }

            HubCommand::Quit => {
                log::info!("Received Quit command");
                self.quit = true;
                self.broadcast(HubEvent::shutdown());
            }

            HubCommand::DispatchAction(action) => {
                self.handle_action(action);
            }

            HubCommand::ListWorktrees { response_tx } => {
                // Reload worktrees and return the list
                if let Err(e) = self.load_available_worktrees() {
                    log::error!("Failed to load worktrees: {}", e);
                }
                let worktrees = self.state.read().unwrap().available_worktrees.clone();
                let _ = response_tx.send(worktrees);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config() -> Config {
        Config {
            server_url: "http://localhost:3000".to_string(),
            token: "btstr_test-key".to_string(),
            poll_interval: 10,
            agent_timeout: 300,
            max_sessions: 10,
            worktree_base: PathBuf::from("/tmp/test-worktrees"),
        }
    }

    // Default terminal dimensions for tests
    const TEST_DIMS: (u16, u16) = (24, 80);

    #[test]
    fn test_hub_creation() {
        let config = test_config();
        let hub = Hub::new(config, TEST_DIMS).unwrap();

        assert!(!hub.should_quit());
        assert!(hub.is_polling_enabled());
        assert_eq!(hub.agent_count(), 0);
    }

    #[test]
    fn test_hub_quit() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        assert!(!hub.should_quit());
        hub.request_quit();
        assert!(hub.should_quit());
    }

    #[test]
    fn test_hub_toggle_polling() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        assert!(hub.is_polling_enabled());
        hub.toggle_polling();
        assert!(!hub.is_polling_enabled());
        hub.toggle_polling();
        assert!(hub.is_polling_enabled());
    }

    #[test]
    fn test_hub_terminal_dims() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        assert_eq!(hub.terminal_dims(), TEST_DIMS);
        hub.set_terminal_dims(40, 120);
        assert_eq!(hub.terminal_dims(), (40, 120));
    }

    #[test]
    fn test_handle_action_quit() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        hub.handle_action(HubAction::Quit);
        assert!(hub.should_quit());
    }

    #[test]
    fn test_handle_action_toggle_polling() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        assert!(hub.is_polling_enabled());
        hub.handle_action(HubAction::TogglePolling);
        assert!(!hub.is_polling_enabled());
    }

    #[test]
    fn test_handle_action_resize() {
        use crate::client::ClientId;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        hub.handle_action(HubAction::ResizeForClient {
            client_id: ClientId::Tui,
            cols: 150,
            rows: 50,
        });
        assert_eq!(hub.terminal_dims(), (50, 150));
    }

    // === Phase 2A: TuiClient is source of truth for TUI selection ===
    //
    // These tests verify that:
    // 1. TuiClient owns TUI's selected_agent state
    // 2. HubState.selected is NOT used for TUI selection
    // 3. SelectAgentForClient updates TuiClient, not HubState

    #[test]
    fn test_tui_selection_comes_from_tui_client() {
        use crate::client::ClientId;

        let config = test_config();
        let hub = Hub::new(config, TEST_DIMS).unwrap();

        // TUI client should be registered
        assert!(
            hub.clients.get(&ClientId::Tui).is_some(),
            "TuiClient should be registered"
        );

        // Initial selection should be None (no agents)
        let tui_selection = hub.clients.selected_agent(&ClientId::Tui);
        assert!(
            tui_selection.is_none(),
            "TuiClient should have no selection initially"
        );
    }

    #[test]
    fn test_select_agent_for_client_updates_tui_client() {
        use crate::client::ClientId;
        use std::path::PathBuf;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Add a test agent directly to state
        let agent = crate::agent::Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(42),
            "botster-issue-42".to_string(),
            PathBuf::from("/tmp/test"),
        );
        let agent_key = "test-repo-42".to_string();
        hub.state
            .write()
            .unwrap()
            .add_agent(agent_key.clone(), agent);

        // Use SelectAgentForClient action (client-scoped)
        hub.handle_action(HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: agent_key.clone(),
        });

        // TuiClient should now have the agent selected (tracked by registry)
        let tui_selection = hub.clients.selected_agent(&ClientId::Tui);
        assert_eq!(
            tui_selection,
            Some(agent_key.as_str()),
            "TuiClient.selected_agent should be updated by SelectAgentForClient"
        );
    }

    #[test]
    fn test_tui_and_browser_can_have_different_selections() {
        use crate::client::{BrowserClient, ClientId};
        use std::path::PathBuf;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Add two test agents
        for i in 1..=2 {
            let agent = crate::agent::Agent::new(
                Uuid::new_v4(),
                "test/repo".to_string(),
                Some(i),
                format!("botster-issue-{}", i),
                PathBuf::from("/tmp/test"),
            );
            hub.state
                .write()
                .unwrap()
                .add_agent(format!("test-repo-{}", i), agent);
        }

        // Register a browser client
        let browser_client = BrowserClient::new(hub_handle::HubHandle::mock(), "browser-test-123".to_string());
        hub.clients.register(Box::new(browser_client));

        // TUI selects agent 1
        hub.handle_action(HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "test-repo-1".to_string(),
        });

        // Browser selects agent 2
        hub.handle_action(HubAction::SelectAgentForClient {
            client_id: ClientId::browser("browser-test-123"),
            agent_key: "test-repo-2".to_string(),
        });

        // Verify they have different selections (via registry)
        let tui_selection = hub.clients.selected_agent(&ClientId::Tui);
        let browser_selection = hub.clients.selected_agent(&ClientId::browser("browser-test-123"));

        assert_eq!(tui_selection, Some("test-repo-1"));
        assert_eq!(browser_selection, Some("test-repo-2"));
        assert_ne!(
            tui_selection, browser_selection,
            "TUI and browser should have independent selections"
        );
    }

    #[test]
    fn test_get_tui_selected_agent_uses_client_not_hub_state() {
        use crate::client::ClientId;
        use std::path::PathBuf;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Add two test agents
        for i in 1..=2 {
            let agent = crate::agent::Agent::new(
                Uuid::new_v4(),
                "test/repo".to_string(),
                Some(i),
                format!("botster-issue-{}", i),
                PathBuf::from("/tmp/test"),
            );
            hub.state
                .write()
                .unwrap()
                .add_agent(format!("test-repo-{}", i), agent);
        }

        // Use SelectAgentForClient to select agent 2 via TuiClient
        hub.handle_action(HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "test-repo-2".to_string(),
        });

        // The helper method should return the agent based on TuiClient selection
        let selected_key = hub.get_tui_selected_agent_key();
        assert_eq!(
            selected_key,
            Some("test-repo-2".to_string()),
            "get_tui_selected_agent_key should return TuiClient's selection"
        );
    }
}
