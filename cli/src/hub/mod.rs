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
//! - `client_routing`: Client communication (agent/worktree lists, errors)
//! - `server_comms`: WebSocket command channel, notification worker, registration
//! - `actions`: Hub action dispatch
//! - `lifecycle`: Agent spawn/close operations
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
pub mod command_channel;
pub mod commands;
pub mod events;
pub mod handle_cache;
pub mod hub_handle;
pub mod lifecycle;
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

use std::net::TcpListener;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::Instant;

use reqwest::blocking::Client;

use sha2::{Digest, Sha256};

// ActionCable channel imports removed - preview channels now managed by BrowserClient
use crate::client::{ClientId, ClientRegistry, ClientTaskHandle};
use crate::config::Config;
use crate::device::Device;
use crate::git::WorktreeManager;

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

/// WebRTC virtual subscription for routing DataChannel messages.
///
/// Tracks which channel type and PTY coordinates a browser subscription maps to.
#[derive(Debug, Clone)]
pub struct WebRtcSubscription {
    /// Browser identity that owns this subscription.
    pub browser_identity: String,
    /// Channel name (e.g., "TerminalRelayChannel", "HubChannel").
    pub channel_name: String,
    /// Agent index if this is a PTY subscription.
    pub agent_index: Option<usize>,
    /// PTY index if this is a PTY subscription.
    pub pty_index: Option<usize>,
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
    /// Local identifier for this hub session (used for config directories).
    pub hub_identifier: String,
    /// Server-assigned ID for server communication (set after registration).
    pub botster_id: Option<String>,
    /// Async runtime for relay and preview channel operations.
    pub tokio_runtime: tokio::runtime::Runtime,

    // === Control Flags ===
    /// Whether the hub should quit.
    pub quit: bool,

    // === Timing ===
    /// Last time we sent a heartbeat.
    pub last_heartbeat: Instant,

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

    // === Background Workers ===
    /// Background worker for notification sending (non-blocking).
    pub notification_worker: Option<workers::NotificationWorker>,
    /// WebSocket command channel for real-time message delivery from Rails.
    pub command_channel: Option<command_channel::CommandChannelHandle>,

    // === WebRTC Channels ===
    /// WebRTC peer connections indexed by browser identity.
    ///
    /// Each browser that connects via WebRTC gets its own peer connection.
    /// The connection persists to keep the DataChannel alive.
    pub webrtc_channels: std::collections::HashMap<String, crate::channel::WebRtcChannel>,

    /// WebRTC virtual subscriptions tracking.
    ///
    /// Maps subscriptionId -> (browser_identity, channel_name, agent_index, pty_index).
    /// Used to route incoming DataChannel messages to the correct handler.
    pub webrtc_subscriptions: std::collections::HashMap<String, WebRtcSubscription>,

    // === Command Channel (Actor Pattern) ===
    /// Sender for Hub commands (cloned for each client).
    command_tx: tokio::sync::mpsc::Sender<HubCommand>,
    /// Receiver for Hub commands (owned by Hub, polled in event loop).
    command_rx: tokio::sync::mpsc::Receiver<HubCommand>,

    // === Event Broadcast ===
    /// Sender for Hub events (clients subscribe via `subscribe_events()`).
    event_tx: tokio::sync::broadcast::Sender<HubEvent>,
    /// Receiver for forwarding hub events to WebRTC subscribers.
    webrtc_event_rx: Option<tokio::sync::broadcast::Receiver<HubEvent>>,

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
            .finish_non_exhaustive()
    }
}

impl Hub {
    /// Create a new Hub with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The tokio runtime cannot be created
    /// - The HTTP client cannot be created
    /// - Device identity cannot be loaded
    pub fn new(config: Config) -> anyhow::Result<Self> {
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

        // Initialize heartbeat timestamp to past to trigger immediate heartbeat on first tick
        let past = Instant::now() - std::time::Duration::from_secs(3600);

        // Create channel for background agent creation results
        let (pending_agent_tx, pending_agent_rx) = std_mpsc::channel();
        // Create channel for progress updates during agent creation
        let (progress_tx, progress_rx) = std_mpsc::channel();

        // Create command channel for actor pattern
        let (command_tx, command_rx) = tokio::sync::mpsc::channel(256);
        // Create handle cache for thread-safe agent handle access
        let handle_cache = Arc::new(handle_cache::HandleCache::new());
        // Create event broadcast channel for pub/sub
        let (event_tx, webrtc_event_rx) = tokio::sync::broadcast::channel(64);

        Ok(Self {
            state,
            config,
            client,
            device,
            hub_identifier,
            botster_id: None,
            tokio_runtime,
            quit: false,
            last_heartbeat: past,
            browser: crate::relay::BrowserState::default(),
            clients: ClientRegistry::new(),
            pending_agent_tx,
            pending_agent_rx,
            progress_tx,
            progress_rx,
            // Workers are started later via start_background_workers() after registration
            notification_worker: None,
            command_channel: None,
            command_tx,
            command_rx,
            event_tx,
            webrtc_event_rx: Some(webrtc_event_rx),
            handle_cache,
            webrtc_channels: std::collections::HashMap::new(),
            webrtc_subscriptions: std::collections::HashMap::new(),
        })
    }

