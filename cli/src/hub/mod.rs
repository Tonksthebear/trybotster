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
pub mod lifecycle;
pub mod menu;
pub mod polling;
pub mod registration;
pub mod run;
pub mod state;
pub mod workers;

pub use actions::{HubAction, ScrollDirection};
pub use crate::agents::AgentSpawnConfig;
pub use lifecycle::{close_agent, spawn_agent, SpawnResult};
pub use menu::{build_menu, MenuAction, MenuContext, MenuItem};
pub use state::HubState;

use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::Instant;

use reqwest::blocking::Client;

use sha2::{Digest, Sha256};

use crate::app::AppMode;
use crate::client::{ClientId, ClientRegistry, Response, TuiClient};
use crate::config::Config;
use crate::device::Device;
use crate::git::WorktreeManager;
use crate::relay::AgentInfo;
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
    /// Core agent and worktree state.
    pub state: HubState,
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
        use std::time::Duration;

        let state = HubState::new(config.worktree_base.clone());
        let tokio_runtime = tokio::runtime::Runtime::new()?;

        // Generate stable hub_identifier: env var (for testing) > repo path > UUID
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
                Err(_) => {
                    // Fallback to UUID if not in a repo
                    let id = uuid::Uuid::new_v4().to_string();
                    log::info!("Hub identifier (random): {}...", &id[..8]);
                    id
                }
            }
        };

        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

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
                registry.register(Box::new(TuiClient::new()));
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

    /// Get the number of active agents.
    #[must_use]
    pub fn agent_count(&self) -> usize {
        self.state.agent_count()
    }

    /// Get the currently selected agent for TUI.
    ///
    /// This uses `TuiClient.state().selected_agent` as the source of truth,
    /// NOT `HubState.selected`. This is part of the client abstraction.
    #[must_use]
    pub fn selected_agent(&self) -> Option<&crate::agent::Agent> {
        self.get_tui_selected_agent_key()
            .and_then(|key| self.state.agents.get(&key))
    }

    /// Get a mutable reference to the currently selected agent for TUI.
    #[must_use]
    pub fn selected_agent_mut(&mut self) -> Option<&mut crate::agent::Agent> {
        let key = self.get_tui_selected_agent_key()?;
        self.state.agents.get_mut(&key)
    }

    /// Get the selected agent key from TuiClient.
    ///
    /// TuiClient is the single source of truth for TUI's selection.
    /// This replaces the old `hub.state.selected` index-based approach.
    #[must_use]
    pub fn get_tui_selected_agent_key(&self) -> Option<String> {
        self.clients
            .get(&ClientId::Tui)
            .and_then(|client| client.state().selected_agent.clone())
    }

    /// Get the next agent key for a client's navigation.
    ///
    /// Returns the next agent in the ordered list, wrapping around.
    /// If no agent is selected, returns the first agent.
    #[must_use]
    pub fn get_next_agent_key(&self, client_id: &ClientId) -> Option<String> {
        if self.state.agent_keys_ordered.is_empty() {
            return None;
        }

        let current = self.clients.get(client_id)
            .and_then(|c| c.state().selected_agent.as_ref());

        match current {
            Some(key) => {
                let idx = self.state.agent_keys_ordered.iter()
                    .position(|k| k == key)
                    .unwrap_or(0);
                let next_idx = (idx + 1) % self.state.agent_keys_ordered.len();
                Some(self.state.agent_keys_ordered[next_idx].clone())
            }
            None => Some(self.state.agent_keys_ordered[0].clone()),
        }
    }

    /// Get the previous agent key for a client's navigation.
    ///
    /// Returns the previous agent in the ordered list, wrapping around.
    /// If no agent is selected, returns the last agent.
    #[must_use]
    pub fn get_previous_agent_key(&self, client_id: &ClientId) -> Option<String> {
        if self.state.agent_keys_ordered.is_empty() {
            return None;
        }

        let current = self.clients.get(client_id)
            .and_then(|c| c.state().selected_agent.as_ref());

        match current {
            Some(key) => {
                let idx = self.state.agent_keys_ordered.iter()
                    .position(|k| k == key)
                    .unwrap_or(0);
                let prev_idx = if idx == 0 {
                    self.state.agent_keys_ordered.len() - 1
                } else {
                    idx - 1
                };
                Some(self.state.agent_keys_ordered[prev_idx].clone())
            }
            None => Some(self.state.agent_keys_ordered.last()?.clone()),
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
    /// Delegates to `HubState::load_available_worktrees()`.
    pub fn load_available_worktrees(&mut self) -> anyhow::Result<()> {
        self.state.load_available_worktrees()
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

    // === Event Loop Support ===

    /// Perform periodic tasks (polling, heartbeat, notifications).
    ///
    /// Call this from your event loop to handle time-based operations.
    /// This method is **non-blocking** when background workers are running.
    ///
    /// # Worker-based flow (non-blocking)
    ///
    /// When workers are active (after `setup()`):
    /// - `poll_worker_messages()` - non-blocking try_recv from PollingWorker
    /// - `update_heartbeat_agents()` - non-blocking send to HeartbeatWorker
    /// - `poll_agent_notifications_async()` - non-blocking send to NotificationWorker
    ///
    /// # Fallback flow (blocking)
    ///
    /// When workers aren't available (offline mode, testing):
    /// - Falls back to blocking HTTP calls on main thread
    pub fn tick(&mut self) {
        // Process completed background agent creations (always non-blocking)
        self.poll_pending_agents();

        // Process progress events from background agent creations
        self.poll_progress_events();

        // Use background workers if available (non-blocking)
        if self.polling_worker.is_some() {
            self.poll_worker_messages();
            self.update_heartbeat_agents();
            self.poll_agent_notifications_async();
        } else {
            // Fallback to blocking calls (offline mode, testing, or before setup)
            self.poll_messages();
            self.send_heartbeat();
            self.poll_agent_notifications();
        }
    }

    /// Poll for messages from background worker (non-blocking).
    ///
    /// Checks the polling worker's result channel for new messages
    /// and processes them without blocking the main thread.
    fn poll_worker_messages(&mut self) {
        // Collect all available results first (to release borrow)
        let results: Vec<workers::PollingResult> = {
            let Some(ref worker) = self.polling_worker else {
                return;
            };

            let mut results = Vec::new();
            while let Some(result) = worker.try_recv() {
                results.push(result);
            }
            results
        };

        // Now process each result (borrow released)
        for result in results {
            match result {
                workers::PollingResult::Messages(messages) => {
                    if !messages.is_empty() {
                        self.process_polled_messages(messages);
                    }
                }
                workers::PollingResult::Skipped => {
                    // Offline mode or similar - nothing to do
                }
                workers::PollingResult::Error(e) => {
                    log::debug!("Background poll error (will retry): {e}");
                }
            }
        }
    }

    /// Process messages received from background polling.
    ///
    /// Converts messages to actions and dispatches them.
    /// Acknowledgments are queued back to the worker thread.
    fn process_polled_messages(&mut self, messages: Vec<crate::server::types::MessageData>) {
        use crate::server::messages::{message_to_hub_action, MessageContext, ParsedMessage};

        // Detect repo for context
        let (repo_path, repo_name) = if let Ok(repo) = std::env::var("BOTSTER_REPO") {
            (std::path::PathBuf::from("."), repo)
        } else {
            match crate::git::WorktreeManager::detect_current_repo() {
                Ok(result) => result,
                Err(_) if crate::env::is_test_mode() => {
                    (std::path::PathBuf::from("."), "test/repo".to_string())
                }
                Err(e) => {
                    log::warn!("Not in a git repository, skipping message processing: {e}");
                    return;
                }
            }
        };

        log::info!("Processing {} messages from background poll", messages.len());

        let context = MessageContext {
            repo_path,
            repo_name: repo_name.clone(),
            worktree_base: self.config.worktree_base.clone(),
            max_sessions: self.config.max_sessions,
            current_agent_count: self.state.agent_count(),
        };

        for msg in &messages {
            let parsed = ParsedMessage::from_message_data(msg);

            // Try to notify existing agent first
            if self.try_notify_existing_agent(&parsed, &context.repo_name) {
                self.acknowledge_message_async(msg.id);
                continue;
            }

            // Convert to action and dispatch
            match message_to_hub_action(&parsed, &context) {
                Ok(Some(action)) => {
                    self.handle_action(action);
                    self.acknowledge_message_async(msg.id);
                }
                Ok(None) => self.acknowledge_message_async(msg.id),
                Err(e) => {
                    // IMPORTANT: Acknowledge even on error to prevent infinite redelivery.
                    // The message is malformed or we can't handle it - retrying won't help.
                    log::error!("Failed to process message {}: {e} (acknowledging to prevent redelivery)", msg.id);
                    self.acknowledge_message_async(msg.id);
                }
            }
        }
    }

    /// Queue message acknowledgment to background worker (non-blocking).
    fn acknowledge_message_async(&self, message_id: i64) {
        if let Some(ref worker) = self.polling_worker {
            worker.acknowledge(message_id);
        } else {
            // Fallback to blocking ack
            self.acknowledge_message(message_id);
        }
    }

    /// Update heartbeat worker with current agent list (non-blocking).
    ///
    /// Only sends updates when the agent count changes to avoid
    /// sending redundant data every tick (60 FPS would be wasteful).
    ///
    /// The heartbeat worker maintains its own 30-second timer, so we just
    /// need to keep it updated with the current agent list.
    fn update_heartbeat_agents(&mut self) {
        let Some(ref worker) = self.heartbeat_worker else {
            return;
        };

        // Only send if agent count changed (simple change detection)
        let current_count = self.state.agents.len();
        if current_count == self.last_heartbeat_agent_count {
            return;
        }
        self.last_heartbeat_agent_count = current_count;

        // Build agent data for heartbeat
        let agents: Vec<workers::HeartbeatAgentData> = self
            .state
            .agents
            .values()
            .map(|agent| workers::HeartbeatAgentData {
                session_key: agent.session_key(),
                last_invocation_url: agent.last_invocation_url.clone(),
            })
            .collect();

        log::debug!("Heartbeat agent list updated: {} agents", agents.len());
        worker.update_agents(agents);
    }

    /// Poll agents for notifications and send via background worker (non-blocking).
    ///
    /// Collects notifications from all agents and queues them to the
    /// notification worker for background sending to Rails.
    fn poll_agent_notifications_async(&self) {
        use crate::agent::AgentNotification;

        let Some(ref worker) = self.notification_worker else {
            return;
        };

        // Collect and send notifications from all agents
        for agent in self.state.agents.values() {
            for notification in agent.poll_notifications() {
                // Only send if we have issue context (otherwise there's nowhere to post)
                if agent.issue_number.is_none() && agent.last_invocation_url.is_none() {
                    continue;
                }

                let notification_type = match &notification {
                    AgentNotification::Osc9(_) | AgentNotification::Osc777 { .. } => "question_asked",
                };

                log::info!(
                    "Agent {} sent notification: {} (url: {:?})",
                    agent.session_key(), notification_type, agent.last_invocation_url
                );

                let request = workers::NotificationRequest {
                    repo: agent.repo.clone(),
                    issue_number: agent.issue_number,
                    invocation_url: agent.last_invocation_url.clone(),
                    notification_type: notification_type.to_string(),
                };

                worker.send(request);
            }
        }
    }

    /// Poll for completed background agent creation tasks.
    ///
    /// Non-blocking check for results from spawn_blocking tasks.
    /// Processes all completed creations and sends appropriate responses to clients.
    pub fn poll_pending_agents(&mut self) {
        // Process all pending results (non-blocking)
        while let Ok(pending) = self.pending_agent_rx.try_recv() {
            self.handle_pending_agent_result(pending);
        }
    }

    /// Poll for progress events from background agent creation.
    ///
    /// Non-blocking check for progress updates. Sends progress to the requesting
    /// client (browser or TUI).
    pub fn poll_progress_events(&mut self) {
        while let Ok(event) = self.progress_rx.try_recv() {
            self.handle_progress_event(event);
        }
    }

    /// Handle a progress event from background agent creation.
    fn handle_progress_event(&mut self, event: AgentProgressEvent) {
        log::debug!(
            "Progress: {} -> {:?} for client {:?}",
            event.identifier,
            event.stage,
            event.client_id
        );

        // Send progress to browser clients via relay
        if let ClientId::Browser(ref identity) = event.client_id {
            if let Some(ref sender) = self.browser.sender {
                let ctx = crate::relay::BrowserSendContext {
                    sender,
                    runtime: &self.tokio_runtime,
                };
                crate::relay::send_agent_progress_to(
                    &ctx,
                    identity,
                    &event.identifier,
                    event.stage,
                );
            }
        }

        // Track TUI creation progress for display
        if event.client_id.is_tui() {
            self.creating_agent = Some((event.identifier.clone(), event.stage));
        }
    }

    /// Handle a completed agent creation from background thread.
    ///
    /// The background thread has completed the slow git/file operations.
    /// Now we do the fast PTY spawn on the main thread (needs &mut state).
    fn handle_pending_agent_result(&mut self, pending: PendingAgentResult) {
        // Clear TUI creating indicator on completion (success or failure)
        if pending.client_id.is_tui() {
            self.creating_agent = None;
        }

        match pending.result {
            Ok(_) => {
                // Background work succeeded - now spawn the agent (fast, needs &mut state)
                log::info!(
                    "Background worktree ready for {:?}, spawning agent...",
                    pending.client_id
                );

                // Get client dims for PTY
                let dims = self.clients.get(&pending.client_id)
                    .and_then(|c| c.state().dims)
                    .unwrap_or(self.terminal_dims);

                // Spawn agent (fast - just PTY creation)
                match lifecycle::spawn_agent(&mut self.state, &pending.config, dims) {
                    Ok(result) => {
                        log::info!(
                            "Agent spawned: {} for client {:?}",
                            result.session_key,
                            pending.client_id
                        );

                        // Register tunnel if port assigned
                        if let Some(port) = result.tunnel_port {
                            let tm = Arc::clone(&self.tunnel_manager);
                            let key = result.session_key.clone();
                            self.tokio_runtime.spawn(async move {
                                tm.register_agent(key, port).await;
                            });
                        }

                        // Send response to requesting client
                        if let Some(client) = self.clients.get_mut(&pending.client_id) {
                            client.receive_response(Response::agent_created(&result.session_key));
                        }

                        // Broadcast updated agent list to all clients
                        self.broadcast_agent_list();

                        // Auto-select the new agent for the requesting client
                        let session_key = result.session_key.clone();
                        actions::dispatch(
                            self,
                            HubAction::SelectAgentForClient {
                                client_id: pending.client_id.clone(),
                                agent_key: session_key.clone(),
                            },
                        );

                        // Also auto-select for TUI if it has no selection
                        // (ensures TUI state matches what's visually displayed)
                        if pending.client_id != ClientId::Tui {
                            let tui_has_selection = self.get_tui_selected_agent_key().is_some();
                            if !tui_has_selection {
                                actions::dispatch(
                                    self,
                                    HubAction::SelectAgentForClient {
                                        client_id: ClientId::Tui,
                                        agent_key: session_key,
                                    },
                                );
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to spawn agent: {}", e);
                        self.send_error_to(&pending.client_id, format!("Failed to spawn agent: {}", e));
                    }
                }
            }
            Err(e) => {
                log::error!(
                    "Background agent creation failed for {:?}: {}",
                    pending.client_id,
                    e
                );
                self.send_error_to(&pending.client_id, format!("Failed to create agent: {}", e));
            }
        }
    }

    // === Server Communication ===

    /// Build polling configuration from Hub state.
    fn polling_config(&self) -> polling::PollingConfig<'_> {
        polling::PollingConfig {
            client: &self.client,
            server_url: &self.config.server_url,
            api_key: self.config.get_api_key(),
            poll_interval: self.config.poll_interval,
            server_hub_id: self.server_hub_id(),
        }
    }

    /// Poll the server for new messages and process them.
    ///
    /// This method polls at the configured interval and processes any pending
    /// messages from the server, converting them to HubActions.
    pub fn poll_messages(&mut self) {
        use crate::server::messages::{message_to_hub_action, MessageContext, ParsedMessage};
        use std::time::Duration;

        if polling::should_skip_polling(self.quit, self.polling_enabled) {
            return;
        }
        if self.last_poll.elapsed() < Duration::from_secs(self.config.poll_interval) {
            return;
        }
        self.last_poll = Instant::now();

        // Detect repo: env var > git detection > test fallback
        let (repo_path, repo_name) = if let Ok(repo) = std::env::var("BOTSTER_REPO") {
            // Explicit repo override (used in tests and special cases)
            (std::path::PathBuf::from("."), repo)
        } else {
            match crate::git::WorktreeManager::detect_current_repo() {
                Ok(result) => result,
                Err(_) if crate::env::is_test_mode() => {
                    // Test mode fallback - use dummy repo
                    (std::path::PathBuf::from("."), "test/repo".to_string())
                }
                Err(e) => {
                    log::warn!("Not in a git repository, skipping poll: {e}");
                    return;
                }
            }
        };

        let messages = polling::poll_messages(&self.polling_config(), &repo_name);
        if messages.is_empty() {
            return;
        }

        log::info!("Polled {} messages for repo={}", messages.len(), repo_name);

        let context = MessageContext {
            repo_path,
            repo_name: repo_name.clone(),
            worktree_base: self.config.worktree_base.clone(),
            max_sessions: self.config.max_sessions,
            current_agent_count: self.state.agent_count(),
        };

        for msg in &messages {
            let parsed = ParsedMessage::from_message_data(msg);

            // Try to notify existing agent first
            if self.try_notify_existing_agent(&parsed, &context.repo_name) {
                self.acknowledge_message(msg.id);
                continue;
            }

            // Convert to action and dispatch
            match message_to_hub_action(&parsed, &context) {
                Ok(Some(action)) => {
                    self.handle_action(action);
                    self.acknowledge_message(msg.id);
                }
                Ok(None) => self.acknowledge_message(msg.id),
                Err(e) => log::error!("Failed to process message {}: {e}", msg.id),
            }
        }
    }

    /// Try to send a notification to an existing agent for this issue.
    ///
    /// Returns true if an agent was found and notified, false otherwise.
    /// Does NOT apply to cleanup messages - those need to go through the action dispatch.
    fn try_notify_existing_agent(
        &mut self,
        parsed: &crate::server::messages::ParsedMessage,
        default_repo: &str,
    ) -> bool {
        // Cleanup messages should not be treated as notifications
        if parsed.is_cleanup() {
            return false;
        }

        let Some(issue_number) = parsed.issue_number else {
            return false;
        };

        let repo_safe = parsed.repo.as_deref().unwrap_or(default_repo).replace('/', "-");
        let session_key = format!("{repo_safe}-{issue_number}");

        let Some(agent) = self.state.agents.get_mut(&session_key) else {
            return false;
        };

        log::info!("Agent exists for issue #{issue_number}, sending notification");
        let notification = parsed.format_notification();

        if let Err(e) = agent.write_input_to_cli(notification.as_bytes()) {
            log::error!("Failed to send notification to agent: {e}");
        } else {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let _ = agent.write_input_to_cli(b"\r");
            std::thread::sleep(std::time::Duration::from_millis(50));
            let _ = agent.write_input_to_cli(b"\r");
        }

        true
    }

    /// Acknowledge a message to the server.
    fn acknowledge_message(&self, message_id: i64) {
        let config = self.polling_config();
        polling::acknowledge_message(&config, message_id);
    }

    /// Send heartbeat to the server.
    ///
    /// Registers this hub instance and its active agents with the server.
    /// Delegates to `polling::send_heartbeat_if_due()`.
    pub fn send_heartbeat(&mut self) {
        polling::send_heartbeat_if_due(self);
    }

    /// Poll agents for terminal notifications (OSC 9, OSC 777).
    ///
    /// When agents emit notifications, sends them to Rails for GitHub comments.
    /// Delegates to `polling::poll_and_send_agent_notifications()`.
    pub fn poll_agent_notifications(&mut self) {
        polling::poll_and_send_agent_notifications(self);
    }

    // === Connection Setup ===

    /// Register the device with the server if not already registered.
    pub fn register_device(&mut self) {
        registration::register_device(&mut self.device, &self.client, &self.config);
    }

    /// Register the hub with the server and store the server-assigned ID.
    ///
    /// The server-assigned `botster_id` is used for all URLs and WebSocket subscriptions
    /// to guarantee uniqueness (no collision between different CLI instances).
    /// The local `hub_identifier` is kept for config directories.
    pub fn register_hub_with_server(&mut self) {
        let botster_id = registration::register_hub_with_server(
            &self.hub_identifier,
            &self.config.server_url,
            self.config.get_api_key(),
            self.device.device_id,
        );
        // Store server-assigned ID (used for all server communication)
        self.botster_id = Some(botster_id);
    }

    /// Start the tunnel connection in background.
    pub fn start_tunnel(&self) {
        registration::start_tunnel(&self.tunnel_manager, &self.tokio_runtime);
    }

    /// Connect to terminal relay for browser access (Signal E2E encryption).
    pub fn connect_terminal_relay(&mut self) {
        // Extract values before mutable borrow of browser
        let server_id = self.server_hub_id().to_string();
        let local_id = self.hub_identifier.clone();
        let server_url = self.config.server_url.clone();
        let api_key = self.config.get_api_key().to_string();

        registration::connect_terminal_relay(
            &mut self.browser,
            &server_id,
            &local_id,
            &server_url,
            &api_key,
            &self.tokio_runtime,
        );
    }

    /// Perform all initial setup steps.
    pub fn setup(&mut self) {
        self.register_device();
        self.register_hub_with_server();
        self.start_tunnel();
        self.connect_terminal_relay();

        // Start background workers for non-blocking network I/O
        // Must be called after register_hub_with_server() sets botster_id
        self.start_background_workers();
    }

    // === Event Loop ===

    /// Run the Hub event loop with TUI.
    ///
    /// Delegates to `hub::run::run_event_loop()` for the main loop implementation.
    pub fn run(
        &mut self,
        terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
        shutdown_flag: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        run::run_event_loop(self, terminal, shutdown_flag)
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

    // === Client Communication Helpers ===

    /// Build the agent list for sending to clients.
    fn build_agent_list(&self) -> Vec<AgentInfo> {
        self.state
            .agents
            .iter()
            .map(|(key, agent)| AgentInfo {
                id: key.clone(),
                repo: Some(agent.repo.clone()),
                issue_number: agent.issue_number.map(u64::from),
                branch_name: Some(agent.branch_name.clone()),
                name: None, // Agent doesn't have a separate name field
                status: Some(format!("{:?}", agent.status)),
                tunnel_port: agent.tunnel_port,
                server_running: Some(agent.server_pty.is_some()),
                has_server_pty: Some(agent.server_pty.is_some()),
                active_pty_view: None, // Not tracked at Agent level
                scroll_offset: None,   // Not tracked at Agent level
                hub_identifier: Some(self.hub_identifier.clone()),
            })
            .collect()
    }

    /// Send agent list to a specific client.
    pub fn send_agent_list_to(&mut self, client_id: &ClientId) {
        let agents = self.build_agent_list();
        if let Some(client) = self.clients.get_mut(client_id) {
            client.receive_agent_list(agents);
        }
    }

    /// Send worktree list to a specific client.
    pub fn send_worktree_list_to(&mut self, client_id: &ClientId) {
        // available_worktrees is Vec<(path: String, branch: String)>
        let worktrees = self
            .state
            .available_worktrees
            .iter()
            .map(|(path, branch)| crate::relay::WorktreeInfo {
                path: path.clone(),
                branch: branch.clone(),
                issue_number: None, // Not tracked in tuple format
            })
            .collect();

        if let Some(client) = self.clients.get_mut(client_id) {
            client.receive_worktree_list(worktrees);
        }
    }

    /// Send error response to a specific client.
    pub fn send_error_to(&mut self, client_id: &ClientId, message: String) {
        if let Some(client) = self.clients.get_mut(client_id) {
            client.receive_response(Response::Error { message });
        }
    }

    /// Broadcast agent list to all connected clients.
    pub fn broadcast_agent_list(&mut self) {
        let agents = self.build_agent_list();
        for (_client_id, client) in self.clients.iter_mut() {
            client.receive_agent_list(agents.clone());
        }
    }

    /// Broadcast PTY output to all clients viewing a specific agent.
    ///
    /// Uses the viewer index for O(1) routing - only clients that have
    /// selected this agent will receive the output.
    ///
    /// For browser clients, output is buffered in BrowserClient. Call
    /// `drain_and_send_browser_outputs()` from the event loop to send
    /// buffered output to each browser via relay with per-client targeting.
    ///
    /// For TUI, output is a no-op since TUI reads directly from the agent's PTY.
    pub fn broadcast_pty_output(&mut self, agent_key: &str, data: &[u8]) {
        // Get viewer IDs first to avoid borrow issues
        let viewer_ids: Vec<ClientId> = self.clients.viewers_of(agent_key).cloned().collect();

        // Update client state (for TUI this is no-op, for BrowserClient this buffers)
        for client_id in viewer_ids {
            if let Some(client) = self.clients.get_mut(&client_id) {
                client.receive_output(data);
            }
        }

        // Note: Browser output is buffered above in BrowserClient.receive_output().
        // The event loop calls drain_and_send_browser_outputs() to send per-client.
        // This removes the old broadcast-to-all workaround (Phase 5 complete).
    }

    /// Flush all client output buffers.
    ///
    /// Call this at the end of each event loop iteration to ensure
    /// batched output is sent to browsers.
    pub fn flush_all_clients(&mut self) {
        self.clients.flush_all();
    }

    /// Drain buffered output from all browser clients.
    ///
    /// Returns a vector of (identity, data) tuples for relay sending.
    /// Only includes browsers with buffered output. TUI is excluded.
    ///
    /// This method is used to collect per-client output for targeted relay
    /// sending, enabling proper client isolation (each browser only receives
    /// output from agents it's viewing).
    pub fn drain_browser_outputs(&mut self) -> Vec<(String, Vec<u8>)> {
        let mut outputs = Vec::new();

        for (client_id, client) in self.clients.iter_mut() {
            // Only process browser clients
            if let ClientId::Browser(identity) = client_id {
                // Drain any buffered output
                if let Some(data) = client.drain_buffered_output() {
                    outputs.push((identity.clone(), data));
                }
            }
        }

        outputs
    }

    /// Drain and send browser outputs via relay with per-client targeting.
    ///
    /// This method:
    /// 1. Drains buffered output from each BrowserClient
    /// 2. Sends each client's output via relay to that specific browser
    ///
    /// This enables proper client isolation - each browser only receives
    /// output from agents it's viewing, not from all agents.
    ///
    /// Call this from the event loop to route PTY output to browsers.
    pub fn drain_and_send_browser_outputs(&mut self) {
        let outputs = self.drain_browser_outputs();

        if outputs.is_empty() {
            return;
        }

        let Some(ref sender) = self.browser.sender else {
            return;
        };

        for (identity, data) in outputs {
            let output = String::from_utf8_lossy(&data).to_string();
            let sender = sender.clone();
            let identity_clone = identity.clone();
            self.tokio_runtime.spawn(async move {
                if let Err(e) = sender.send_to(&identity_clone, &output).await {
                    log::error!("Failed to send output to {}: {}", identity_clone, e);
                }
            });
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
            api_key: String::new(),
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
        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        hub.handle_action(HubAction::Resize { rows: 50, cols: 150 });
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
        let tui_client = hub.clients.get(&ClientId::Tui);
        assert!(tui_client.is_some(), "TuiClient should be registered");

        // Initial selection should be None (no agents)
        let tui_selection = tui_client.unwrap().state().selected_agent.clone();
        assert!(tui_selection.is_none(), "TuiClient should have no selection initially");
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
        hub.state.add_agent(agent_key.clone(), agent);

        // Use SelectAgentForClient action (client-scoped)
        hub.handle_action(HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: agent_key.clone(),
        });

        // TuiClient should now have the agent selected
        let tui_client = hub.clients.get(&ClientId::Tui).unwrap();
        let tui_selection = tui_client.state().selected_agent.clone();
        assert_eq!(
            tui_selection,
            Some(agent_key.clone()),
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
            hub.state.add_agent(format!("test-repo-{}", i), agent);
        }

        // Register a browser client
        let browser_client = BrowserClient::new("browser-test-123".to_string());
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

        // Verify they have different selections
        let tui_selection = hub.clients.get(&ClientId::Tui)
            .unwrap()
            .state()
            .selected_agent
            .clone();
        let browser_selection = hub.clients.get(&ClientId::browser("browser-test-123"))
            .unwrap()
            .state()
            .selected_agent
            .clone();

        assert_eq!(tui_selection, Some("test-repo-1".to_string()));
        assert_eq!(browser_selection, Some("test-repo-2".to_string()));
        assert_ne!(tui_selection, browser_selection, "TUI and browser should have independent selections");
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
            hub.state.add_agent(format!("test-repo-{}", i), agent);
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

    // === Phase 3B: Hub drains browser output buffers ===
    //
    // These tests verify that:
    // 1. Hub can drain buffered output from BrowserClients
    // 2. Drained output is returned with browser identity for relay routing
    // 3. TUI client has no buffered output (returns None)

    #[test]
    fn test_hub_drain_browser_outputs_returns_buffered_data() {
        use crate::client::{BrowserClient, ClientId};
        use std::path::PathBuf;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Add a test agent
        let agent = crate::agent::Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(42),
            "botster-issue-42".to_string(),
            PathBuf::from("/tmp/test"),
        );
        let agent_key = "test-repo-42".to_string();
        hub.state.add_agent(agent_key.clone(), agent);

        // Register a browser client and select the agent
        let browser_id = "browser-test-drain".to_string();
        let browser_client = BrowserClient::new(browser_id.clone());
        hub.clients.register(Box::new(browser_client));

        hub.handle_action(HubAction::SelectAgentForClient {
            client_id: ClientId::Browser(browser_id.clone()),
            agent_key: agent_key.clone(),
        });

        // Simulate PTY output via broadcast (this buffers in BrowserClient)
        hub.broadcast_pty_output(&agent_key, b"Hello from PTY!");

        // Drain browser outputs - should get the buffered data with identity
        let outputs = hub.drain_browser_outputs();

        assert_eq!(outputs.len(), 1, "Should have one browser's output");
        let (identity, data) = &outputs[0];
        assert_eq!(identity, &browser_id, "Identity should match browser");
        assert_eq!(data, b"Hello from PTY!", "Data should match what was buffered");

        // Second drain should return empty (buffer was cleared)
        let outputs_after = hub.drain_browser_outputs();
        assert!(outputs_after.is_empty(), "Should be empty after drain");
    }

    #[test]
    fn test_hub_drain_multiple_browsers() {
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
            hub.state.add_agent(format!("test-repo-{}", i), agent);
        }

        // Register two browser clients viewing different agents
        let browser1 = BrowserClient::new("browser-1".to_string());
        let browser2 = BrowserClient::new("browser-2".to_string());
        hub.clients.register(Box::new(browser1));
        hub.clients.register(Box::new(browser2));

        hub.handle_action(HubAction::SelectAgentForClient {
            client_id: ClientId::Browser("browser-1".to_string()),
            agent_key: "test-repo-1".to_string(),
        });
        hub.handle_action(HubAction::SelectAgentForClient {
            client_id: ClientId::Browser("browser-2".to_string()),
            agent_key: "test-repo-2".to_string(),
        });

        // Send different output to each agent
        hub.broadcast_pty_output("test-repo-1", b"Output for agent 1");
        hub.broadcast_pty_output("test-repo-2", b"Output for agent 2");

        // Drain outputs - should get both browsers' data
        let outputs = hub.drain_browser_outputs();

        assert_eq!(outputs.len(), 2, "Should have two browsers' output");

        // Check both outputs are present (order may vary)
        let output_map: std::collections::HashMap<_, _> = outputs.into_iter().collect();
        assert_eq!(
            output_map.get("browser-1"),
            Some(&b"Output for agent 1".to_vec()),
            "Browser 1 should have agent 1's output"
        );
        assert_eq!(
            output_map.get("browser-2"),
            Some(&b"Output for agent 2".to_vec()),
            "Browser 2 should have agent 2's output"
        );
    }

    #[test]
    fn test_hub_drain_does_not_include_tui() {
        use crate::client::{BrowserClient, ClientId};
        use std::path::PathBuf;
        use uuid::Uuid;

        let config = test_config();
        let mut hub = Hub::new(config, TEST_DIMS).unwrap();

        // Add a test agent
        let agent = crate::agent::Agent::new(
            Uuid::new_v4(),
            "test/repo".to_string(),
            Some(42),
            "botster-issue-42".to_string(),
            PathBuf::from("/tmp/test"),
        );
        let agent_key = "test-repo-42".to_string();
        hub.state.add_agent(agent_key.clone(), agent);

        // Select with TUI (TUI client is already registered)
        hub.handle_action(HubAction::SelectAgentForClient {
            client_id: ClientId::Tui,
            agent_key: agent_key.clone(),
        });

        // Register a browser client viewing the same agent
        let browser_client = BrowserClient::new("browser-only".to_string());
        hub.clients.register(Box::new(browser_client));
        hub.handle_action(HubAction::SelectAgentForClient {
            client_id: ClientId::Browser("browser-only".to_string()),
            agent_key: agent_key.clone(),
        });

        // Both TUI and browser are viewing - send output
        hub.broadcast_pty_output(&agent_key, b"Shared output");

        // Drain outputs - should only get browser's data, not TUI
        let outputs = hub.drain_browser_outputs();

        assert_eq!(outputs.len(), 1, "Should only have browser output, not TUI");
        assert_eq!(outputs[0].0, "browser-only");
    }
}
