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

pub use actions::HubAction;
pub use crate::agents::AgentSpawnConfig;
pub use lifecycle::{close_agent, spawn_agent, SpawnResult};
pub use menu::{build_menu, MenuAction, MenuContext, MenuItem};
pub use state::HubState;

use std::sync::Arc;
use std::time::Instant;

use reqwest::blocking::Client;

use crate::app::AppMode;
use crate::config::Config;
use crate::device::Device;
use crate::tunnel::TunnelManager;

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
    /// Browser connection state and communication.
    pub browser: crate::relay::BrowserState,
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
            browser: crate::relay::BrowserState::default(),
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

    /// Build polling configuration from Hub state.
    fn polling_config(&self) -> polling::PollingConfig<'_> {
        polling::PollingConfig {
            client: &self.client,
            server_url: &self.config.server_url,
            api_key: self.config.get_api_key(),
            poll_interval: self.config.poll_interval,
            hub_identifier: &self.hub_identifier,
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

        let (repo_path, repo_name) = match crate::git::WorktreeManager::detect_current_repo() {
            Ok(result) => result,
            Err(e) => {
                log::warn!("Not in a git repository, skipping poll: {e}");
                return;
            }
        };

        let messages = polling::poll_messages(&self.polling_config(), &repo_name);
        if messages.is_empty() {
            return;
        }

        let context = MessageContext {
            repo_path,
            repo_name,
            worktree_base: self.config.worktree_base.clone(),
            max_sessions: self.config.max_sessions,
            current_agent_count: self.state.agent_count(),
        };

        for msg in messages {
            let parsed = ParsedMessage::from_message_data(&msg);

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
    fn try_notify_existing_agent(
        &mut self,
        parsed: &crate::server::messages::ParsedMessage,
        default_repo: &str,
    ) -> bool {
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

    /// Register the hub with the server before connecting to channels.
    pub fn register_hub_with_server(&self) {
        registration::register_hub_with_server(
            &self.hub_identifier,
            &self.config.server_url,
            self.config.get_api_key(),
            self.device.device_id,
        );
    }

    /// Start the tunnel connection in background.
    pub fn start_tunnel(&self) {
        registration::start_tunnel(&self.tunnel_manager, &self.tokio_runtime);
    }

    /// Connect to the terminal relay for browser access.
    pub fn connect_terminal_relay(&mut self) {
        registration::connect_terminal_relay(
            &mut self.browser,
            &self.device.secret_key,
            &self.hub_identifier,
            &self.config.server_url,
            self.config.get_api_key(),
            &self.tokio_runtime,
        );
    }

    /// Perform all initial setup steps.
    pub fn setup(&mut self) {
        self.register_device();
        self.register_hub_with_server();
        self.start_tunnel();
        self.connect_terminal_relay();
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

    /// Send shutdown notification to server.
    pub fn shutdown(&self) {
        registration::shutdown(
            &self.client,
            &self.config.server_url,
            &self.hub_identifier,
            self.config.get_api_key(),
        );
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
