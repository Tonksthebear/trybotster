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

// Rust guideline compliant 2026-01-23

pub mod actions;
pub mod agent_handle;
mod client_routing;
pub mod commands;
pub mod events;
pub mod handle_cache;
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

    // === TUI Selection ===
    /// Currently selected agent for TUI.
    ///
    /// Tracked here so Hub methods like `get_tui_selected_agent_key()` can access it.
    /// Updated by `handle_select_agent_for_client()` when TUI selects an agent.
    pub tui_selected_agent: Option<String>,

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

    // === Handle Cache ===
    /// Thread-safe cache of agent handles for non-blocking client access.
    ///
    /// Updated by Hub when agents are created/deleted via `sync_handle_cache()`.
    /// `HubHandle.get_agent()` reads from this cache directly, allowing clients
    /// to access agent handles without blocking commands - safe from any thread.
    pub handle_cache: Arc<handle_cache::HandleCache>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("state", &self.state)
            .field("hub_identifier", &self.hub_identifier)
            .field("quit", &self.quit)
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

        // Create handle cache for thread-safe agent handle access
        let handle_cache = Arc::new(handle_cache::HandleCache::new());
        let hub_handle_for_tui = hub_handle::HubHandle::new(command_sender, Arc::clone(&handle_cache));

        // Create event broadcast channel for pub/sub
        let (event_tx, _) = tokio::sync::broadcast::channel(64);

        // Get runtime handle before moving tokio_runtime into struct
        let runtime_handle = tokio_runtime.handle().clone();

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
                // Create TuiClient with a dummy output channel.
                // In Hub context, TuiClient is used for client identity and dimensions,
                // not for receiving PTY output (TuiRunner has its own TuiClient for that).
                let (output_tx, _output_rx) = tokio::sync::mpsc::unbounded_channel();
                registry.register(Box::new(TuiClient::new(hub_handle_for_tui, output_tx, runtime_handle.clone())));
                registry
            },
            pending_agent_tx,
            pending_agent_rx,
            progress_tx,
            progress_rx,
            tui_selected_agent: None,
            creating_agent: None,
            // Workers are started later via start_background_workers() after registration
            polling_worker: None,
            heartbeat_worker: None,
            notification_worker: None,
            last_heartbeat_agent_count: 0,
            command_tx,
            command_rx,
            event_tx,
            handle_cache,
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

    /// Connect an agent's preview channel for HTTP proxying.
    ///
    /// Terminal relay is handled separately (PtySession broadcasts events,
    /// clients subscribe via relay module).
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

    /// Sync the handle cache with current state.
    ///
    /// Call this after agents are created or deleted to ensure the cache
    /// reflects the current state. The cache allows `HubHandle.get_agent()`
    /// to read directly without sending blocking commands.
    pub fn sync_handle_cache(&self) {
        let state = self.state.read().unwrap();
        let handles: Vec<AgentHandle> = (0..state.agent_count())
            .filter_map(|i| state.get_agent_handle(i))
            .collect();
        self.handle_cache.set_all(handles);
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
    /// Delegates to `HubState::load_available_worktrees()` and syncs the
    /// result to HandleCache for non-blocking client reads.
    pub fn load_available_worktrees(&mut self) -> anyhow::Result<()> {
        self.state.write().unwrap().load_available_worktrees()?;
        // Sync to HandleCache so clients can read without blocking commands
        let worktrees = self.state.read().unwrap().available_worktrees.clone();
        self.handle_cache.set_worktrees(worktrees);
        Ok(())
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

        // Seed shared state so clients have data immediately
        if let Err(e) = self.load_available_worktrees() {
            log::warn!("Failed to load initial worktrees: {}", e);
        }
        let connection_result = self.generate_connection_url();
        self.handle_cache.set_connection_url(connection_result);
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
        hub_handle::HubHandle::new(self.command_sender(), Arc::clone(&self.handle_cache))
    }

    /// Register TuiClient and return the output receiver.
    ///
    /// Creates a new output channel, updates the TuiClient with the sender,
    /// and returns the receiver for TuiRunner to consume.
    ///
    /// # Panics
    ///
    /// Panics if TuiClient is not registered (should always be present).
    pub fn register_tui_client(&mut self) -> tokio::sync::mpsc::UnboundedReceiver<crate::client::TuiOutput> {
        use crate::client::TuiOutput;

        let (output_tx, output_rx) = tokio::sync::mpsc::unbounded_channel::<TuiOutput>();

        // Replace the TuiClient with one that has the real output channel
        let hub_handle = self.handle();
        let runtime_handle = self.tokio_runtime.handle().clone();
        let tui_client = crate::client::TuiClient::new(hub_handle, output_tx, runtime_handle);

        // Re-register (replaces existing)
        self.clients.register(Box::new(tui_client));

        output_rx
    }

    /// Register TuiClient with output channel and request channel.
    ///
    /// Creates a TuiClient with output channel for PTY output and wires the
    /// request channel for TuiRunner -> TuiClient communication. TuiRunner
    /// sends `TuiRequest` messages through the request channel, TuiClient
    /// processes them (forwarding to Hub when needed).
    ///
    /// # Arguments
    ///
    /// * `request_rx` - Receiver for TuiRequest messages from TuiRunner
    ///
    /// # Returns
    ///
    /// Receiver for TuiOutput messages to TuiRunner.
    pub fn register_tui_client_with_request_channel(
        &mut self,
        request_rx: tokio::sync::mpsc::UnboundedReceiver<crate::client::TuiRequest>,
    ) -> tokio::sync::mpsc::UnboundedReceiver<crate::client::TuiOutput> {
        use crate::client::TuiOutput;

        let (output_tx, output_rx) = tokio::sync::mpsc::unbounded_channel::<TuiOutput>();

        // Create TuiClient with output channel
        let hub_handle = self.handle();
        let runtime_handle = self.tokio_runtime.handle().clone();
        let mut tui_client = crate::client::TuiClient::new(hub_handle, output_tx, runtime_handle);

        // Wire the request channel
        tui_client.set_request_receiver(request_rx);

        // Re-register (replaces existing)
        self.clients.register(Box::new(tui_client));

        output_rx
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
                // Reload worktrees and return the list (load_available_worktrees syncs cache)
                if let Err(e) = self.load_available_worktrees() {
                    log::error!("Failed to load worktrees: {}", e);
                }
                let worktrees = self.state.read().unwrap().available_worktrees.clone();
                let _ = response_tx.send(worktrees);
            }

            HubCommand::GetConnectionCode { response_tx } => {
                let result = self.generate_connection_url();
                // Cache for direct reads by clients on Hub thread
                self.handle_cache.set_connection_url(result.clone());
                let _ = response_tx.send(result);
            }

            HubCommand::RefreshConnectionCode { response_tx } => {
                // Request bundle regeneration from relay, then return new URL
                let result = self.refresh_and_get_connection_url();
                // Cache for direct reads by clients on Hub thread
                self.handle_cache.set_connection_url(result.clone());
                let _ = response_tx.send(result);
            }

            // ============================================================
            // Browser Client Support Commands
            // ============================================================

            HubCommand::GetCryptoService { response_tx } => {
                let _ = response_tx.send(self.browser.crypto_service.clone());
            }

            HubCommand::GetServerHubId { response_tx } => {
                let _ = response_tx.send(Some(self.server_hub_id().to_string()));
            }

            HubCommand::GetServerUrl { response_tx } => {
                let _ = response_tx.send(self.config.server_url.clone());
            }

            HubCommand::GetApiKey { response_tx } => {
                let _ = response_tx.send(self.config.get_api_key().to_string());
            }

            HubCommand::GetTokioRuntime { response_tx } => {
                let _ = response_tx.send(Some(self.tokio_runtime.handle().clone()));
            }

            // ============================================================
            // Browser PTY I/O Commands (fire-and-forget)
            // ============================================================

            HubCommand::BrowserPtyInput {
                client_id,
                agent_index,
                pty_index,
                data,
            } => {
                // Route input through Client trait
                if let Some(client) = self.clients.get(&client_id) {
                    if let Err(e) = client.send_input(agent_index, pty_index, &data) {
                        log::warn!("Failed to send browser PTY input: {}", e);
                    }
                }
            }

        }
    }

    /// Generate the connection URL from the current Signal bundle.
    ///
    /// Format: `{server_url}/hubs/{id}#{base32_binary_bundle}`
    /// - URL portion: byte mode (any case allowed)
    /// - Bundle (after #): alphanumeric mode (uppercase Base32)
    ///
    /// This is the canonical source for connection URLs. Use this method
    /// instead of accessing `hub.connection_url` cache directly.
    pub(crate) fn generate_connection_url(&self) -> Result<String, String> {
        let bundle = self
            .browser
            .signal_bundle
            .as_ref()
            .ok_or_else(|| "Signal bundle not initialized".to_string())?;

        let bytes = bundle
            .to_binary()
            .map_err(|e| format!("Cannot serialize PreKeyBundle: {}", e))?;

        let encoded = data_encoding::BASE32_NOPAD.encode(&bytes);
        let url = format!(
            "{}/hubs/{}#{}",
            self.config.server_url,
            self.server_hub_id(),
            encoded
        );

        log::debug!(
            "Generated connection URL: {} chars (QR alphanumeric capacity: 4296)",
            url.len()
        );

        Ok(url)
    }

    /// Request bundle regeneration from relay and return the new connection URL.
    ///
    /// This is a blocking operation that:
    /// 1. Requests bundle regeneration from the relay
    /// 2. Waits for the new bundle to arrive
    /// 3. Returns the new connection URL
    fn refresh_and_get_connection_url(&mut self) -> Result<String, String> {
        use std::time::Duration;

        // Check if relay is connected
        let sender = self
            .browser
            .sender
            .as_ref()
            .ok_or_else(|| "Relay not connected".to_string())?
            .clone();

        // Get original bundle bytes for comparison (if any)
        let original_bytes = self
            .browser
            .signal_bundle
            .as_ref()
            .and_then(|b| b.to_binary().ok());

        // Request bundle regeneration
        self.tokio_runtime.block_on(async {
            sender
                .request_bundle_regeneration()
                .await
                .map_err(|e| format!("Failed to request bundle regeneration: {}", e))
        })?;

        log::info!("Requested bundle regeneration, waiting for new bundle...");

        // Wait for new bundle (with timeout)
        // Bundle regeneration timeout: 10 seconds should be plenty
        const BUNDLE_TIMEOUT: Duration = Duration::from_secs(10);
        const POLL_INTERVAL: Duration = Duration::from_millis(50);

        let start = Instant::now();

        while start.elapsed() < BUNDLE_TIMEOUT {
            // Process relay events to receive the new bundle
            self.tokio_runtime.block_on(async {
                tokio::time::sleep(POLL_INTERVAL).await;
            });

            // Check if bundle changed by comparing binary representation
            if let Some(ref bundle) = self.browser.signal_bundle {
                if let Ok(new_bytes) = bundle.to_binary() {
                    let changed = match &original_bytes {
                        Some(orig) => &new_bytes != orig,
                        None => true, // Had no bundle before, now we do
                    };
                    if changed {
                        log::info!("New bundle received");
                        return self.generate_connection_url();
                    }
                }
            }
        }

        Err("Timeout waiting for new bundle".to_string())
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
    fn test_tui_selection_comes_from_hub() {
        use crate::client::ClientId;

        let config = test_config();
        let hub = Hub::new(config, TEST_DIMS).unwrap();

        // TUI client should be registered
        assert!(
            hub.clients.get(&ClientId::Tui).is_some(),
            "TuiClient should be registered"
        );

        // Initial selection should be None (no agents)
        assert!(
            hub.tui_selected_agent.is_none(),
            "TUI should have no selection initially"
        );
    }

    #[test]
    fn test_select_agent_for_client_updates_hub_tui_selection() {
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

        // Hub should now track the TUI selection
        assert_eq!(
            hub.tui_selected_agent.as_ref(),
            Some(&agent_key),
            "hub.tui_selected_agent should be updated by SelectAgentForClient"
        );
    }

    #[test]
    fn test_get_tui_selected_agent_uses_hub_field() {
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

        // Use SelectAgentForClient to select agent 2 via TUI
        hub.handle_action(HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: "test-repo-2".to_string(),
        });

        // The helper method should return the agent based on hub.tui_selected_agent
        let selected_key = hub.get_tui_selected_agent_key();
        assert_eq!(
            selected_key,
            Some("test-repo-2".to_string()),
            "get_tui_selected_agent_key should return hub.tui_selected_agent"
        );
    }

    // === TUI set_dims and resize_pty Test ===
    //
    // This test verifies that set_dims() updates local dims and resize_pty()
    // propagates to the connected PTY.
    //
    // Flow: TuiRunner sends TuiRequest::SetDims -> handle_request() updates dims
    // and calls resize_pty() with explicit agent/PTY indices.

    // === Agent Lifecycle / HandleCache Integration Tests ===
    //
    // These tests verify the integration between Hub, HandleCache, and Client trait
    // for agent lifecycle operations (create, select, delete).

    /// Helper: create a test agent and return (key, agent).
    fn make_agent(issue: u32) -> (String, crate::agent::Agent) {
        let agent = crate::agent::Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(issue),
            format!("botster-issue-{}", issue),
            PathBuf::from("/tmp/test"),
        );
        (format!("test-repo-{}", issue), agent)
    }

    /// Helper: add agent to hub state and return the key.
    fn add_agent_to_hub(hub: &Hub, issue: u32) -> String {
        let (key, agent) = make_agent(issue);
        hub.state.write().unwrap().add_agent(key.clone(), agent);
        key
    }

    /// Helper: spawn a command processor on Hub's tokio runtime for a given agent.
    ///
    /// Takes the command_rx from the PtySession and processes Connect/Disconnect commands.
    /// This unblocks `connect_blocking()` calls in `select_agent()` during tests.
    ///
    /// The processor runs as a tokio task on the Hub's runtime, which is necessary
    /// because `blocking_send`/`blocking_recv` on tokio channels need the runtime's
    /// worker threads to drive the async processing.
    fn spawn_pty_command_processor(hub: &Hub, agent_key: &str) {
        let command_rx = hub
            .state
            .write()
            .unwrap()
            .agents
            .get_mut(agent_key)
            .expect("agent must exist")
            .cli_pty
            .take_command_receiver()
            .expect("command_rx not yet taken");

        hub.tokio_runtime.spawn(async move {
            use crate::agent::pty::PtyCommand;

            let mut rx = command_rx;
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    PtyCommand::Connect { response_tx, .. } => {
                        let _ = response_tx.send(Vec::new());
                    }
                    _ => {
                        // Ignore other commands in tests
                    }
                }
            }
        });
    }

    #[test]
    fn test_handle_cache_syncs_on_agent_create() {
        let config = test_config();
        let hub = Hub::new(config, TEST_DIMS).unwrap();

        // Initially empty
        assert!(hub.handle_cache.is_empty());

        // Add agent to state
        let key = add_agent_to_hub(&hub, 42);

        // Sync cache
        hub.sync_handle_cache();

        // Cache should have 1 agent
        assert_eq!(hub.handle_cache.len(), 1);
        let cached = hub.handle_cache.get_agent(0);
        assert!(cached.is_some(), "get_agent(0) should return Some after sync");

        // Verify agent_id matches
        let handle = cached.unwrap();
        assert_eq!(handle.agent_id(), key);
    }

    #[test]
    fn test_handle_cache_syncs_on_agent_delete() {
        let config = test_config();
        let hub = Hub::new(config, TEST_DIMS).unwrap();

        // Add 2 agents
        let key1 = add_agent_to_hub(&hub, 1);
        let key2 = add_agent_to_hub(&hub, 2);
        hub.sync_handle_cache();

        assert_eq!(hub.handle_cache.len(), 2);
        assert_eq!(hub.handle_cache.get_agent(0).unwrap().agent_id(), &key1);
        assert_eq!(hub.handle_cache.get_agent(1).unwrap().agent_id(), &key2);

        // Remove agent 0 (key1)
        hub.state.write().unwrap().remove_agent(&key1);
        hub.sync_handle_cache();

        // Cache should now have 1 agent, and index 0 should point to what was agent 1 (key2)
        assert_eq!(hub.handle_cache.len(), 1);
        let remaining = hub.handle_cache.get_agent(0).unwrap();
        assert_eq!(
            remaining.agent_id(), key2,
            "After deleting agent 0, index 0 should now point to what was agent 1"
        );
    }

    #[test]
    fn test_select_agent_via_client_trait() {
        use crate::client::{TuiRequest, TuiAgentMetadata};

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Add agent and sync cache (required for HubHandle.get_agent() to work)
        let key = add_agent_to_hub(&hub, 42);
        hub.sync_handle_cache();

        // Spawn PTY command processor on Hub's runtime so connect_blocking() works.
        spawn_pty_command_processor(&hub, &key);

        // Create TuiRequest channel and register TuiClient with it
        let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<TuiRequest>();
        let _output_rx = hub.register_tui_client_with_request_channel(request_rx);

        // Send SelectAgent request through the channel
        let (response_tx, response_rx) = tokio::sync::oneshot::channel::<Option<TuiAgentMetadata>>();
        request_tx
            .send(TuiRequest::SelectAgent {
                index: 0,
                response_tx,
            })
            .unwrap();

        // poll_requests() -> handle_request(SelectAgent) -> select_agent(0)
        //   -> connect_to_pty_with_handle() -> pty_handle.connect_blocking()
        hub.clients.get_tui_mut().unwrap().poll_requests();

        // Verify response metadata
        let result = response_rx.blocking_recv().unwrap();
        assert!(result.is_some(), "SelectAgent(0) should return Some metadata");
        let metadata = result.unwrap();
        assert_eq!(metadata.agent_id, key);
        assert_eq!(metadata.agent_index, 0);
    }

    #[test]
    fn test_select_nonexistent_agent_returns_none() {
        use crate::client::{TuiRequest, TuiAgentMetadata};

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // No agents -- cache is empty

        // Create TuiRequest channel and register TuiClient
        let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<TuiRequest>();
        let _output_rx = hub.register_tui_client_with_request_channel(request_rx);

        // Send SelectAgent with out-of-bounds index
        let (response_tx, response_rx) = tokio::sync::oneshot::channel::<Option<TuiAgentMetadata>>();
        request_tx
            .send(TuiRequest::SelectAgent {
                index: 99,
                response_tx,
            })
            .unwrap();

        // Poll requests
        if let Some(tui) = hub.clients.get_tui_mut() {
            tui.poll_requests();
        }

        // Verify response is None
        let result = response_rx.blocking_recv().unwrap();
        assert!(result.is_none(), "SelectAgent(99) with no agents should return None");
    }

    #[test]
    fn test_create_delete_agent_cycle() {
        let config = test_config();
        let hub = Hub::new(config, TEST_DIMS).unwrap();

        // Add 3 agents, sync cache after each
        let key1 = add_agent_to_hub(&hub, 1);
        hub.sync_handle_cache();
        assert_eq!(hub.handle_cache.len(), 1);

        let key2 = add_agent_to_hub(&hub, 2);
        hub.sync_handle_cache();
        assert_eq!(hub.handle_cache.len(), 2);

        let key3 = add_agent_to_hub(&hub, 3);
        hub.sync_handle_cache();
        assert_eq!(hub.handle_cache.len(), 3);

        // Verify cache contents
        assert_eq!(hub.handle_cache.get_agent(0).unwrap().agent_id(), &key1);
        assert_eq!(hub.handle_cache.get_agent(1).unwrap().agent_id(), &key2);
        assert_eq!(hub.handle_cache.get_agent(2).unwrap().agent_id(), &key3);

        // Delete middle agent (key2)
        hub.state.write().unwrap().remove_agent(&key2);
        hub.sync_handle_cache();

        // Cache should have 2 agents with correct IDs
        assert_eq!(hub.handle_cache.len(), 2);
        assert_eq!(
            hub.handle_cache.get_agent(0).unwrap().agent_id(), &key1,
            "After deleting middle agent, index 0 should still be agent 1"
        );
        assert_eq!(
            hub.handle_cache.get_agent(1).unwrap().agent_id(), &key3,
            "After deleting middle agent, index 1 should now be agent 3"
        );
    }

    #[test]
    fn test_select_agent_after_deletion() {
        use crate::client::{TuiRequest, TuiAgentMetadata};

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Add 3 agents and sync cache
        let key1 = add_agent_to_hub(&hub, 1);
        let key2 = add_agent_to_hub(&hub, 2);
        let key3 = add_agent_to_hub(&hub, 3);
        hub.sync_handle_cache();

        // Spawn PTY command processors on Hub's runtime for all agents
        spawn_pty_command_processor(&hub, &key1);
        spawn_pty_command_processor(&hub, &key2);
        spawn_pty_command_processor(&hub, &key3);

        // Create TuiRequest channel and register TuiClient
        let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<TuiRequest>();
        let _output_rx = hub.register_tui_client_with_request_channel(request_rx);

        // Select agent at index 1 (should be key2)
        let (resp_tx1, resp_rx1) = tokio::sync::oneshot::channel::<Option<TuiAgentMetadata>>();
        request_tx
            .send(TuiRequest::SelectAgent {
                index: 1,
                response_tx: resp_tx1,
            })
            .unwrap();
        hub.clients.get_tui_mut().unwrap().poll_requests();
        let result1 = resp_rx1.blocking_recv().unwrap();
        assert_eq!(result1.as_ref().unwrap().agent_id, key2, "Index 1 should be agent 2");

        // Delete agent at index 0 (key1)
        hub.state.write().unwrap().remove_agent(&key1);
        hub.sync_handle_cache();

        // Now index 0 should be key2, index 1 should be key3
        // Select agent at index 0 (should now be key2)
        let (resp_tx2, resp_rx2) = tokio::sync::oneshot::channel::<Option<TuiAgentMetadata>>();
        request_tx
            .send(TuiRequest::SelectAgent {
                index: 0,
                response_tx: resp_tx2,
            })
            .unwrap();
        hub.clients.get_tui_mut().unwrap().poll_requests();
        let result2 = resp_rx2.blocking_recv().unwrap();
        assert!(result2.is_some(), "Index 0 after deletion should still return Some");
        assert_eq!(
            result2.unwrap().agent_id, key2,
            "After deleting agent 0, index 0 should now point to what was agent 1 (key2)"
        );
    }

    /// Test that TuiClient.set_dims() updates dims and resize_pty() propagates to PTY.
    ///
    /// After refactoring, set_dims() only updates local dims. PTY resize propagation
    /// happens through TuiRequest::SetDims which calls resize_pty() with explicit indices.
    ///
    /// The scenario:
    /// 1. Create Hub and Agent with PTY at (24 rows, 80 cols)
    /// 2. Register TuiClient and connect to the PTY
    /// 3. Call set_dims(100, 40) to update local dims
    /// 4. Call resize_pty(0, 0, 40, 100) to propagate to PTY
    /// 5. Verify: client dims updated and PTY resized
    #[test]
    fn test_tui_set_dims_and_resize_pty() {
        use crate::agent::Agent;
        use crate::client::ClientId;
        use std::path::PathBuf;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Register TuiClient
        let _output_rx = hub.register_tui_client();

        // Create an agent with PTY at default dimensions (24 rows, 80 cols)
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(42),
            "test-branch".to_string(),
            PathBuf::from("/tmp/test"),
        );

        let agent_key = "test-repo-42".to_string();
        hub.state.write().unwrap().add_agent(agent_key.clone(), agent);
        hub.sync_handle_cache();

        // Connect TuiClient to the PTY (agent_index=0, pty_index=0)
        // Must register with PTY so TUI becomes the size owner.
        {
            let state = hub.state.read().unwrap();
            let _ = state.agents.get(&agent_key).unwrap().cli_pty.connect(ClientId::Tui, (80, 24));
        }

        // Get initial PTY dimensions
        let initial_pty_dims = {
            let state = hub.state.read().unwrap();
            state.agents.get(&agent_key).unwrap().cli_pty.dimensions()
        };
        assert_eq!(initial_pty_dims, (24, 80), "Initial PTY dims");

        // Update dims on TuiClient (set_dims only updates local dims now)
        if let Some(client) = hub.clients.get_mut(&ClientId::Tui) {
            client.set_dims(100, 40);
        }

        // Verify TuiClient dims were updated
        let tui_dims = hub.clients.get(&ClientId::Tui).unwrap().dims();
        assert_eq!(tui_dims, (100, 40), "TuiClient dims should be updated");

        // PTY should NOT be resized yet (set_dims no longer propagates)
        let pty_dims_before_resize = {
            let state = hub.state.read().unwrap();
            state.agents.get(&agent_key).unwrap().cli_pty.dimensions()
        };
        assert_eq!(pty_dims_before_resize, (24, 80), "PTY should not be resized by set_dims alone");

        // Now call resize_pty directly (this is what TuiRequest::SetDims handler does)
        if let Some(client) = hub.clients.get_mut(&ClientId::Tui) {
            let _ = client.resize_pty(0, 0, 40, 100);
        }

        // Process pending PTY commands
        {
            let mut state = hub.state.write().unwrap();
            state.agents.get_mut(&agent_key).unwrap().cli_pty.process_commands();
        }

        // Verify PTY dimensions were updated
        let pty_dims_after = {
            let state = hub.state.read().unwrap();
            state.agents.get(&agent_key).unwrap().cli_pty.dimensions()
        };
        assert_eq!(
            pty_dims_after,
            (40, 100),
            "PTY should be resized after explicit resize_pty call"
        );
    }

    // === Resize Flow Integration Tests ===
    //
    // These tests verify the complete resize flow across different scenarios:
    // - Resize without connection (safety check)
    // - Multiple resizes (final state correctness)
    // - Browser resize via BrowserRequest channel
    // - Resize updates client dims even without PTY

    /// Helper: create a Hub with TuiClient registered and crypto service initialized.
    ///
    /// Matches the pattern from `actions::tests::test_hub()`. The crypto service
    /// is needed for BrowserClient creation via `ClientConnected` action.
    fn test_hub_with_crypto() -> Hub {
        use crate::relay::crypto_service::CryptoService;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();
        let _output_rx = hub.register_tui_client();

        // Initialize crypto service for browser client tests
        let crypto_service = CryptoService::start("test-hub").unwrap();
        hub.browser.crypto_service = Some(crypto_service);

        hub
    }

    /// Helper: connect TuiClient to an agent's PTY for testing.
    ///
    /// Registers the TUI as a connected client on the PTY (making it the size
    /// owner).
    fn connect_tui_to_pty(hub: &mut Hub, agent_key: &str) {
        let state = hub.state.read().unwrap();
        let _ = state
            .agents
            .get(agent_key)
            .unwrap()
            .cli_pty
            .connect(ClientId::Tui, (80, 24));
    }

    /// Test that calling set_dims() without a PTY connection is safe.
    ///
    /// Verifies:
    /// 1. No crash when resizing without a connected PTY
    /// 2. Client dims are updated correctly despite no PTY
    ///
    /// This is important because resize events can arrive before agent selection
    /// (e.g., terminal window resized while no agent is selected).
    #[test]
    fn test_resize_without_connection_is_safe() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Register TuiClient but do NOT connect to any PTY
        let _output_rx = hub.register_tui_client();

        // Verify initial dims
        let initial_dims = hub.clients.get(&ClientId::Tui).unwrap().dims();
        assert_eq!(initial_dims, (80, 24), "Initial TuiClient dims should be (80, 24)");

        // Call set_dims without any PTY connection - should NOT crash
        if let Some(client) = hub.clients.get_mut(&ClientId::Tui) {
            client.set_dims(120, 40);
        }

        // Verify dims were updated on the client despite no PTY
        let updated_dims = hub.clients.get(&ClientId::Tui).unwrap().dims();
        assert_eq!(
            updated_dims,
            (120, 40),
            "TuiClient dims should be updated to (120, 40) even without PTY connection"
        );
    }

    /// Test that multiple resize calls result in the correct final PTY state.
    ///
    /// Verifies:
    /// 1. Multiple rapid set_dims() + resize_pty() calls are all processed
    /// 2. PTY commands are processed after each resize
    /// 3. Final PTY dimensions match the last resize call
    ///
    /// This exercises the scenario where terminal resize events arrive in rapid
    /// succession (e.g., user dragging the window edge). In production, both
    /// set_dims() and resize_pty() are called from TuiRequest::SetDims handler.
    #[test]
    fn test_resize_multiple_times_final_state_correct() {
        use crate::agent::Agent;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();
        let _output_rx = hub.register_tui_client();

        // Create agent and connect TuiClient to PTY
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(42),
            "test-branch".to_string(),
            PathBuf::from("/tmp/test"),
        );
        let agent_key = "test-repo-42".to_string();
        hub.state.write().unwrap().add_agent(agent_key.clone(), agent);
        hub.sync_handle_cache();
        connect_tui_to_pty(&mut hub, &agent_key);

        // Verify initial PTY dimensions
        let initial_dims = {
            let state = hub.state.read().unwrap();
            state.agents.get(&agent_key).unwrap().cli_pty.dimensions()
        };
        assert_eq!(initial_dims, (24, 80), "Initial PTY dims should be (24, 80)");

        // Resize #1: (100 cols, 30 rows)
        if let Some(client) = hub.clients.get_mut(&ClientId::Tui) {
            client.set_dims(100, 30);
            let _ = client.resize_pty(0, 0, 30, 100);
        }
        {
            let mut state = hub.state.write().unwrap();
            state.agents.get_mut(&agent_key).unwrap().cli_pty.process_commands();
        }

        // Resize #2: (120 cols, 40 rows)
        if let Some(client) = hub.clients.get_mut(&ClientId::Tui) {
            client.set_dims(120, 40);
            let _ = client.resize_pty(0, 0, 40, 120);
        }
        {
            let mut state = hub.state.write().unwrap();
            state.agents.get_mut(&agent_key).unwrap().cli_pty.process_commands();
        }

        // Resize #3: (80 cols, 24 rows) - back to default
        if let Some(client) = hub.clients.get_mut(&ClientId::Tui) {
            client.set_dims(80, 24);
            let _ = client.resize_pty(0, 0, 24, 80);
        }
        {
            let mut state = hub.state.write().unwrap();
            state.agents.get_mut(&agent_key).unwrap().cli_pty.process_commands();
        }

        // Verify final PTY dimensions match the last resize: (24 rows, 80 cols)
        let final_pty_dims = {
            let state = hub.state.read().unwrap();
            state.agents.get(&agent_key).unwrap().cli_pty.dimensions()
        };
        assert_eq!(
            final_pty_dims,
            (24, 80),
            "Final PTY dims should be (24 rows, 80 cols) after three resizes"
        );

        // Verify client dims also match the last resize
        let client_dims = hub.clients.get(&ClientId::Tui).unwrap().dims();
        assert_eq!(
            client_dims,
            (80, 24),
            "TuiClient dims should be (80 cols, 24 rows) after three resizes"
        );
    }

    /// Test that BrowserRequest::Resize propagates to the PTY.
    ///
    /// Verifies the full browser resize flow:
    /// 1. BrowserClient receives BrowserRequest::Resize via its request channel
    /// 2. poll_requests() processes it and calls resize_pty()
    /// 3. PTY receives the resize command and updates dimensions
    ///
    /// This mirrors the production flow where the browser input receiver task
    /// sends BrowserRequest::Resize when it receives a resize command from the
    /// browser via the encrypted ActionCable channel.
    #[test]
    fn test_browser_resize_propagates_to_pty() {
        use crate::agent::Agent;
        use crate::client::{BrowserClient, BrowserRequest};
        use uuid::Uuid;

        let mut hub = test_hub_with_crypto();

        // Create agent
        let agent = Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(42),
            "test-branch".to_string(),
            PathBuf::from("/tmp/test"),
        );
        let agent_key = "test-repo-42".to_string();
        hub.state.write().unwrap().add_agent(agent_key.clone(), agent);
        hub.sync_handle_cache();

        // Register BrowserClient via ClientConnected action
        let browser_id = ClientId::Browser("test-browser-12345678".to_string());
        hub.handle_action(HubAction::ClientConnected {
            client_id: browser_id.clone(),
        });

        // Verify browser client was registered
        assert!(
            hub.clients.get(&browser_id).is_some(),
            "BrowserClient should be registered after ClientConnected"
        );

        // Connect browser client to the PTY (register as size owner)
        {
            let state = hub.state.read().unwrap();
            let _ = state
                .agents
                .get(&agent_key)
                .unwrap()
                .cli_pty
                .connect(browser_id.clone(), (80, 24));
        }

        // Get the request sender from the BrowserClient for sending BrowserRequest
        let request_tx = {
            let client = hub.clients.get(&browser_id).unwrap();
            let browser = client
                .as_any()
                .and_then(|a| a.downcast_ref::<BrowserClient>())
                .expect("Should be a BrowserClient");
            browser.request_sender_for_test()
        };

        // Send BrowserRequest::Resize through the channel
        request_tx
            .send(BrowserRequest::Resize {
                agent_index: 0,
                pty_index: 0,
                rows: 50,
                cols: 120,
            })
            .expect("Should send resize request");

        // Process the request via poll_requests() on BrowserClient
        {
            let client = hub.clients.get_mut(&browser_id).unwrap();
            let browser = client
                .as_any_mut()
                .and_then(|a| a.downcast_mut::<BrowserClient>())
                .expect("Should be a BrowserClient");
            browser.poll_requests();
        }

        // Process PTY commands to apply the resize
        {
            let mut state = hub.state.write().unwrap();
            state
                .agents
                .get_mut(&agent_key)
                .unwrap()
                .cli_pty
                .process_commands();
        }

        // Verify PTY dimensions updated to (50 rows, 120 cols)
        let pty_dims = {
            let state = hub.state.read().unwrap();
            state.agents.get(&agent_key).unwrap().cli_pty.dimensions()
        };
        assert_eq!(
            pty_dims,
            (50, 120),
            "PTY should be resized to (50 rows, 120 cols) after BrowserRequest::Resize"
        );
    }

    /// Test that set_dims() updates client dimensions even without a PTY.
    ///
    /// Verifies:
    /// 1. TuiClient starts with default dims (80, 24)
    /// 2. set_dims() updates the stored dims
    /// 3. dims() returns the new values
    ///
    /// This is important because the client tracks its terminal size
    /// independently of any PTY connection. When an agent is later selected,
    /// the stored dims are used for the initial PTY size.
    #[test]
    fn test_resize_updates_client_dims_even_without_pty() {
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();
        let _output_rx = hub.register_tui_client();

        // Verify default dims
        let default_dims = hub.clients.get(&ClientId::Tui).unwrap().dims();
        assert_eq!(
            default_dims,
            (80, 24),
            "TuiClient should start with default (80, 24)"
        );

        // Call set_dims without connecting to any PTY
        if let Some(client) = hub.clients.get_mut(&ClientId::Tui) {
            client.set_dims(120, 40);
        }

        // Verify dims() returns the new values
        let new_dims = hub.clients.get(&ClientId::Tui).unwrap().dims();
        assert_eq!(
            new_dims,
            (120, 40),
            "client.dims() should return (120, 40) after set_dims(120, 40)"
        );
    }

    // === Stress / Concurrency Tests ===
    //
    // These tests verify thread safety and deadlock freedom under concurrent
    // access patterns. Each uses a timeout to detect deadlocks.

    /// Run a closure on a background thread with a timeout.
    ///
    /// If the closure doesn't complete within the given duration, the test
    /// fails with a "possible deadlock" message. This is the primary mechanism
    /// for detecting deadlocks in concurrent tests.
    fn run_with_timeout<F: FnOnce() + Send + 'static>(f: F, timeout: std::time::Duration) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            f();
            let _ = tx.send(());
        });
        rx.recv_timeout(timeout).expect("Test timed out - possible deadlock");
    }

    /// Stress test: concurrent reads and writes to HandleCache.
    ///
    /// Spawns a reader thread that continuously reads from the cache while
    /// the main thread adds/removes agents and syncs the cache. Verifies
    /// that the RwLock-based HandleCache doesn't deadlock or panic under
    /// concurrent access.
    ///
    /// This exercises the production pattern where TuiClient/BrowserClient
    /// read from HandleCache (via HubHandle) while Hub mutates it on agent
    /// lifecycle events.
    #[test]
    fn test_handle_cache_concurrent_read_write() {
        use std::sync::Arc;
        use std::time::Duration;

        run_with_timeout(|| {
            let config = test_config();
            let hub = Hub::new(config, TEST_DIMS).unwrap();
            let cache = Arc::clone(&hub.handle_cache);

            // Spawn reader thread: continuously reads from cache
            let reader_cache = Arc::clone(&cache);
            let reader_handle = std::thread::spawn(move || {
                for _ in 0..100 {
                    // Exercise all read paths
                    let _len = reader_cache.len();
                    let _empty = reader_cache.is_empty();
                    let _agent = reader_cache.get_agent(0);
                    let _all = reader_cache.get_all_agents();

                    // Small yield to interleave with writer
                    std::thread::yield_now();
                }
            });

            // Main thread: add/remove agents and sync cache
            for i in 0..100u32 {
                let key = add_agent_to_hub(&hub, i);
                hub.sync_handle_cache();

                // Every other iteration, remove the agent we just added
                if i % 2 == 0 {
                    hub.state.write().unwrap().remove_agent(&key);
                    hub.sync_handle_cache();
                }
            }

            // Join reader - should not have panicked
            reader_handle.join().expect("Reader thread panicked during concurrent access");
        }, Duration::from_secs(5));
    }

    /// Stress test: rapid agent selection with no deadlock.
    ///
    /// Creates 3 agents, registers a TuiClient with a request channel,
    /// then sends 50 SelectAgent commands rapidly, alternating indices.
    /// Calls poll_requests() between each to process the selection.
    ///
    /// This exercises the production pattern where a user rapidly switches
    /// between agents via keyboard shortcuts. The test verifies that the
    /// selection/PTY-connect flow doesn't deadlock under rapid switching.
    #[test]
    fn test_rapid_agent_selection_no_deadlock() {
        use crate::client::{TuiAgentMetadata, TuiRequest};
        use std::time::Duration;

        run_with_timeout(|| {
            let config = test_config();
            let mut hub = Hub::new(config, TEST_DIMS).unwrap();

            // Add 3 agents
            let keys: Vec<String> = (1..=3).map(|i| add_agent_to_hub(&hub, i)).collect();
            hub.sync_handle_cache();

            // Spawn PTY command processors so connect_blocking() won't block forever
            for key in &keys {
                spawn_pty_command_processor(&hub, key);
            }

            // Register TuiClient with request channel
            let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<TuiRequest>();
            let _output_rx = hub.register_tui_client_with_request_channel(request_rx);

            // Rapid selection: 50 iterations, alternating between agents 0, 1, 2
            for i in 0..50usize {
                let index = i % 3;
                let (resp_tx, resp_rx) =
                    tokio::sync::oneshot::channel::<Option<TuiAgentMetadata>>();

                request_tx
                    .send(TuiRequest::SelectAgent {
                        index,
                        response_tx: resp_tx,
                    })
                    .unwrap();

                // Process the request (this is where deadlocks would surface)
                hub.clients.get_tui_mut().unwrap().poll_requests();

                // Consume response to avoid channel backup
                let _result = resp_rx.blocking_recv();
            }

            // Verify final state is consistent: cache should still have 3 agents
            assert_eq!(
                hub.handle_cache.len(),
                3,
                "HandleCache should still have 3 agents after rapid selection"
            );

            // Verify Hub's TUI selection is set (should be the last selected agent)
            // Last iteration: i=49, index=49%3=1, so agent at index 1 should be selected
            // Note: hub.tui_selected_agent is updated by SelectAgentForClient action,
            // but TuiClient.poll_requests() dispatches via HubHandle, not directly on Hub.
            // So we verify the cache is consistent instead.
            let cached_agent = hub.handle_cache.get_agent(1);
            assert!(
                cached_agent.is_some(),
                "Agent at index 1 should still be accessible after rapid selection"
            );
        }, Duration::from_secs(5));
    }

    /// Stress test: interleaved resize and agent selection.
    ///
    /// Interleaves set_dims() and SelectAgent calls to verify no crash
    /// or deadlock when terminal resize events arrive during agent selection.
    ///
    /// This exercises the production scenario where the user resizes the
    /// terminal window while also switching between agents.
    #[test]
    fn test_resize_during_agent_selection() {
        use crate::client::{TuiAgentMetadata, TuiRequest};
        use std::time::Duration;

        run_with_timeout(|| {
            let config = test_config();
            let mut hub = Hub::new(config, TEST_DIMS).unwrap();

            // Add an agent and sync cache
            let key = add_agent_to_hub(&hub, 42);
            hub.sync_handle_cache();

            // Spawn PTY command processor
            spawn_pty_command_processor(&hub, &key);

            // Register TuiClient with request channel
            let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel::<TuiRequest>();
            let _output_rx = hub.register_tui_client_with_request_channel(request_rx);

            // Interleave resize and selection 20 times
            for i in 0..20u16 {
                // Resize with varying dimensions
                let cols = 80 + i;
                let rows = 24 + (i % 10);
                if let Some(client) = hub.clients.get_mut(&ClientId::Tui) {
                    client.set_dims(cols, rows);
                }

                // Select agent (always index 0 since we only have one)
                let (resp_tx, resp_rx) =
                    tokio::sync::oneshot::channel::<Option<TuiAgentMetadata>>();
                request_tx
                    .send(TuiRequest::SelectAgent {
                        index: 0,
                        response_tx: resp_tx,
                    })
                    .unwrap();
                hub.clients.get_tui_mut().unwrap().poll_requests();
                let _result = resp_rx.blocking_recv();

                // Another resize after selection
                if let Some(client) = hub.clients.get_mut(&ClientId::Tui) {
                    client.set_dims(cols + 1, rows + 1);
                }
            }

            // Verify final client dims match last resize (cols=100, rows=33+1=34)
            // Last iteration: i=19, cols=80+19=99, rows=24+9=33
            // After-selection resize: cols=100, rows=34
            let final_dims = hub.clients.get(&ClientId::Tui).unwrap().dims();
            assert_eq!(
                final_dims,
                (100, 34),
                "Final client dims should match last set_dims call"
            );
        }, Duration::from_secs(5));
    }

    /// Consistency test: cache count matches state count through add/remove cycle.
    ///
    /// Adds 5 agents one by one, calling sync_handle_cache() after each, then
    /// removes 3 agents one by one with sync after each. At every step, verifies
    /// that the cache count matches the state count.
    ///
    /// This is a deterministic correctness test (not a concurrency test) that
    /// ensures the cache faithfully mirrors Hub state through lifecycle events.
    #[test]
    fn test_multiple_cache_syncs_are_consistent() {
        use std::time::Duration;

        run_with_timeout(|| {
            let config = test_config();
            let hub = Hub::new(config, TEST_DIMS).unwrap();

            // Add 5 agents one by one, verify cache matches state at each step
            let mut keys = Vec::new();
            for i in 1..=5u32 {
                let key = add_agent_to_hub(&hub, i);
                keys.push(key);
                hub.sync_handle_cache();

                let state_count = hub.state.read().unwrap().agent_count();
                let cache_count = hub.handle_cache.len();
                assert_eq!(
                    cache_count, state_count,
                    "After adding agent {i}: cache count ({cache_count}) should match state count ({state_count})"
                );
                assert_eq!(cache_count, i as usize);
            }

            // Remove 3 agents one by one, verify cache matches state at each step
            for (removed_count, key) in keys.iter().take(3).enumerate() {
                hub.state.write().unwrap().remove_agent(key);
                hub.sync_handle_cache();

                let state_count = hub.state.read().unwrap().agent_count();
                let cache_count = hub.handle_cache.len();
                let expected = 5 - (removed_count + 1);
                assert_eq!(
                    cache_count, state_count,
                    "After removing agent {}: cache count ({cache_count}) should match state count ({state_count})",
                    removed_count + 1
                );
                assert_eq!(cache_count, expected);
            }

            // Final verification: 2 agents remain
            assert_eq!(hub.handle_cache.len(), 2);
            assert_eq!(hub.state.read().unwrap().agent_count(), 2);

            // Verify the remaining agents are correct (keys[3] and keys[4])
            let cached_agents = hub.handle_cache.get_all_agents();
            assert_eq!(cached_agents[0].agent_id(), &keys[3]);
            assert_eq!(cached_agents[1].agent_id(), &keys[4]);
        }, Duration::from_secs(5));
    }
}
