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
pub mod state;

pub use actions::HubAction;
pub use crate::agents::AgentSpawnConfig;
pub use lifecycle::{close_agent, spawn_agent, SpawnResult};
pub use state::HubState;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use reqwest::blocking::Client;
use tokio::sync::mpsc;

use crate::app::AppMode;
use crate::config::Config;
use crate::device::Device;
use crate::relay::connection::{BrowserEvent, BrowserResize, TerminalOutputSender};
use crate::tunnel::TunnelManager;
use crate::BrowserMode;

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
    /// Unique identifier for this hub session.
    pub hub_identifier: String,
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

    // === Browser Relay ===
    /// Terminal output sender for browser communication.
    pub terminal_output_sender: Option<TerminalOutputSender>,
    /// Browser event receiver.
    pub browser_event_rx: Option<mpsc::Receiver<BrowserEvent>>,
    /// Whether a browser is connected.
    pub browser_connected: bool,
    /// Browser terminal dimensions.
    pub browser_dims: Option<BrowserResize>,
    /// Browser display mode (TUI or GUI).
    pub browser_mode: Option<BrowserMode>,

    // === Screen Hashing (bandwidth optimization) ===
    /// Last screen hash per agent (for change detection).
    pub last_agent_screen_hash: HashMap<String, u64>,
    /// Last browser screen hash (for change detection).
    pub last_browser_screen_hash: Option<u64>,
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
        let hub_identifier = uuid::Uuid::new_v4().to_string();
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

        Ok(Self {
            state,
            config,
            client,
            device,
            tunnel_manager,
            hub_identifier,
            tokio_runtime,
            quit: false,
            polling_enabled: true,
            last_poll: Instant::now(),
            last_heartbeat: Instant::now(),
            terminal_dims,
            mode: AppMode::Normal,
            menu_selected: 0,
            input_buffer: String::new(),
            worktree_selected: 0,
            connection_url: None,
            terminal_output_sender: None,
            browser_event_rx: None,
            browser_connected: false,
            browser_dims: None,
            browser_mode: None,
            last_agent_screen_hash: HashMap::new(),
            last_browser_screen_hash: None,
        })
    }

    /// Get the current terminal dimensions.
    #[must_use]
    pub fn terminal_dims(&self) -> (u16, u16) {
        self.terminal_dims
    }

    /// Set terminal dimensions.
    pub fn set_terminal_dims(&mut self, rows: u16, cols: u16) {
        self.terminal_dims = (rows, cols);
    }

    /// Get the number of active agents.
    #[must_use]
    pub fn agent_count(&self) -> usize {
        self.state.agent_count()
    }

    /// Get the currently selected agent.
    #[must_use]
    pub fn selected_agent(&self) -> Option<&crate::agent::Agent> {
        self.state.selected_agent()
    }

    /// Get a mutable reference to the currently selected agent.
    #[must_use]
    pub fn selected_agent_mut(&mut self) -> Option<&mut crate::agent::Agent> {
        self.state.selected_agent_mut()
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
    pub fn handle_action(&mut self, action: HubAction) {
        match action {
            HubAction::Quit => {
                self.quit = true;
            }
            HubAction::SelectNext => {
                self.state.select_next();
            }
            HubAction::SelectPrevious => {
                self.state.select_previous();
            }
            HubAction::SelectByIndex(index) => {
                self.state.select_by_index(index);
            }
            HubAction::SelectByKey(key) => {
                self.state.select_by_key(&key);
            }
            HubAction::TogglePtyView => {
                if let Some(agent) = self.state.selected_agent_mut() {
                    agent.toggle_pty_view();
                }
            }
            HubAction::ScrollUp(lines) => {
                if let Some(agent) = self.state.selected_agent_mut() {
                    agent.scroll_up(lines);
                }
            }
            HubAction::ScrollDown(lines) => {
                if let Some(agent) = self.state.selected_agent_mut() {
                    agent.scroll_down(lines);
                }
            }
            HubAction::ScrollToTop => {
                if let Some(agent) = self.state.selected_agent_mut() {
                    agent.scroll_to_top();
                }
            }
            HubAction::ScrollToBottom => {
                if let Some(agent) = self.state.selected_agent_mut() {
                    agent.scroll_to_bottom();
                }
            }
            HubAction::SendInput(data) => {
                if let Some(agent) = self.state.selected_agent_mut() {
                    if let Err(e) = agent.write_input(&data) {
                        log::error!("Failed to send input to agent: {}", e);
                    }
                }
            }
            HubAction::Resize { rows, cols } => {
                self.terminal_dims = (rows, cols);
                // Resize all agents
                for agent in self.state.agents.values_mut() {
                    agent.resize(rows, cols);
                }
            }
            HubAction::TogglePolling => {
                self.polling_enabled = !self.polling_enabled;
            }

            // === Agent Lifecycle ===
            HubAction::SpawnAgent {
                issue_number,
                branch_name,
                worktree_path,
                repo_path,
                repo_name,
                prompt,
                message_id,
                invocation_url,
            } => {
                let config = crate::agents::AgentSpawnConfig {
                    issue_number,
                    branch_name,
                    worktree_path,
                    repo_path,
                    repo_name,
                    prompt,
                    message_id,
                    invocation_url,
                };
                // Use browser dims if connected, otherwise use local terminal dims
                let dims = self.browser_dims
                    .as_ref()
                    .map(|d| (d.rows, d.cols))
                    .unwrap_or(self.terminal_dims);

                match lifecycle::spawn_agent(&mut self.state, config, dims) {
                    Ok(result) => {
                        log::info!("Spawned agent: {}", result.session_key);
                        // Register tunnel if allocated
                        if let Some(port) = result.tunnel_port {
                            let tm = self.tunnel_manager.clone();
                            let key = result.session_key.clone();
                            self.tokio_runtime.spawn(async move {
                                tm.register_agent(key, port).await;
                            });
                        }
                    }
                    Err(e) => log::error!("Failed to spawn agent: {}", e),
                }
            }

            HubAction::CloseAgent { session_key, delete_worktree } => {
                if let Err(e) = lifecycle::close_agent(&mut self.state, &session_key, delete_worktree) {
                    log::error!("Failed to close agent {}: {}", session_key, e);
                }
            }

            HubAction::KillSelectedAgent => {
                if let Some(key) = self.state.selected_session_key().map(String::from) {
                    if let Err(e) = lifecycle::close_agent(&mut self.state, &key, false) {
                        log::error!("Failed to kill agent: {}", e);
                    }
                }
            }

            // === UI Mode ===
            HubAction::OpenMenu => {
                self.mode = AppMode::Menu;
                self.menu_selected = 0;
            }

            HubAction::CloseModal => {
                self.mode = AppMode::Normal;
                self.input_buffer.clear();
            }

            HubAction::ShowConnectionCode => {
                self.connection_url = Some(format!(
                    "{}/agents/connect#key={}&hub={}",
                    self.config.server_url,
                    self.device.public_key_base64url(),
                    self.hub_identifier
                ));
                self.mode = AppMode::ConnectionCode;
            }

            HubAction::CopyConnectionUrl => {
                if let Some(url) = &self.connection_url {
                    match arboard::Clipboard::new() {
                        Ok(mut clipboard) => {
                            if clipboard.set_text(url.clone()).is_ok() {
                                log::info!("Connection URL copied to clipboard");
                            }
                        }
                        Err(e) => log::warn!("Could not access clipboard: {}", e),
                    }
                }
            }

            // === Menu Navigation ===
            HubAction::MenuUp => {
                if self.menu_selected > 0 {
                    self.menu_selected -= 1;
                }
            }

            HubAction::MenuDown => {
                if self.menu_selected < crate::constants::MENU_ITEMS.len().saturating_sub(1) {
                    self.menu_selected += 1;
                }
            }

            HubAction::MenuSelect(index) => {
                self.handle_menu_select(index);
            }

            // === Worktree Selection ===
            HubAction::WorktreeUp => {
                if self.worktree_selected > 0 {
                    self.worktree_selected -= 1;
                }
            }

            HubAction::WorktreeDown => {
                // +1 for "Create New" option at index 0
                if self.worktree_selected < self.state.available_worktrees.len() {
                    self.worktree_selected += 1;
                }
            }

            HubAction::WorktreeSelect(index) => {
                if index == 0 {
                    self.mode = AppMode::NewAgentCreateWorktree;
                    self.input_buffer.clear();
                } else {
                    self.mode = AppMode::NewAgentPrompt;
                    self.input_buffer.clear();
                }
            }

            // === Text Input ===
            HubAction::InputChar(c) => {
                self.input_buffer.push(c);
            }

            HubAction::InputBackspace => {
                self.input_buffer.pop();
            }

            HubAction::InputSubmit => {
                self.handle_input_submit();
            }

            HubAction::InputClear => {
                self.input_buffer.clear();
            }

            // === Confirmation Dialogs ===
            HubAction::ConfirmCloseAgent => {
                if let Some(key) = self.state.selected_session_key().map(String::from) {
                    let _ = lifecycle::close_agent(&mut self.state, &key, false);
                }
                self.mode = AppMode::Normal;
            }

            HubAction::ConfirmCloseAgentDeleteWorktree => {
                if let Some(key) = self.state.selected_session_key().map(String::from) {
                    let _ = lifecycle::close_agent(&mut self.state, &key, true);
                }
                self.mode = AppMode::Normal;
            }

            HubAction::RefreshWorktrees => {
                if let Err(e) = self.load_available_worktrees() {
                    log::error!("Failed to refresh worktrees: {}", e);
                }
            }

            HubAction::None => {}
        }
    }

    /// Handle menu item selection.
    fn handle_menu_select(&mut self, index: usize) {
        use crate::constants;

        match index {
            constants::MENU_INDEX_TOGGLE_POLLING => {
                self.polling_enabled = !self.polling_enabled;
                self.mode = AppMode::Normal;
            }
            constants::MENU_INDEX_NEW_AGENT => {
                if let Err(e) = self.load_available_worktrees() {
                    log::error!("Failed to load worktrees: {}", e);
                    self.mode = AppMode::Normal;
                } else {
                    self.mode = AppMode::NewAgentSelectWorktree;
                    self.worktree_selected = 0;
                }
            }
            constants::MENU_INDEX_CLOSE_AGENT => {
                if !self.state.agent_keys_ordered.is_empty() {
                    self.mode = AppMode::CloseAgentConfirm;
                } else {
                    self.mode = AppMode::Normal;
                }
            }
            constants::MENU_INDEX_CONNECTION_CODE => {
                self.handle_action(HubAction::ShowConnectionCode);
            }
            _ => {
                self.mode = AppMode::Normal;
            }
        }
    }

    /// Handle input submission based on current mode.
    fn handle_input_submit(&mut self) {
        match self.mode {
            AppMode::NewAgentCreateWorktree => {
                if !self.input_buffer.is_empty() {
                    if let Err(e) = self.create_and_spawn_agent() {
                        log::error!("Failed to create worktree and spawn agent: {}", e);
                    }
                }
            }
            AppMode::NewAgentPrompt => {
                if let Err(e) = self.spawn_agent_from_worktree() {
                    log::error!("Failed to spawn agent: {}", e);
                }
            }
            _ => {}
        }
        self.mode = AppMode::Normal;
        self.input_buffer.clear();
    }

    /// Load available worktrees for the selection UI.
    pub fn load_available_worktrees(&mut self) -> anyhow::Result<()> {
        use std::collections::HashSet;
        use std::process::Command;

        let (repo_path, _) = crate::git::WorktreeManager::detect_current_repo()?;

        let output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repo_path)
            .output()?;

        if !output.status.success() {
            anyhow::bail!(
                "Failed to list worktrees: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let worktree_output = String::from_utf8_lossy(&output.stdout);
        let mut current_path = String::new();
        let mut current_branch = String::new();
        let mut worktrees = Vec::new();

        for line in worktree_output.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                current_path = path.to_string();
            } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                current_branch = branch.to_string();
            } else if line.is_empty() && !current_path.is_empty() {
                worktrees.push((current_path.clone(), current_branch.clone()));
                current_path.clear();
                current_branch.clear();
            }
        }

        if !current_path.is_empty() {
            worktrees.push((current_path, current_branch));
        }

        // Filter out worktrees already in use and the main repository
        let open_paths: HashSet<_> = self
            .state
            .agents
            .values()
            .map(|a| a.worktree_path.display().to_string())
            .collect();

        self.state.available_worktrees = worktrees
            .into_iter()
            .filter(|(path, _)| {
                if open_paths.contains(path) {
                    return false;
                }
                if let Ok(repo) = git2::Repository::open(path) {
                    if !repo.is_worktree() {
                        return false;
                    }
                }
                true
            })
            .collect();

        Ok(())
    }

    /// Spawn an agent from a selected existing worktree.
    fn spawn_agent_from_worktree(&mut self) -> anyhow::Result<()> {
        let worktree_index = self.worktree_selected.saturating_sub(1);

        if let Some((path, branch)) = self.state.available_worktrees.get(worktree_index).cloned() {
            let issue_number = branch
                .strip_prefix("botster-issue-")
                .and_then(|s| s.parse::<u32>().ok());

            let (repo_path, repo_name) = crate::git::WorktreeManager::detect_current_repo()?;
            let worktree_path = std::path::PathBuf::from(&path);

            let prompt = if self.input_buffer.is_empty() {
                issue_number
                    .map(|n| format!("Work on issue #{n}"))
                    .unwrap_or_else(|| format!("Work on {branch}"))
            } else {
                self.input_buffer.clone()
            };

            let config = crate::agents::AgentSpawnConfig {
                issue_number,
                branch_name: branch,
                worktree_path,
                repo_path,
                repo_name,
                prompt,
                message_id: None,
                invocation_url: None,
            };

            let dims = self.browser_dims
                .as_ref()
                .map(|d| (d.rows, d.cols))
                .unwrap_or(self.terminal_dims);

            let result = lifecycle::spawn_agent(&mut self.state, config, dims)?;
            if let Some(port) = result.tunnel_port {
                let tm = self.tunnel_manager.clone();
                let key = result.session_key;
                self.tokio_runtime.spawn(async move {
                    tm.register_agent(key, port).await;
                });
            }
        }

        Ok(())
    }

    /// Create a new worktree and spawn an agent on it.
    fn create_and_spawn_agent(&mut self) -> anyhow::Result<()> {
        let branch_name = self.input_buffer.trim();

        if branch_name.is_empty() {
            anyhow::bail!("Branch name cannot be empty");
        }

        let (issue_number, actual_branch_name) = if let Ok(num) = branch_name.parse::<u32>() {
            (Some(num), format!("botster-issue-{num}"))
        } else {
            (None, branch_name.to_string())
        };

        let (repo_path, repo_name) = crate::git::WorktreeManager::detect_current_repo()?;
        let worktree_path = self.state.git_manager.create_worktree_with_branch(&actual_branch_name)?;

        let prompt = issue_number
            .map(|n| format!("Work on issue #{n}"))
            .unwrap_or_else(|| format!("Work on {actual_branch_name}"));

        let config = crate::agents::AgentSpawnConfig {
            issue_number,
            branch_name: actual_branch_name,
            worktree_path,
            repo_path,
            repo_name,
            prompt,
            message_id: None,
            invocation_url: None,
        };

        let dims = self.browser_dims
            .as_ref()
            .map(|d| (d.rows, d.cols))
            .unwrap_or(self.terminal_dims);

        let result = lifecycle::spawn_agent(&mut self.state, config, dims)?;
        if let Some(port) = result.tunnel_port {
            let tm = self.tunnel_manager.clone();
            let key = result.session_key;
            self.tokio_runtime.spawn(async move {
                tm.register_agent(key, port).await;
            });
        }

        Ok(())
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
    /// This method is non-blocking and respects configured intervals.
    pub fn tick(&mut self) {
        // Poll server for new messages
        self.poll_messages();

        // Send heartbeat to register hub
        self.send_heartbeat();

        // Poll agents for terminal notifications
        self.poll_agent_notifications();
    }

    // === Server Communication ===

    /// Poll the server for new messages and process them.
    ///
    /// This method polls at the configured interval and processes any pending
    /// messages from the server, converting them to HubActions.
    pub fn poll_messages(&mut self) {
        use std::time::Duration;
        use crate::server::types::MessageData;

        // Skip if shutdown requested or polling disabled
        if self.quit || !self.polling_enabled {
            return;
        }

        // Skip if in offline mode
        if std::env::var("BOTSTER_OFFLINE_MODE").is_ok() {
            return;
        }

        // Check poll interval
        if self.last_poll.elapsed() < Duration::from_secs(self.config.poll_interval as u64) {
            return;
        }

        self.last_poll = Instant::now();

        // Detect current repo
        let (repo_path, repo_name) = match crate::git::WorktreeManager::detect_current_repo() {
            Ok(result) => result,
            Err(e) => {
                log::warn!("Not in a git repository, skipping poll: {e}");
                return;
            }
        };

        // Poll the server
        let url = format!("{}/bots/messages?repo={}", self.config.server_url, repo_name);
        let response = match self
            .client
            .get(&url)
            .header("X-API-Key", self.config.get_api_key())
            .send()
        {
            Ok(r) => r,
            Err(e) => {
                log::warn!("Failed to connect to server: {e}");
                return;
            }
        };

        if !response.status().is_success() {
            log::warn!("Failed to poll messages: {}", response.status());
            return;
        }

        #[derive(serde::Deserialize)]
        struct MessageResponse {
            messages: Vec<MessageData>,
        }

        let message_response: MessageResponse = match response.json() {
            Ok(r) => r,
            Err(e) => {
                log::warn!("Failed to parse message response: {e}");
                return;
            }
        };

        if !message_response.messages.is_empty() {
            log::info!("Polled {} pending messages", message_response.messages.len());
        }

        // Process messages using the server/messages module
        let context = crate::server::messages::MessageContext {
            repo_path,
            repo_name,
            worktree_base: self.config.worktree_base.clone(),
            max_sessions: self.config.max_sessions,
            current_agent_count: self.state.agent_count(),
        };

        for msg in message_response.messages {
            let parsed = crate::server::messages::ParsedMessage::from_message_data(&msg);

            // Check if agent already exists for this issue (to send notification instead of spawning)
            if let Some(issue_number) = parsed.issue_number {
                let repo_safe = parsed.repo.as_deref().unwrap_or(&context.repo_name).replace('/', "-");
                let session_key = format!("{repo_safe}-{issue_number}");

                if let Some(existing_agent) = self.state.agents.get_mut(&session_key) {
                    // Agent exists - send notification
                    log::info!("Agent exists for issue #{issue_number}, sending notification");
                    let notification = parsed.format_notification();
                    if let Err(e) = existing_agent.write_input_to_cli(notification.as_bytes()) {
                        log::error!("Failed to send notification to agent: {e}");
                    } else {
                        // Send enters to submit
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        let _ = existing_agent.write_input_to_cli(&[b'\r']);
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        let _ = existing_agent.write_input_to_cli(&[b'\r']);
                    }

                    // Acknowledge the message
                    self.acknowledge_message(msg.id);
                    continue;
                }
            }

            // Convert message to action
            match crate::server::messages::message_to_hub_action(&parsed, &context) {
                Ok(Some(action)) => {
                    self.handle_action(action);
                    self.acknowledge_message(msg.id);
                }
                Ok(None) => {
                    // WebRTC or other non-action message
                    self.acknowledge_message(msg.id);
                }
                Err(e) => {
                    log::error!("Failed to process message {}: {e}", msg.id);
                }
            }
        }
    }

    /// Acknowledge a message to the server.
    fn acknowledge_message(&self, message_id: i64) {
        let url = format!("{}/bots/messages/{message_id}", self.config.server_url);

        match self
            .client
            .patch(&url)
            .header("X-API-Key", self.config.get_api_key())
            .header("Content-Type", "application/json")
            .send()
        {
            Ok(response) if response.status().is_success() => {
                log::debug!("Acknowledged message {message_id}");
            }
            Ok(response) => {
                log::warn!("Failed to acknowledge message {message_id}: {}", response.status());
            }
            Err(e) => {
                log::warn!("Failed to acknowledge message {message_id}: {e}");
            }
        }
    }

    /// Send heartbeat to the server.
    ///
    /// Registers this hub instance and its active agents with the server.
    pub fn send_heartbeat(&mut self) {
        use std::time::Duration;

        // Skip if shutdown requested
        if self.quit {
            return;
        }

        // Skip in offline mode
        if std::env::var("BOTSTER_OFFLINE_MODE").is_ok() {
            return;
        }

        // Check heartbeat interval (30 seconds)
        const HEARTBEAT_INTERVAL: u64 = 30;
        if self.last_heartbeat.elapsed() < Duration::from_secs(HEARTBEAT_INTERVAL) {
            return;
        }
        self.last_heartbeat = Instant::now();

        // Detect current repo
        let repo_name = match crate::git::WorktreeManager::detect_current_repo() {
            Ok((_, name)) => name,
            Err(e) => {
                log::debug!("Not in a git repository, skipping heartbeat: {e}");
                return;
            }
        };

        // Build agents list
        let agents_list: Vec<serde_json::Value> = self
            .state
            .agents
            .values()
            .map(|agent| {
                serde_json::json!({
                    "session_key": agent.session_key(),
                    "last_invocation_url": agent.last_invocation_url,
                })
            })
            .collect();

        let url = format!("{}/api/hubs/{}", self.config.server_url, self.hub_identifier);
        let payload = serde_json::json!({
            "repo": repo_name,
            "agents": agents_list,
            "device_id": self.device.device_id,
        });

        match self
            .client
            .put(&url)
            .header("X-API-Key", self.config.get_api_key())
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
        {
            Ok(response) if response.status().is_success() => {
                log::debug!("Heartbeat sent: {} agents registered", agents_list.len());
            }
            Ok(response) => {
                log::warn!("Heartbeat failed: {}", response.status());
            }
            Err(e) => {
                log::warn!("Failed to send heartbeat: {e}");
            }
        }
    }

    /// Poll agents for terminal notifications (OSC 9, OSC 777).
    ///
    /// When agents emit notifications, sends them to Rails for GitHub comments.
    pub fn poll_agent_notifications(&mut self) {
        use crate::agent::AgentNotification;

        // Collect notifications
        let mut notifications: Vec<(String, String, Option<u32>, Option<String>, String)> = Vec::new();

        for (session_key, agent) in &self.state.agents {
            for notification in agent.poll_notifications() {
                let notification_type = match &notification {
                    AgentNotification::Osc9(_) | AgentNotification::Osc777 { .. } => "question_asked",
                };

                notifications.push((
                    session_key.clone(),
                    agent.repo.clone(),
                    agent.issue_number,
                    agent.last_invocation_url.clone(),
                    notification_type.to_string(),
                ));
            }
        }

        // Send notifications to Rails
        for (session_key, repo, issue_number, invocation_url, notification_type) in notifications {
            if issue_number.is_some() || invocation_url.is_some() {
                log::info!(
                    "Agent {session_key} sent notification: {notification_type} (url: {invocation_url:?})"
                );

                if let Err(e) = self.send_agent_notification(&repo, issue_number, invocation_url.as_deref(), &notification_type) {
                    log::error!("Failed to send notification to Rails: {e}");
                }
            }
        }
    }

    /// Send an agent notification to Rails.
    fn send_agent_notification(
        &self,
        repo: &str,
        issue_number: Option<u32>,
        invocation_url: Option<&str>,
        notification_type: &str,
    ) -> anyhow::Result<()> {
        let url = format!("{}/api/agent_notifications", self.config.server_url);

        let payload = serde_json::json!({
            "repo": repo,
            "issue_number": issue_number,
            "invocation_url": invocation_url,
            "notification_type": notification_type,
        });

        let response = self
            .client
            .post(&url)
            .header("X-API-Key", self.config.get_api_key())
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()?;

        if response.status().is_success() {
            log::info!("Sent notification to Rails: type={notification_type}");
            Ok(())
        } else {
            anyhow::bail!("Failed to send notification: {}", response.status())
        }
    }

    // === Connection Setup ===

    /// Register the device with the server if not already registered.
    ///
    /// This should be called after `new()` to ensure the device identity
    /// is known to the server for browser-based key exchange.
    pub fn register_device(&mut self) {
        if self.device.device_id.is_none() {
            match self.device.register(
                &self.client,
                &self.config.server_url,
                self.config.get_api_key(),
                self.config.server_assisted_pairing,
            ) {
                Ok(id) => log::info!("Device registered with server: id={id}"),
                Err(e) => log::warn!("Device registration failed: {e} - will retry later"),
            }
        }
    }

    /// Register the hub with the server before connecting to channels.
    ///
    /// This creates the Hub record on the server so that the terminal
    /// relay channel can find it when the CLI subscribes.
    pub fn register_hub_with_server(&self) {
        let repo_name = crate::git::WorktreeManager::detect_current_repo()
            .map(|(_, name)| name)
            .unwrap_or_default();

        let url = format!("{}/api/hubs/{}", self.config.server_url, self.hub_identifier);
        let payload = serde_json::json!({
            "repo": repo_name,
            "agents": [],
            "device_id": self.device.device_id,
        });

        log::info!("Registering hub with server before channel connections...");
        match reqwest::blocking::Client::new()
            .put(&url)
            .header("Content-Type", "application/json")
            .header("X-Hub-Identifier", &self.hub_identifier)
            .header("X-API-Key", self.config.get_api_key())
            .json(&payload)
            .send()
        {
            Ok(response) if response.status().is_success() => {
                log::info!("Hub registered successfully");
            }
            Ok(response) => {
                log::warn!("Hub registration returned status: {}", response.status());
            }
            Err(e) => {
                log::warn!("Failed to register hub: {e} - channels may not work");
            }
        }
    }

    /// Start the tunnel connection in background.
    ///
    /// The tunnel provides HTTP forwarding for agent dev servers.
    pub fn start_tunnel(&self) {
        let tunnel_manager = self.tunnel_manager.clone();
        self.tokio_runtime.spawn(async move {
            loop {
                if let Err(e) = tunnel_manager.connect().await {
                    log::warn!("Tunnel connection error: {e}, reconnecting in 5s...");
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        });
    }

    /// Connect to the terminal relay for browser access.
    ///
    /// This establishes an Action Cable WebSocket connection with E2E encryption
    /// for secure browser-based terminal access.
    pub fn connect_terminal_relay(&mut self) {
        use crate::relay::connection::TerminalRelay;

        let relay = TerminalRelay::new(
            self.device.secret_key.clone(),
            self.hub_identifier.clone(),
            self.config.server_url.clone(),
            self.config.get_api_key().to_string(),
        );

        match self.tokio_runtime.block_on(relay.connect()) {
            Ok((sender, rx)) => {
                log::info!("Connected to terminal relay for E2E encrypted browser access");
                self.terminal_output_sender = Some(sender);
                self.browser_event_rx = Some(rx);
            }
            Err(e) => {
                log::warn!("Failed to connect to terminal relay: {e} - browser access disabled");
            }
        }
    }

    /// Perform all initial setup steps.
    ///
    /// This convenience method calls all setup methods in the correct order:
    /// 1. Register device with server
    /// 2. Register hub with server
    /// 3. Start tunnel connection
    /// 4. Connect terminal relay
    pub fn setup(&mut self) {
        self.register_device();
        self.register_hub_with_server();
        self.start_tunnel();
        self.connect_terminal_relay();
    }

    // === Event Loop ===

    /// Run the Hub event loop with TUI.
    ///
    /// This is the main entry point for running the Hub with a terminal UI.
    /// The loop handles:
    /// 1. Keyboard/mouse input → HubActions
    /// 2. Browser events → HubActions
    /// 3. Rendering via tui::render()
    /// 4. Periodic tasks via tick()
    ///
    /// # Arguments
    ///
    /// * `terminal` - The ratatui terminal for rendering
    /// * `shutdown_flag` - Atomic flag for external shutdown requests (signals)
    ///
    /// # Errors
    ///
    /// Returns an error if terminal operations fail.
    pub fn run(
        &mut self,
        terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
        shutdown_flag: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        use std::sync::atomic::Ordering;
        use std::time::Duration;

        use crossterm::event;

        use crate::tui;
        use crate::constants;
        use crate::BrowserDimensions;

        log::info!("Hub event loop starting");

        while !self.quit && !shutdown_flag.load(Ordering::SeqCst) {
            // 1. Handle keyboard/mouse input
            if event::poll(Duration::from_millis(10))? {
                let ev = event::read()?;
                let context = tui::InputContext {
                    terminal_rows: terminal.size()?.height,
                    menu_selected: self.menu_selected,
                    menu_count: constants::MENU_ITEMS.len(),
                    worktree_selected: self.worktree_selected,
                    worktree_count: self.state.available_worktrees.len(),
                };
                if let Some(action) = tui::event_to_hub_action(&ev, &self.mode, &context) {
                    self.handle_action(action);
                }
            }

            // Check quit after input handling
            if self.quit || shutdown_flag.load(Ordering::SeqCst) {
                break;
            }

            // 2. Get browser dimensions for rendering
            let browser_dims: Option<BrowserDimensions> = self.browser_dims.as_ref().map(|dims| {
                BrowserDimensions {
                    cols: dims.cols,
                    rows: dims.rows,
                    mode: crate::BrowserMode::Tui,
                }
            });

            // 3. Handle browser resize
            self.handle_browser_resize(&browser_dims, terminal);

            // 4. Render using tui::render()
            let (ansi_output, _rows, _cols) = tui::render(terminal, self, browser_dims.clone())?;

            // 5. Poll and handle browser events
            self.poll_browser_events(terminal)?;

            // 6. Send output to browser
            self.send_browser_output(&ansi_output);

            // 7. Periodic tasks (polling, heartbeat, notifications)
            self.tick();

            // Small sleep to prevent CPU spinning (60 FPS max)
            std::thread::sleep(Duration::from_millis(16));
        }

        log::info!("Hub event loop exiting");
        Ok(())
    }

    /// Handle browser dimension changes and resize agents accordingly.
    fn handle_browser_resize(
        &mut self,
        browser_dims: &Option<crate::BrowserDimensions>,
        terminal: &ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    ) {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

        static LAST_DIMS: AtomicU32 = AtomicU32::new(0);
        static WAS_CONNECTED: AtomicBool = AtomicBool::new(false);

        let is_connected = browser_dims.is_some();
        let was_connected = WAS_CONNECTED.swap(is_connected, Ordering::Relaxed);

        if let Some(dims) = browser_dims {
            if dims.cols >= 20 && dims.rows >= 5 {
                let mode_bit = if dims.mode == crate::BrowserMode::Gui { 1u32 << 31 } else { 0 };
                let combined = mode_bit | ((dims.cols as u32) << 16) | (dims.rows as u32);
                let last = LAST_DIMS.swap(combined, Ordering::Relaxed);

                if last != combined {
                    let (agent_cols, agent_rows) = match dims.mode {
                        crate::BrowserMode::Gui => {
                            log::info!("GUI mode - using full browser dimensions: {}x{}", dims.cols, dims.rows);
                            (dims.cols, dims.rows)
                        }
                        crate::BrowserMode::Tui => {
                            let tui_cols = (dims.cols * 70 / 100).saturating_sub(2);
                            let tui_rows = dims.rows.saturating_sub(2);
                            log::info!("TUI mode - using 70% width: {}x{} (from {}x{})", tui_cols, tui_rows, dims.cols, dims.rows);
                            (tui_cols, tui_rows)
                        }
                    };
                    for agent in self.state.agents.values() {
                        agent.resize(agent_rows, agent_cols);
                    }
                }
            }
        } else if was_connected {
            log::info!("Browser disconnected, resetting agents to local terminal size");
            let terminal_size = terminal.size().unwrap_or_default();
            let terminal_cols = (terminal_size.width * 70 / 100).saturating_sub(2);
            let terminal_rows = terminal_size.height.saturating_sub(2);
            for agent in self.state.agents.values() {
                agent.resize(terminal_rows, terminal_cols);
            }
            LAST_DIMS.store(0, Ordering::Relaxed);
        }
    }

    /// Poll and handle browser events from the terminal relay.
    fn poll_browser_events(
        &mut self,
        terminal: &ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    ) -> anyhow::Result<()> {
        use crate::app::{dispatch_key_event, parse_terminal_input};
        use crate::constants;

        // Collect events to avoid borrow conflicts
        let browser_events: Vec<BrowserEvent> = if let Some(ref mut rx) = self.browser_event_rx {
            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            events
        } else {
            Vec::new()
        };

        for event in browser_events {
            match event {
                BrowserEvent::Connected { device_name, .. } => {
                    log::info!("Browser connected: {} - E2E encryption active", device_name);
                    self.browser_connected = true;
                    self.browser_mode = Some(crate::BrowserMode::Gui);
                }
                BrowserEvent::Disconnected => {
                    log::info!("Browser disconnected");
                    self.browser_connected = false;
                    self.browser_dims = None;
                }
                BrowserEvent::Input(data) => {
                    // Parse raw terminal input and convert to HubActions
                    let keys = parse_terminal_input(&data);
                    for (code, modifiers) in keys {
                        let input_action = dispatch_key_event(
                            &self.mode,
                            code,
                            modifiers,
                            terminal.size()?.height,
                            self.menu_selected,
                            constants::MENU_ITEMS.len(),
                            self.worktree_selected,
                            self.state.available_worktrees.len(),
                        );
                        // Convert InputAction to HubAction (skip Quit from browser)
                        let hub_action = self.input_action_to_hub_action(input_action);
                        if !matches!(hub_action, HubAction::Quit) {
                            self.handle_action(hub_action);
                        }
                    }
                }
                BrowserEvent::Resize(resize) => {
                    log::info!("Browser resize: {}x{}", resize.cols, resize.rows);
                    self.browser_dims = Some(resize.clone());
                    for agent in self.state.agents.values() {
                        agent.resize(resize.rows, resize.cols);
                    }
                    self.last_browser_screen_hash = None;
                }
                BrowserEvent::SetMode { mode } => {
                    log::info!("Browser set mode: {}", mode);
                    self.browser_mode = if mode == "gui" {
                        Some(crate::BrowserMode::Gui)
                    } else {
                        Some(crate::BrowserMode::Tui)
                    };
                    self.last_browser_screen_hash = None;
                }
                BrowserEvent::ListAgents => {
                    log::info!("Browser requested agent list");
                    self.send_agent_list_to_browser();
                }
                BrowserEvent::ListWorktrees => {
                    log::info!("Browser requested worktree list");
                    self.send_worktree_list_to_browser();
                }
                BrowserEvent::SelectAgent { id } => {
                    log::info!("Browser selected agent: {}", id);
                    self.handle_action(HubAction::SelectByKey(id.clone()));
                    self.last_browser_screen_hash = None;
                    self.send_agent_selected_to_browser(&id);
                }
                BrowserEvent::CreateAgent { issue_or_branch, prompt } => {
                    log::info!("Browser creating agent: {:?}", issue_or_branch);
                    if let Some(input) = issue_or_branch {
                        self.handle_browser_create_agent(&input, prompt);
                    }
                }
                BrowserEvent::ReopenWorktree { path, branch, prompt } => {
                    log::info!("Browser reopening worktree: {} branch: {}", path, branch);
                    self.handle_browser_reopen_worktree(&path, &branch, prompt);
                }
                BrowserEvent::DeleteAgent { id, delete_worktree } => {
                    log::info!("Browser deleting agent: {} delete_worktree: {}", id, delete_worktree);
                    self.handle_action(HubAction::CloseAgent { session_key: id, delete_worktree });
                    self.last_browser_screen_hash = None;
                    self.send_agent_list_to_browser();
                }
                BrowserEvent::TogglePtyView => {
                    self.handle_action(HubAction::TogglePtyView);
                    self.last_browser_screen_hash = None;
                    self.send_agent_list_to_browser();
                }
                BrowserEvent::Scroll { direction, lines } => {
                    match direction.as_str() {
                        "up" => self.handle_action(HubAction::ScrollUp(lines as usize)),
                        "down" => self.handle_action(HubAction::ScrollDown(lines as usize)),
                        _ => {}
                    }
                    self.last_browser_screen_hash = None;
                }
                BrowserEvent::ScrollToBottom => {
                    self.handle_action(HubAction::ScrollToBottom);
                    self.last_browser_screen_hash = None;
                }
                BrowserEvent::ScrollToTop => {
                    self.handle_action(HubAction::ScrollToTop);
                    self.last_browser_screen_hash = None;
                }
            }
        }

        Ok(())
    }

    /// Convert InputAction to HubAction.
    fn input_action_to_hub_action(&self, action: crate::app::InputAction) -> HubAction {
        use crate::app::InputAction;

        match action {
            InputAction::Quit => HubAction::Quit,
            InputAction::OpenMenu => HubAction::OpenMenu,
            InputAction::CloseModal => HubAction::CloseModal,
            InputAction::CopyConnectionUrl => HubAction::CopyConnectionUrl,
            InputAction::PreviousAgent => HubAction::SelectPrevious,
            InputAction::NextAgent => HubAction::SelectNext,
            InputAction::MenuUp => HubAction::MenuUp,
            InputAction::MenuDown => HubAction::MenuDown,
            InputAction::MenuSelect(idx) => HubAction::MenuSelect(idx),
            InputAction::WorktreeUp => HubAction::WorktreeUp,
            InputAction::WorktreeDown => HubAction::WorktreeDown,
            InputAction::WorktreeSelect(idx) => HubAction::WorktreeSelect(idx),
            InputAction::InputChar(c) => HubAction::InputChar(c),
            InputAction::InputBackspace => HubAction::InputBackspace,
            InputAction::InputSubmit => HubAction::InputSubmit,
            InputAction::ForwardToPty(bytes) => HubAction::SendInput(bytes),
            InputAction::KillAgent => HubAction::KillSelectedAgent,
            InputAction::TogglePtyView => HubAction::TogglePtyView,
            InputAction::CloseAgentKeepWorktree => HubAction::ConfirmCloseAgent,
            InputAction::CloseAgentDeleteWorktree => HubAction::ConfirmCloseAgentDeleteWorktree,
            InputAction::ScrollUp(n) => HubAction::ScrollUp(n),
            InputAction::ScrollDown(n) => HubAction::ScrollDown(n),
            InputAction::ScrollToTop => HubAction::ScrollToTop,
            InputAction::ScrollToBottom => HubAction::ScrollToBottom,
            InputAction::None => HubAction::None,
        }
    }

    // === Browser Communication ===

    /// Send agent list to browser.
    fn send_agent_list_to_browser(&self) {
        use crate::{AgentInfo, TerminalMessage};

        let Some(ref sender) = self.terminal_output_sender else {
            return;
        };

        let agents: Vec<AgentInfo> = self.state.agent_keys_ordered.iter()
            .filter_map(|key| self.state.agents.get(key).map(|agent| (key, agent)))
            .map(|(id, agent)| AgentInfo {
                id: id.clone(),
                repo: Some(agent.repo.clone()),
                issue_number: agent.issue_number.map(|n| n as u64),
                branch_name: Some(agent.branch_name.clone()),
                name: None,
                status: Some(format!("{:?}", agent.status)),
                tunnel_port: agent.tunnel_port,
                server_running: Some(agent.is_server_running()),
                has_server_pty: Some(agent.has_server_pty()),
                active_pty_view: Some(format!("{:?}", agent.active_pty).to_lowercase()),
                scroll_offset: Some(agent.get_scroll_offset() as u32),
                hub_identifier: Some(self.hub_identifier.clone()),
            })
            .collect();

        let message = TerminalMessage::Agents { agents };
        if let Ok(json) = serde_json::to_string(&message) {
            let sender = sender.clone();
            self.tokio_runtime.spawn(async move {
                let _ = sender.send(&json).await;
            });
        }
    }

    /// Send worktree list to browser.
    fn send_worktree_list_to_browser(&self) {
        use crate::{TerminalMessage, WorktreeInfo, WorktreeManager};

        let Some(ref sender) = self.terminal_output_sender else {
            return;
        };

        let worktrees: Vec<WorktreeInfo> = self.state.available_worktrees.iter()
            .map(|(path, branch)| {
                let issue_number = branch.strip_prefix("botster-issue-")
                    .and_then(|s| s.parse::<u64>().ok());
                WorktreeInfo {
                    path: path.clone(),
                    branch: branch.clone(),
                    issue_number,
                }
            })
            .collect();

        let repo = WorktreeManager::detect_current_repo()
            .map(|(_, name)| name)
            .ok();

        let message = TerminalMessage::Worktrees { worktrees, repo };
        if let Ok(json) = serde_json::to_string(&message) {
            let sender = sender.clone();
            self.tokio_runtime.spawn(async move {
                let _ = sender.send(&json).await;
            });
        }
    }

    /// Send selected agent notification to browser.
    fn send_agent_selected_to_browser(&self, agent_id: &str) {
        use crate::TerminalMessage;

        let Some(ref sender) = self.terminal_output_sender else {
            return;
        };

        let msg = TerminalMessage::AgentSelected { id: agent_id.to_string() };
        if let Ok(json) = serde_json::to_string(&msg) {
            let sender = sender.clone();
            self.tokio_runtime.spawn(async move {
                let _ = sender.send(&json).await;
            });
        }
    }

    /// Handle browser create agent request.
    fn handle_browser_create_agent(&mut self, input: &str, prompt: Option<String>) {
        use crate::WorktreeManager;

        let branch_name = input.trim();
        if branch_name.is_empty() {
            return;
        }

        let (issue_number, actual_branch_name) = if let Ok(num) = branch_name.parse::<u32>() {
            (Some(num), format!("botster-issue-{}", num))
        } else {
            (None, branch_name.to_string())
        };

        let (repo_path, repo_name) = match WorktreeManager::detect_current_repo() {
            Ok(result) => result,
            Err(e) => {
                log::error!("Failed to detect repo: {}", e);
                return;
            }
        };

        let worktree_path = match self.state.git_manager.create_worktree_with_branch(&actual_branch_name) {
            Ok(path) => path,
            Err(e) => {
                log::error!("Failed to create worktree: {}", e);
                return;
            }
        };

        let final_prompt = prompt.unwrap_or_else(|| {
            issue_number
                .map(|num| format!("Work on issue #{}", num))
                .unwrap_or_else(|| format!("Work on {}", actual_branch_name))
        });

        self.handle_action(HubAction::SpawnAgent {
            issue_number,
            branch_name: actual_branch_name,
            worktree_path,
            repo_path,
            repo_name,
            prompt: final_prompt,
            message_id: None,
            invocation_url: None,
        });

        self.last_browser_screen_hash = None;
        self.send_agent_list_to_browser();

        // Select newly created agent
        if let Some(key) = self.state.agent_keys_ordered.last().cloned() {
            self.state.selected = self.state.agent_keys_ordered.len() - 1;
            self.send_agent_selected_to_browser(&key);
        }
    }

    /// Handle browser reopen worktree request.
    fn handle_browser_reopen_worktree(&mut self, path: &str, branch: &str, prompt: Option<String>) {
        use crate::WorktreeManager;

        let issue_number = branch.strip_prefix("botster-issue-")
            .and_then(|s| s.parse::<u32>().ok());

        let (repo_path, repo_name) = match WorktreeManager::detect_current_repo() {
            Ok(result) => result,
            Err(e) => {
                log::error!("Failed to detect repo: {}", e);
                return;
            }
        };

        let final_prompt = prompt.unwrap_or_else(|| {
            issue_number
                .map(|num| format!("Work on issue #{}", num))
                .unwrap_or_else(|| format!("Work on {}", branch))
        });

        self.handle_action(HubAction::SpawnAgent {
            issue_number,
            branch_name: branch.to_string(),
            worktree_path: std::path::PathBuf::from(path),
            repo_path,
            repo_name,
            prompt: final_prompt,
            message_id: None,
            invocation_url: None,
        });

        self.last_browser_screen_hash = None;
        self.send_agent_list_to_browser();

        if let Some(key) = self.state.agent_keys_ordered.last().cloned() {
            self.state.selected = self.state.agent_keys_ordered.len() - 1;
            self.send_agent_selected_to_browser(&key);
        }
    }

    /// Send output to browser via E2E encrypted relay.
    fn send_browser_output(&self, ansi_output: &str) {
        if !self.browser_connected {
            return;
        }

        let Some(ref sender) = self.terminal_output_sender else {
            return;
        };

        // Determine what output to send based on browser mode
        let output_to_send = match self.browser_mode {
            Some(crate::BrowserMode::Gui) => {
                // GUI mode: send only selected agent's PTY output
                self.state.selected_agent()
                    .map(|agent| agent.get_screen_as_ansi())
                    .unwrap_or_else(|| String::from("\x1b[2J\x1b[HNo agent selected"))
            }
            Some(crate::BrowserMode::Tui) | None => {
                // TUI mode: send full hub TUI output
                ansi_output.to_string()
            }
        };

        let sender = sender.clone();
        let output = output_to_send;
        self.tokio_runtime.spawn(async move {
            if let Err(e) = sender.send(&output).await {
                log::warn!("Failed to send output to browser: {}", e);
            }
        });
    }

    /// Send shutdown notification to server.
    ///
    /// Call this when the hub is shutting down to unregister from the server.
    pub fn shutdown(&self) {
        log::info!("Sending shutdown notification to server...");
        let shutdown_url = format!("{}/api/hubs/{}", self.config.server_url, self.hub_identifier);

        match self.client.delete(&shutdown_url)
            .header("X-API-Key", self.config.get_api_key())
            .send()
        {
            Ok(response) if response.status().is_success() => {
                log::info!("Hub unregistered from server");
            }
            Ok(response) => {
                log::warn!("Failed to unregister hub: {}", response.status());
            }
            Err(e) => {
                log::warn!("Failed to send shutdown notification: {e}");
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
            api_key: String::new(),
            poll_interval: 10,
            agent_timeout: 300,
            max_sessions: 10,
            worktree_base: PathBuf::from("/tmp/test-worktrees"),
            server_assisted_pairing: false,
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
}