    /// Start background workers for non-blocking network I/O.
    ///
    /// Call this after hub registration completes and `botster_id` is set.
    /// Currently starts the NotificationWorker for background notification sending.
    pub fn start_background_workers(&mut self) {
        let worker_config = workers::WorkerConfig {
            server_url: self.config.server_url.clone(),
            api_key: self.config.get_api_key().to_string(),
            server_hub_id: self.server_hub_id().to_string(),
        };

        // Start notification worker
        if self.notification_worker.is_none() {
            log::info!("Starting background notification worker");
            self.notification_worker = Some(workers::NotificationWorker::new(worker_config));
        }
    }

    /// Connect to the HubCommandChannel for real-time message delivery.
    ///
    /// Call this after hub registration completes and `botster_id` is set.
    /// The command channel replaces HTTP polling for message delivery.
    pub fn connect_command_channel(&mut self) {
        if self.command_channel.is_some() {
            log::warn!("Command channel already connected");
            return;
        }

        let server_url = self.config.server_url.clone();
        let api_key = self.config.get_api_key().to_string();
        let hub_id = self.server_hub_id().to_string();

        // Start from sequence 0 on first connect (no messages acked yet)
        let start_from = 0i64;

        log::info!("Connecting command channel to {} (hub={})", server_url, hub_id);

        // Must enter tokio runtime context for tokio::spawn in connect()
        let _guard = self.tokio_runtime.enter();
        let handle = command_channel::connect(&server_url, &api_key, &hub_id, start_from);
        self.command_channel = Some(handle);
    }

    /// Shutdown all background workers gracefully.
    pub fn shutdown_background_workers(&mut self) {
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

    /// Get the hub ID to use for server communication.
    ///
    /// Returns the server-assigned `botster_id` if available (after registration),
    /// otherwise falls back to local `hub_identifier`.
    #[must_use]
    pub fn server_hub_id(&self) -> &str {
        self.botster_id.as_deref().unwrap_or(&self.hub_identifier)
    }

    /// Connect agent channels after spawn.
    ///
    /// NOTE: Preview channels are now managed by BrowserClient, not Agent.
    /// HttpChannel is created on-demand when a browser requests preview.
    /// Terminal channels are handled by PtySession broadcasts + browser relay.
    ///
    /// This method is kept for backwards compatibility but is now a no-op.
    /// The spawning code still calls this, but there's nothing to connect here.
    #[allow(unused_variables)]
    pub fn connect_agent_channels(&mut self, session_key: &str, agent_index: usize) {
        log::debug!(
            "connect_agent_channels called for {} (index={}) - channels managed by BrowserClient",
            session_key,
            agent_index
        );
        // Preview channels (HttpChannel) are now created lazily by BrowserClient
        // when a browser_wants_preview message is received.
        // See: cli/src/client/http_channel.rs
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

    // === Port Allocation ===

    /// Starting port for allocation (3000 is often used by Rails).
    const PORT_RANGE_START: u16 = 3001;
    /// Maximum port to try before giving up.
    const PORT_RANGE_END: u16 = 4000;

    /// Allocate a unique port for an agent's dev server.
    ///
    /// Finds an available port by:
    /// 1. Checking it's not already allocated to another agent
    /// 2. Verifying it's actually open via `TcpListener::bind`
    ///
    /// Ports are scanned starting from 3001 (avoiding 3000 which Rails commonly uses).
    ///
    /// # Returns
    ///
    /// - `Some(port)` if an available port was found and reserved
    /// - `None` if no port available after scanning the range
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(port) = hub.allocate_unique_port() {
    ///     log::info!("Allocated port {} for dev server", port);
    ///     // Port is now tracked in hub.state.allocated_ports
    /// }
    /// ```
    #[must_use]
    pub fn allocate_unique_port(&self) -> Option<u16> {
        let mut state = self.state.write().unwrap();

        for port in Self::PORT_RANGE_START..=Self::PORT_RANGE_END {
            // Skip if already allocated to another agent
            if state.allocated_ports.contains(&port) {
                continue;
            }

            // Check if port is actually available
            if TcpListener::bind(format!("127.0.0.1:{port}")).is_ok() {
                state.allocated_ports.insert(port);
                log::debug!("Allocated port {port} (total allocated: {})", state.allocated_ports.len());
                return Some(port);
            }
        }

        log::warn!(
            "No available ports in range {}-{}",
            Self::PORT_RANGE_START,
            Self::PORT_RANGE_END
        );
        None
    }

    /// Release a previously allocated port.
    ///
    /// Call this when an agent is deleted to return its port to the pool.
    /// Safe to call with a port that wasn't allocated (no-op).
    pub fn release_port(&self, port: u16) {
        let mut state = self.state.write().unwrap();
        if state.allocated_ports.remove(&port) {
            log::debug!("Released port {port} (total allocated: {})", state.allocated_ports.len());
        }
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


    // === Event Loop ===

    /// Perform all initial setup steps.
    ///
    /// Note: Signal Protocol bundle generation is deferred until the connection
    /// URL is first requested (TUI QR display, external automation, etc.).
    /// This avoids blocking boot for up to 10 seconds on bundle generation.
    pub fn setup(&mut self) {
        self.register_device();
        self.register_hub_with_server();
        self.init_signal_protocol();

        // Start background workers for non-blocking network I/O
        // Must be called after register_hub_with_server() sets botster_id
        self.start_background_workers();

        // Connect command channel for real-time WebSocket message delivery
        // Must be called after register_hub_with_server() sets botster_id
        self.connect_command_channel();

        // Seed shared state so clients have data immediately
        if let Err(e) = self.load_available_worktrees() {
            log::warn!("Failed to load initial worktrees: {}", e);
        }

        // Bundle generation is deferred - don't call generate_connection_url() here.
        // The bundle will be generated lazily when:
        // 1. TUI requests QR code display (GetConnectionCode command)
        // 2. External automation requests the connection URL
        // This avoids blocking boot for up to 10 seconds.
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
        // Shutdown command channel
        if let Some(ref channel) = self.command_channel {
            channel.shutdown();
        }
        self.command_channel = None;

        // Shutdown background workers (allows pending notifications to drain)
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

    /// Register TuiClient with output channel and request channel.
    ///
    /// Creates a TuiClient, wires the request channel, spawns it as an async
    /// task via `run_task()`, and registers a `ClientTaskHandle` in the registry.
    /// TuiRunner sends `TuiRequest` messages through the request channel,
    /// TuiClient processes them in its async task loop.
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

        // Create TuiClient with output channel and hub event subscription
        let hub_handle = self.handle();
        let hub_event_rx = self.subscribe_events();
        let mut tui_client = crate::client::TuiClient::new(hub_handle, output_tx, Some(hub_event_rx));

        // Wire the request channel
        tui_client.set_request_receiver(request_rx);

        // Spawn TuiClient as an async task on the tokio runtime
        let join_handle = self.tokio_runtime.spawn(tui_client.run_task());

        // Register the task handle (replaces existing if any)
        self.clients.register(ClientId::Tui, ClientTaskHandle {
            join_handle,
        });

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
                    dims: request.dims,
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
                agent_index: _,
                pty_index: _,
                data: _,
            } => {
                // Browser PTY input now flows through BrowserClient's run_task()
                // via BrowserRequest channel, not through Hub commands.
                log::debug!(
                    "BrowserPtyInput received for {} (routed via client task)",
                    client_id
                );
            }

        }
    }

    /// Generate connection URL, lazily generating bundle if needed.
    ///
    /// Format: `{server_url}/hubs/{id}#{base32_binary_bundle}`
    /// - URL portion: byte mode (any case allowed)
    /// - Bundle (after #): alphanumeric mode (uppercase Base32)
    ///
    /// On first call, this generates the PreKeyBundle (lazy initialization).
    /// Subsequent calls return the cached bundle unless it was used.
    pub(crate) fn generate_connection_url(&mut self) -> Result<String, String> {
        // Delegate to the lazy generation function which handles caching
        self.get_or_generate_connection_url()
    }

    /// Regenerate the PreKeyBundle and return the new connection URL.
    ///
    /// Forces bundle regeneration even if a cached bundle exists.
    fn refresh_and_get_connection_url(&mut self) -> Result<String, String> {
        // Mark bundle as used to force regeneration
        self.browser.bundle_used = true;

        // Delegate to lazy generation (will regenerate due to bundle_used flag)
        self.get_or_generate_connection_url()
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

    #[test]
    fn test_hub_creation() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

        assert!(!hub.should_quit());
        assert_eq!(hub.agent_count(), 0);
    }

    #[test]
    fn test_hub_quit() {
        let config = test_config();
        let mut hub = Hub::new(config).unwrap();

        assert!(!hub.should_quit());
        hub.request_quit();
        assert!(hub.should_quit());
    }

    #[test]
    fn test_handle_action_quit() {
        let config = test_config();
        let mut hub = Hub::new(config).unwrap();

        hub.handle_action(HubAction::Quit);
        assert!(hub.should_quit());
    }

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

    #[test]
    fn test_handle_cache_syncs_on_agent_create() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

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
        let hub = Hub::new(config).unwrap();

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

    // Note: Tests for TuiClient request processing (select_agent_via_client_trait,
    // select_nonexistent_agent_returns_none) were removed because TuiClient now
    // processes requests in its own async task. Those flows are tested in
    // client/tui.rs tests. Hub-side tests verify selection tracking via
    // handle_action(SelectAgentForClient) above.

    #[test]
    fn test_create_delete_agent_cycle() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

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
            let hub = Hub::new(config).unwrap();
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

    /// Stress test: rapid agent selection via Hub actions.
    ///
    /// Creates 3 agents, then sends 50 SelectAgentForClient actions rapidly,
    /// alternating between agents. Verifies that the handle cache remains
    /// consistent throughout.
    #[test]
    fn test_rapid_agent_selection_no_deadlock() {
        use std::time::Duration;

        run_with_timeout(|| {
            let config = test_config();
            let mut hub = Hub::new(config).unwrap();

            // Add 3 agents
            let keys: Vec<String> = (1..=3).map(|i| add_agent_to_hub(&hub, i)).collect();
            hub.sync_handle_cache();

            // Rapid selection: 50 iterations, alternating between agents
            for i in 0..50usize {
                let index = i % 3;
                let agent_key = keys[index].clone();

                hub.handle_action(HubAction::SelectAgentForClient {
                    client_id: ClientId::Tui,
                    agent_key,
                });
            }

            // Verify final state is consistent: cache should still have 3 agents
            assert_eq!(
                hub.handle_cache.len(),
                3,
                "HandleCache should still have 3 agents after rapid selection"
            );

            let cached_agent = hub.handle_cache.get_agent(1);
            assert!(
                cached_agent.is_some(),
                "Agent at index 1 should still be accessible after rapid selection"
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
            let hub = Hub::new(config).unwrap();

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

    // === Port Allocation Tests ===

    #[test]
    fn test_allocate_unique_port_returns_port() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

        let port = hub.allocate_unique_port();
        assert!(port.is_some(), "Should allocate a port");

        let port = port.unwrap();
        assert!(
            port >= Hub::PORT_RANGE_START && port <= Hub::PORT_RANGE_END,
            "Port {} should be in range {}-{}",
            port,
            Hub::PORT_RANGE_START,
            Hub::PORT_RANGE_END
        );
    }

    #[test]
    fn test_allocate_unique_port_tracks_allocation() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

        let port1 = hub.allocate_unique_port().unwrap();
        let port2 = hub.allocate_unique_port().unwrap();

        assert_ne!(port1, port2, "Should allocate different ports");

        // Verify both are tracked
        let state = hub.state.read().unwrap();
        assert!(state.allocated_ports.contains(&port1));
        assert!(state.allocated_ports.contains(&port2));
        assert_eq!(state.allocated_ports.len(), 2);
    }

    #[test]
    fn test_release_port() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

        let port = hub.allocate_unique_port().unwrap();
        assert!(hub.state.read().unwrap().allocated_ports.contains(&port));

        hub.release_port(port);
        assert!(!hub.state.read().unwrap().allocated_ports.contains(&port));
    }

    #[test]
    fn test_release_port_allows_reallocation() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

        let port1 = hub.allocate_unique_port().unwrap();
        hub.release_port(port1);

        // The released port should be available again (assuming no other process grabbed it)
        let port2 = hub.allocate_unique_port().unwrap();

        // port2 could be port1 or another port - we just verify allocation works
        assert!(
            port2 >= Hub::PORT_RANGE_START && port2 <= Hub::PORT_RANGE_END,
            "Should allocate a valid port after release"
        );
    }

    #[test]
    fn test_release_unallocated_port_is_noop() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

        // Should not panic or error
        hub.release_port(9999);
        assert!(hub.state.read().unwrap().allocated_ports.is_empty());
    }
}
