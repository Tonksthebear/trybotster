use anyhow::Result;
use botster_hub::{
    agents, allocate_tunnel_port, app, commands, constants, kill_orphaned_processes,
    render_agent_terminal, Agent, AgentNotification, BrowserCommand, BrowserDimensions,
    BrowserMode, Config, IceServerConfig, PtyView, TunnelManager, TunnelStatus, WebAgentInfo,
    WebRTCHandler, WebWorktreeInfo, WorktreeManager,
};
use app::{buffer_to_ansi, centered_rect, convert_browser_key_to_crossterm, InputAction};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{CrosstermBackend, TestBackend},
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame, Terminal,
};
use reqwest::blocking::Client;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

/// Version constant re-exported from commands module.
use commands::update::VERSION;

/// Global flag for signal-triggered shutdown (as Arc for signal-hook compatibility)
static SHUTDOWN_FLAG: std::sync::LazyLock<Arc<AtomicBool>> =
    std::sync::LazyLock::new(|| Arc::new(AtomicBool::new(false)));

/// Check if shutdown has been requested
fn shutdown_requested() -> bool {
    SHUTDOWN_FLAG.load(Ordering::SeqCst)
}

/// Re-export AgentSpawnConfig from agents module
type AgentSpawnConfig = agents::AgentSpawnConfig;

// AppMode is imported from app::state module
use app::AppMode;

struct BotsterApp {
    agents: HashMap<String, Agent>, // Key: session_key (repo-safe-issue_number or repo-safe-branch)
    agent_keys_ordered: Vec<String>, // Ordered list of agent keys for UI navigation
    selected: usize,
    config: Config,
    git_manager: WorktreeManager,
    client: Client,
    quit: bool,
    last_poll: Instant,
    terminal_rows: u16,
    terminal_cols: u16,
    mode: AppMode,
    menu_selected: usize,
    polling_enabled: bool,
    input_buffer: String,
    available_worktrees: Vec<(String, String)>, // (path, branch)
    worktree_selected: usize,
    // WebRTC P2P support for browser connections
    tokio_runtime: tokio::runtime::Runtime,
    webrtc_handler: Arc<StdMutex<WebRTCHandler>>,
    // Track last agent screen hash for change detection (reduces bandwidth)
    last_agent_screen_hash: HashMap<String, u64>,
    // Hub tracking - per-session UUID for identifying this CLI instance
    hub_identifier: String,
    // Track when we last sent a heartbeat to the server
    last_heartbeat: Instant,
    // HTTP tunnel manager for forwarding local dev servers
    tunnel_manager: Arc<TunnelManager>,
}

impl BotsterApp {
    fn new(terminal_rows: u16, terminal_cols: u16) -> Result<Self> {
        let config = Config::load()?;
        let git_manager = WorktreeManager::new(config.worktree_base.clone());
        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        // Create tokio runtime for async WebRTC operations
        let tokio_runtime = tokio::runtime::Runtime::new()?;

        // Create WebRTC handler for full TUI streaming
        let webrtc_handler = Arc::new(StdMutex::new(WebRTCHandler::new()));

        // Generate per-session UUID for hub identification
        let hub_identifier = uuid::Uuid::new_v4().to_string();
        log::info!("Generated hub identifier: {}", hub_identifier);

        // Create tunnel manager for HTTP forwarding
        let tunnel_manager = Arc::new(TunnelManager::new(
            hub_identifier.clone(),
            config.api_key.clone(),
            config.server_url.clone(),
        ));

        // Start tunnel connection in background
        let tunnel_manager_clone = tunnel_manager.clone();
        tokio_runtime.spawn(async move {
            loop {
                if let Err(e) = tunnel_manager_clone.connect().await {
                    log::warn!("Tunnel connection error: {}, reconnecting in 5s...", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        });

        let app = Self {
            agents: HashMap::new(),
            agent_keys_ordered: Vec::new(),
            selected: 0,
            config,
            git_manager,
            client,
            quit: false,
            last_poll: Instant::now(),
            terminal_rows,
            terminal_cols,
            mode: AppMode::Normal,
            menu_selected: 0,
            polling_enabled: true,
            input_buffer: String::new(),
            available_worktrees: Vec::new(),
            worktree_selected: 0,
            tokio_runtime,
            webrtc_handler,
            last_agent_screen_hash: HashMap::new(),
            hub_identifier,
            last_heartbeat: Instant::now(),
            tunnel_manager,
        };

        log::info!("Botster Hub started, waiting for messages...");

        Ok(app)
    }

    /// Shared helper to spawn an agent with common configuration
    /// This consolidates duplicate code from spawn_agent_from_worktree and create_and_spawn_agent
    fn spawn_agent_with_config(&mut self, config: AgentSpawnConfig) -> Result<()> {
        let id = uuid::Uuid::new_v4();
        let mut agent = Agent::new(
            id,
            config.repo_name.clone(),
            config.issue_number,
            config.branch_name.clone(),
            config.worktree_path.clone(),
        );
        agent.resize(self.terminal_rows, self.terminal_cols);

        // Set invocation URL for notifications
        // Use provided URL, or construct from repo + issue_number if available
        agent.last_invocation_url = config.invocation_url.or_else(|| {
            config.issue_number.map(|num| {
                format!("https://github.com/{}/issues/{}", config.repo_name, num)
            })
        });
        if let Some(ref url) = agent.last_invocation_url {
            log::info!("Agent invocation URL: {}", url);
        }

        // Write prompt to .botster_prompt file
        let prompt_file_path = config.worktree_path.join(".botster_prompt");
        std::fs::write(&prompt_file_path, &config.prompt)?;

        // Copy fresh .botster_init from main repo to worktree
        let source_init = config.repo_path.join(".botster_init");
        let dest_init = config.worktree_path.join(".botster_init");
        if source_init.exists() {
            std::fs::copy(&source_init, &dest_init)?;
        }

        // Build environment variables
        let mut env_vars = HashMap::new();
        env_vars.insert("BOTSTER_REPO".to_string(), config.repo_name.clone());
        env_vars.insert(
            "BOTSTER_ISSUE_NUMBER".to_string(),
            config.issue_number
                .map(|n| n.to_string())
                .unwrap_or_else(|| "0".to_string()),
        );
        env_vars.insert("BOTSTER_BRANCH_NAME".to_string(), config.branch_name.clone());
        env_vars.insert(
            "BOTSTER_WORKTREE_PATH".to_string(),
            config.worktree_path.display().to_string(),
        );
        env_vars.insert("BOTSTER_TASK_DESCRIPTION".to_string(), config.prompt.clone());

        if let Some(msg_id) = config.message_id {
            env_vars.insert("BOTSTER_MESSAGE_ID".to_string(), msg_id.to_string());
        }

        let bin_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "botster-hub".to_string());
        env_vars.insert("BOTSTER_HUB_BIN".to_string(), bin_path);

        // Allocate a tunnel port for this agent
        let tunnel_port = allocate_tunnel_port();
        if let Some(port) = tunnel_port {
            env_vars.insert("BOTSTER_TUNNEL_PORT".to_string(), port.to_string());
            log::info!("Allocated tunnel port {} for agent", port);
        }

        // Kill any existing orphaned claude processes for this worktree
        // This prevents duplicate claude instances when reopening a worktree
        kill_orphaned_processes(&config.worktree_path);

        // Spawn the agent
        let init_commands = vec!["source .botster_init".to_string()];
        agent.spawn("bash", "", init_commands, env_vars.clone())?;

        // Store tunnel port on the agent
        agent.tunnel_port = tunnel_port;

        // Spawn server PTY if tunnel port is allocated and .botster_server exists
        if let Some(port) = tunnel_port {
            let server_script = config.worktree_path.join(".botster_server");
            if server_script.exists() {
                log::info!("Spawning server PTY on port {} using .botster_server", port);
                let mut server_env = HashMap::new();
                server_env.insert("BOTSTER_TUNNEL_PORT".to_string(), port.to_string());
                server_env.insert(
                    "BOTSTER_WORKTREE_PATH".to_string(),
                    config.worktree_path.display().to_string(),
                );
                // Spawn server in its own PTY (uses bash + source, like CLI PTY)
                if let Err(e) = agent.spawn_server_pty(".botster_server", server_env) {
                    log::warn!("Failed to spawn server PTY: {}", e);
                }
            }
        }

        // Register the agent
        let session_key = agent.session_key();
        let has_tunnel = tunnel_port.is_some();

        self.agent_keys_ordered.push(session_key.clone());
        self.agents.insert(session_key.clone(), agent);

        let label = if let Some(num) = config.issue_number {
            format!("issue #{}", num)
        } else {
            format!("branch {}", config.branch_name)
        };
        log::info!("Spawned agent for {}", label);

        // For tunnel agents, send heartbeat FIRST to ensure Hub exists in Rails
        // before the tunnel manager tries to register the agent
        if has_tunnel {
            log::debug!("Sending immediate heartbeat for tunnel agent");
            self.last_heartbeat = Instant::now() - Duration::from_secs(60);
            if let Err(e) = self.send_heartbeat() {
                log::warn!("Failed to send pre-tunnel heartbeat: {}", e);
            }
        }

        // Register tunnel port with the tunnel manager (AFTER heartbeat creates the Hub)
        if let Some(port) = tunnel_port {
            let tunnel_manager = self.tunnel_manager.clone();
            let session_key_clone = session_key.clone();
            self.tokio_runtime.spawn(async move {
                tunnel_manager.register_agent(session_key_clone, port).await;
            });
        }

        Ok(())
    }

    fn handle_events(&mut self) -> Result<bool> {
        let mut handled_any = false;

        // Process ALL pending events (not just one) to prevent event queue buildup
        while event::poll(Duration::from_millis(0))? {
            handled_any = true;
            match event::read()? {
                Event::Resize(cols, rows) => {
                    // Calculate terminal widget dimensions
                    let terminal_cols = (cols * 70 / 100).saturating_sub(2);
                    let terminal_rows = rows.saturating_sub(2);

                    // Update stored dimensions
                    self.terminal_rows = terminal_rows;
                    self.terminal_cols = terminal_cols;

                    // Resize all agents
                    for agent in self.agents.values() {
                        agent.resize(terminal_rows, terminal_cols);
                    }
                }
                Event::Key(key) => {
                    self.handle_key_event(key)?;
                }
                Event::Mouse(mouse) => {
                    self.handle_mouse_event(mouse)?;
                }
                _ => {}
            }
        }

        Ok(handled_any)
    }

    fn handle_mouse_event(&mut self, mouse: crossterm::event::MouseEvent) -> Result<bool> {
        use crossterm::event::MouseEventKind;

        // Only handle mouse events in normal mode
        if self.mode != AppMode::Normal {
            return Ok(true);
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                // Scroll up in scrollback buffer
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.get_mut(&key) {
                        agent.scroll_up(3); // Scroll 3 lines per mouse wheel tick
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                // Scroll down in scrollback buffer
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.get_mut(&key) {
                        agent.scroll_down(3); // Scroll 3 lines per mouse wheel tick
                    }
                }
            }
            _ => {}
        }

        Ok(true)
    }

    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        use crossterm::event::KeyEventKind;

        // Only process key press events (not release or repeat)
        if key.kind != KeyEventKind::Press {
            return Ok(true);
        }

        // Dispatch to input module and get action
        let action = app::dispatch_key_event(
            &self.mode,
            key.code,
            key.modifiers,
            self.terminal_rows,
            self.menu_selected,
            constants::MENU_ITEMS.len(),
            self.worktree_selected,
            self.available_worktrees.len(),
        );

        // Process the action
        self.process_input_action(action)
    }

    /// Process an input action and apply the corresponding state changes.
    fn process_input_action(&mut self, action: InputAction) -> Result<bool> {
        match action {
            InputAction::None => {}
            InputAction::Quit => {
                self.quit = true;
            }
            InputAction::OpenMenu => {
                self.mode = AppMode::Menu;
                self.menu_selected = 0;
            }
            InputAction::CloseModal => {
                self.mode = AppMode::Normal;
                self.input_buffer.clear();
            }
            InputAction::PreviousAgent => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            InputAction::NextAgent => {
                if self.selected < self.agent_keys_ordered.len().saturating_sub(1) {
                    self.selected += 1;
                }
            }
            InputAction::KillAgent => {
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    self.agents.remove(&key);
                    self.agent_keys_ordered.remove(self.selected);
                    if self.selected >= self.agent_keys_ordered.len() && self.selected > 0 {
                        self.selected = self.agent_keys_ordered.len() - 1;
                    }
                }
            }
            InputAction::TogglePtyView => {
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.get_mut(&key) {
                        agent.toggle_pty_view();
                    }
                }
            }
            InputAction::ScrollUp(lines) => {
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.get_mut(&key) {
                        agent.scroll_up(lines);
                    }
                }
            }
            InputAction::ScrollDown(lines) => {
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.get_mut(&key) {
                        agent.scroll_down(lines);
                    }
                }
            }
            InputAction::ScrollToTop => {
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.get_mut(&key) {
                        agent.scroll_to_top();
                    }
                }
            }
            InputAction::ScrollToBottom => {
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.get_mut(&key) {
                        agent.scroll_to_bottom();
                    }
                }
            }
            InputAction::ForwardToPty(bytes) => {
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.get_mut(&key) {
                        agent.scroll_to_bottom();
                        let _ = agent.write_to_active_pty(&bytes);
                    }
                }
            }
            InputAction::MenuUp => {
                if self.menu_selected > 0 {
                    self.menu_selected -= 1;
                }
            }
            InputAction::MenuDown => {
                if self.menu_selected < constants::MENU_ITEMS.len() - 1 {
                    self.menu_selected += 1;
                }
            }
            InputAction::MenuSelect(index) => {
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
                        if !self.agent_keys_ordered.is_empty() {
                            self.mode = AppMode::CloseAgentConfirm;
                        } else {
                            self.mode = AppMode::Normal;
                        }
                    }
                    _ => {
                        self.mode = AppMode::Normal;
                    }
                }
            }
            InputAction::WorktreeUp => {
                if self.worktree_selected > 0 {
                    self.worktree_selected -= 1;
                }
            }
            InputAction::WorktreeDown => {
                if self.worktree_selected < self.available_worktrees.len() {
                    self.worktree_selected += 1;
                }
            }
            InputAction::WorktreeSelect(index) => {
                if index == 0 {
                    self.mode = AppMode::NewAgentCreateWorktree;
                    self.input_buffer.clear();
                } else {
                    self.mode = AppMode::NewAgentPrompt;
                    self.input_buffer.clear();
                }
            }
            InputAction::InputChar(c) => {
                self.input_buffer.push(c);
            }
            InputAction::InputBackspace => {
                self.input_buffer.pop();
            }
            InputAction::InputSubmit => {
                // Handle based on current mode
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
            InputAction::CloseAgentKeepWorktree => {
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.remove(&key) {
                        self.agent_keys_ordered.remove(self.selected);
                        if self.selected >= self.agent_keys_ordered.len() && self.selected > 0 {
                            self.selected = self.agent_keys_ordered.len() - 1;
                        }
                        let label = if let Some(num) = agent.issue_number {
                            format!("issue #{}", num)
                        } else {
                            format!("branch {}", agent.branch_name)
                        };
                        log::info!("Closed agent for {}", label);
                    }
                }
                self.mode = AppMode::Normal;
            }
            InputAction::CloseAgentDeleteWorktree => {
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.remove(&key) {
                        self.agent_keys_ordered.remove(self.selected);
                        if self.selected >= self.agent_keys_ordered.len() && self.selected > 0 {
                            self.selected = self.agent_keys_ordered.len() - 1;
                        }
                        if let Err(e) = self
                            .git_manager
                            .delete_worktree_by_path(&agent.worktree_path, &agent.branch_name)
                        {
                            let label = if let Some(num) = agent.issue_number {
                                format!("issue #{}", num)
                            } else {
                                format!("branch {}", agent.branch_name)
                            };
                            log::error!("Failed to delete worktree for {}: {}", label, e);
                        } else {
                            let label = if let Some(num) = agent.issue_number {
                                format!("issue #{}", num)
                            } else {
                                format!("branch {}", agent.branch_name)
                            };
                            log::info!("Closed agent and deleted worktree for {}", label);
                        }
                    }
                }
                self.mode = AppMode::Normal;
            }
        }
        Ok(true)
    }

    fn load_available_worktrees(&mut self) -> Result<()> {
        use std::process::Command;

        let (repo_path, _) = WorktreeManager::detect_current_repo()?;

        let output = Command::new("git")
            .args(&["worktree", "list", "--porcelain"])
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
            if line.starts_with("worktree ") {
                current_path = line.strip_prefix("worktree ").unwrap_or("").to_string();
            } else if line.starts_with("branch ") {
                current_branch = line
                    .strip_prefix("branch refs/heads/")
                    .unwrap_or("")
                    .to_string();
            } else if line.is_empty() && !current_path.is_empty() {
                worktrees.push((current_path.clone(), current_branch.clone()));
                current_path.clear();
                current_branch.clear();
            }
        }

        if !current_path.is_empty() {
            worktrees.push((current_path, current_branch));
        }

        // Filter out:
        // 1. The main repository (not a worktree, can't be deleted)
        // 2. Worktrees that already have agents open
        let open_paths: std::collections::HashSet<_> = self
            .agents
            .values()
            .map(|a| a.worktree_path.display().to_string())
            .collect();

        self.available_worktrees = worktrees
            .into_iter()
            .filter(|(path, _)| {
                // Filter out worktrees already in use
                if open_paths.contains(path) {
                    return false;
                }

                // Filter out the main repository - check if it's actually a worktree
                if let Ok(repo) = git2::Repository::open(path) {
                    if !repo.is_worktree() {
                        log::info!("Filtering out main repository from worktree list: {}", path);
                        return false;
                    }
                }

                true
            })
            .collect();

        Ok(())
    }

    fn spawn_agent_from_worktree(&mut self) -> Result<()> {
        // Adjust for "Create New" option at index 0
        let worktree_index = self.worktree_selected.saturating_sub(1);

        if let Some((path, branch)) = self.available_worktrees.get(worktree_index).cloned() {
            // Extract issue number from branch name if it follows botster-issue-N format
            let issue_number = if let Some(num_str) = branch.strip_prefix("botster-issue-") {
                num_str.parse::<u32>().ok()
            } else {
                None
            };

            let (repo_path, repo_name) = WorktreeManager::detect_current_repo()?;
            let worktree_path = std::path::PathBuf::from(&path);

            let prompt = if self.input_buffer.is_empty() {
                if let Some(issue_num) = issue_number {
                    format!("Work on issue #{}", issue_num)
                } else {
                    format!("Work on {}", branch)
                }
            } else {
                self.input_buffer.clone()
            };

            self.spawn_agent_with_config(AgentSpawnConfig {
                issue_number,
                branch_name: branch,
                worktree_path,
                repo_path,
                repo_name,
                prompt,
                message_id: None,
                invocation_url: None, // Will be auto-constructed from repo + issue_number
            })?;
        }

        Ok(())
    }

    fn create_and_spawn_agent(&mut self) -> Result<()> {
        let branch_name = self.input_buffer.trim();

        if branch_name.is_empty() {
            anyhow::bail!("Branch name cannot be empty");
        }

        // Try to parse as issue number, otherwise treat as custom branch name
        let (issue_number, actual_branch_name) = if let Ok(num) = branch_name.parse::<u32>() {
            (Some(num), format!("botster-issue-{}", num))
        } else {
            (None, branch_name.to_string())
        };

        let (repo_path, repo_name) = WorktreeManager::detect_current_repo()?;

        // Create worktree with custom branch name
        let worktree_path = self
            .git_manager
            .create_worktree_with_branch(&actual_branch_name)?;

        let prompt = if let Some(num) = issue_number {
            format!("Work on issue #{}", num)
        } else {
            format!("Work on {}", actual_branch_name)
        };

        self.spawn_agent_with_config(AgentSpawnConfig {
            issue_number,
            branch_name: actual_branch_name,
            worktree_path,
            repo_path,
            repo_name,
            prompt,
            message_id: None,
            invocation_url: None, // Will be auto-constructed from repo + issue_number
        })
    }

    /// Render the TUI and return ANSI output for WebRTC streaming
    /// Returns (ansi_string, rows, cols) for sending to connected browsers
    /// If browser_dims is provided, renders at those dimensions for proper layout
    fn view(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
        browser_dims: Option<BrowserDimensions>,
    ) -> Result<(String, u16, u16)> {
        // Collect all state needed for rendering
        let agent_keys_ordered = self.agent_keys_ordered.clone();
        let agents = &self.agents;
        let selected = self.selected;
        let seconds_since_poll = self.last_poll.elapsed().as_secs();
        let poll_interval = self.config.poll_interval;
        let mode = self.mode.clone();
        let polling_enabled = self.polling_enabled;
        let menu_selected = self.menu_selected;
        let available_worktrees = self.available_worktrees.clone();
        let worktree_selected = self.worktree_selected;
        let input_buffer = self.input_buffer.clone();
        let tunnel_status = self.tunnel_manager.get_status();

        // Helper to render UI to a frame
        let render_ui = |f: &mut Frame, agents: &HashMap<String, Agent>| {
            use ratatui::{
                layout::Alignment,
                text::{Line, Span},
                widgets::{Clear, Paragraph, Wrap},
            };

            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
                .split(f.area());

            // Render agent list
            let items: Vec<ListItem> = agent_keys_ordered
                .iter()
                .filter_map(|key| agents.get(key))
                .map(|agent| {
                    let base_text = if let Some(issue_num) = agent.issue_number {
                        format!("{}#{}", agent.repo, issue_num)
                    } else {
                        format!("{}/{}", agent.repo, agent.branch_name)
                    };

                    // Add server status indicator if tunnel port is assigned
                    let server_info = if let Some(port) = agent.tunnel_port {
                        let server_icon = if agent.is_server_running() {
                            "▶" // Server running
                        } else {
                            "○" // Server not running
                        };
                        format!(" {}:{}", server_icon, port)
                    } else {
                        String::new()
                    };

                    ListItem::new(format!("{}{}", base_text, server_info))
                })
                .collect();

            let mut state = ListState::default();
            state.select(Some(
                selected.min(agent_keys_ordered.len().saturating_sub(1)),
            ));

            // Add polling indicator
            let poll_status = if !polling_enabled {
                "PAUSED"
            } else if seconds_since_poll < 1 {
                "●"
            } else {
                "○"
            };

            // Add tunnel status indicator
            let tunnel_indicator = match tunnel_status {
                TunnelStatus::Connected => "⬤",    // Filled circle = connected
                TunnelStatus::Connecting => "◐",   // Half circle = connecting
                TunnelStatus::Disconnected => "○", // Empty circle = disconnected
            };

            let agent_title = format!(
                " Agents ({}) {} {}s T:{} ",
                agent_keys_ordered.len(),
                poll_status,
                if polling_enabled {
                    poll_interval - seconds_since_poll.min(poll_interval)
                } else {
                    0
                },
                tunnel_indicator
            );

            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(agent_title))
                .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
                .highlight_symbol("> ");

            f.render_stateful_widget(list, chunks[0], &mut state);

            // Render terminal view using the extracted render function
            // This ensures we test the exact same code path
            let selected_agent = agent_keys_ordered
                .get(selected)
                .and_then(|key| agents.get(key));
            if let Some(agent) = selected_agent {
                render_agent_terminal(agent, chunks[1], f.buffer_mut());
            }

            // Render modal overlays based on mode
            match mode {
                AppMode::Menu => {
                    let menu_items = vec![
                        format!(
                            "{} {} ({})",
                            if menu_selected == constants::MENU_INDEX_TOGGLE_POLLING { ">" } else { " " },
                            constants::MENU_ITEMS[constants::MENU_INDEX_TOGGLE_POLLING],
                            if polling_enabled { "ON" } else { "OFF" }
                        ),
                        format!(
                            "{} {}",
                            if menu_selected == constants::MENU_INDEX_NEW_AGENT { ">" } else { " " },
                            constants::MENU_ITEMS[constants::MENU_INDEX_NEW_AGENT]
                        ),
                        format!(
                            "{} {}",
                            if menu_selected == constants::MENU_INDEX_CLOSE_AGENT { ">" } else { " " },
                            constants::MENU_ITEMS[constants::MENU_INDEX_CLOSE_AGENT]
                        ),
                    ];

                    let area = centered_rect(
                        constants::MENU_MODAL_WIDTH_PERCENT,
                        constants::MENU_MODAL_HEIGHT_PERCENT,
                        f.area(),
                    );
                    f.render_widget(Clear, area);

                    let menu_text: Vec<Line> = menu_items
                        .iter()
                        .map(|item| Line::from(item.clone()))
                        .collect();

                    let menu = Paragraph::new(menu_text)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(" Menu [↑/↓ navigate | Enter select | Esc cancel] "),
                        )
                        .alignment(Alignment::Left);

                    f.render_widget(menu, area);
                }
                AppMode::NewAgentSelectWorktree => {
                    let mut worktree_items: Vec<String> = vec![format!(
                        "{} [Create New Worktree]",
                        if worktree_selected == 0 { ">" } else { " " }
                    )];

                    // Add existing worktrees (index offset by 1)
                    for (i, (path, branch)) in available_worktrees.iter().enumerate() {
                        worktree_items.push(format!(
                            "{} {} ({})",
                            if i + 1 == worktree_selected { ">" } else { " " },
                            branch,
                            path
                        ));
                    }

                    let area = centered_rect(70, 50, f.area());
                    f.render_widget(Clear, area);

                    let worktree_text: Vec<Line> = worktree_items
                        .iter()
                        .map(|item| Line::from(item.clone()))
                        .collect();

                    let worktree_list =
                        Paragraph::new(worktree_text)
                            .block(Block::default().borders(Borders::ALL).title(
                                " Select Worktree [↑/↓ navigate | Enter select | Esc cancel] ",
                            ))
                            .alignment(Alignment::Left)
                            .wrap(Wrap { trim: false });

                    f.render_widget(worktree_list, area);
                }
                AppMode::NewAgentCreateWorktree => {
                    let area = centered_rect(60, 30, f.area());
                    f.render_widget(Clear, area);

                    let prompt_text = vec![
                        Line::from("Enter branch name or issue number:"),
                        Line::from(""),
                        Line::from("Examples: 123, feature-auth, bugfix-login"),
                        Line::from(""),
                        Line::from(Span::raw(input_buffer.clone())),
                    ];

                    let prompt_widget = Paragraph::new(prompt_text)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(" Create Worktree [Enter confirm | Esc cancel] "),
                        )
                        .alignment(Alignment::Left);

                    f.render_widget(prompt_widget, area);
                }
                AppMode::NewAgentPrompt => {
                    let area = centered_rect(60, 20, f.area());
                    f.render_widget(Clear, area);

                    let prompt_text = vec![
                        Line::from("Enter prompt for agent (leave empty for default):"),
                        Line::from(""),
                        Line::from(Span::raw(input_buffer.clone())),
                    ];

                    let prompt_widget = Paragraph::new(prompt_text)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(" Agent Prompt [Enter confirm | Esc cancel] "),
                        )
                        .alignment(Alignment::Left);

                    f.render_widget(prompt_widget, area);
                }
                AppMode::CloseAgentConfirm => {
                    let area = centered_rect(50, 20, f.area());
                    f.render_widget(Clear, area);

                    let confirm_text = vec![
                        Line::from("Close selected agent?"),
                        Line::from(""),
                        Line::from("Y - Close agent (keep worktree)"),
                        Line::from("D - Close agent and delete worktree"),
                        Line::from("N/Esc - Cancel"),
                    ];

                    let confirm_widget = Paragraph::new(confirm_text)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(" Confirm Close "),
                        )
                        .alignment(Alignment::Left);

                    f.render_widget(confirm_widget, area);
                }
                AppMode::Normal => {}
            }
        };

        // Always render to real terminal for local display
        terminal.draw(|f| render_ui(f, agents))?;

        // For WebRTC streaming, render to browser-sized buffer if dimensions provided
        let (ansi_output, out_rows, out_cols) = if let Some(dims) = browser_dims {
            // Create a virtual terminal at browser dimensions
            let backend = TestBackend::new(dims.cols, dims.rows);
            let mut virtual_terminal = Terminal::new(backend)?;

            // Render to virtual terminal at browser dimensions
            let completed_frame = virtual_terminal.draw(|f| {
                // Log once when dimensions change
                static LAST_AREA: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let area = f.area();
                let combined = ((area.width as u32) << 16) | (area.height as u32);
                let last = LAST_AREA.swap(combined, std::sync::atomic::Ordering::Relaxed);
                if last != combined {
                    log::info!("Virtual terminal rendering at {}x{}", area.width, area.height);
                }
                render_ui(f, agents)
            })?;

            // Convert virtual buffer to ANSI
            let ansi = buffer_to_ansi(
                &completed_frame.buffer,
                dims.cols,
                dims.rows,
                None, // No clipping needed, already at correct size
                None,
            );
            (ansi, dims.rows, dims.cols)
        } else {
            // No browser connected, return empty output
            (String::new(), 0, 0)
        };

        Ok((ansi_output, out_rows, out_cols))
    }

    fn poll_messages(&mut self) -> Result<()> {
        // Skip if shutdown requested (prevents blocking during exit)
        if shutdown_requested() {
            return Ok(());
        }

        // Skip polling if disabled (or BOTSTER_OFFLINE_MODE env var is set)
        if !self.polling_enabled || std::env::var("BOTSTER_OFFLINE_MODE").is_ok() {
            return Ok(());
        }

        // Poll every N seconds
        if self.last_poll.elapsed() < Duration::from_secs(self.config.poll_interval as u64) {
            return Ok(());
        }

        self.last_poll = Instant::now();

        // Detect current repo for filtering
        let (_, repo_name) = match WorktreeManager::detect_current_repo() {
            Ok(result) => result,
            Err(e) => {
                log::warn!("Not in a git repository, skipping poll: {}", e);
                return Ok(());
            }
        };

        // Poll the Rails endpoint with repo filter
        let url = format!(
            "{}/bots/messages?repo={}",
            self.config.server_url, repo_name
        );
        let response = match self
            .client
            .get(&url)
            .header("X-API-Key", &self.config.api_key)
            .send()
        {
            Ok(r) => r,
            Err(e) => {
                log::warn!("Failed to connect to server: {}", e);
                return Ok(());
            }
        };

        if !response.status().is_success() {
            log::warn!("Failed to poll messages: {}", response.status());
            return Ok(());
        }

        #[derive(serde::Deserialize)]
        struct MessageResponse {
            messages: Vec<MessageData>,
        }

        #[derive(serde::Deserialize)]
        struct MessageData {
            id: i64,
            event_type: String,
            payload: serde_json::Value,
        }

        let message_response: MessageResponse = match response.json() {
            Ok(r) => r,
            Err(e) => {
                log::warn!("Failed to parse message response: {}", e);
                return Ok(());
            }
        };

        log::info!(
            "Polled {} pending messages",
            message_response.messages.len()
        );

        // Spawn agents for new messages
        for msg in message_response.messages {
            if let Err(e) = self.spawn_agent_for_message(msg.id, &msg.payload, &msg.event_type) {
                log::error!(
                    "Failed to process message {} ({}): {}",
                    msg.id,
                    msg.event_type,
                    e
                );
                // TODO: Mark message as failed
            } else {
                // Acknowledge message to trigger eyes reaction on GitHub
                if let Err(e) = self.acknowledge_message(msg.id) {
                    log::warn!("Failed to acknowledge message {}: {}", msg.id, e);
                } else {
                    log::info!(
                        "Successfully processed and acknowledged message {} ({})",
                        msg.id,
                        msg.event_type
                    );
                }
            }
        }

        Ok(())
    }

    /// Acknowledge a message to the Rails server.
    /// This triggers the server to add an eyes emoji reaction on GitHub,
    /// providing visual feedback to the user that the bot saw their message.
    fn acknowledge_message(&self, message_id: i64) -> Result<()> {
        let url = format!("{}/bots/messages/{}", self.config.server_url, message_id);

        let response = self
            .client
            .patch(&url)
            .header("X-API-Key", &self.config.api_key)
            .header("Content-Type", "application/json")
            .send()?;

        if response.status().is_success() {
            log::debug!("Acknowledged message {}", message_id);
            Ok(())
        } else {
            anyhow::bail!(
                "Failed to acknowledge message {}: {}",
                message_id,
                response.status()
            )
        }
    }

    /// Poll all agents for terminal notifications (OSC 9, OSC 777)
    /// and send them to Rails to trigger GitHub comments.
    /// OSC 777 notifications (sent natively by Claude Code) are treated as "question_asked"
    /// to alert the user that the agent needs attention.
    fn poll_agent_notifications(&mut self) {
        // Collect notifications: (session_key, repo, issue_number, invocation_url, notification)
        let mut notifications_to_send: Vec<(String, String, Option<u32>, Option<String>, AgentNotification)> =
            Vec::new();

        for (session_key, agent) in &self.agents {
            let notifications = agent.poll_notifications();
            for notification in notifications {
                notifications_to_send.push((
                    session_key.clone(),
                    agent.repo.clone(),
                    agent.issue_number,
                    agent.last_invocation_url.clone(),
                    notification,
                ));
            }
        }

        // Process and send notifications
        for (session_key, repo, issue_number, invocation_url, notification) in notifications_to_send {
            // Claude Code sends OSC 9 notifications when it needs user attention
            // (see: https://github.com/anthropics/claude-code/issues/3340)
            // We treat both OSC 9 and OSC 777 as "question_asked" for a generic message
            let notification_type = match &notification {
                AgentNotification::Osc9(_) | AgentNotification::Osc777 { .. } => {
                    "question_asked".to_string()
                }
            };

            if issue_number.is_some() || invocation_url.is_some() {
                log::info!(
                    "Agent {} sent notification: {} (url: {:?})",
                    session_key,
                    notification_type,
                    invocation_url
                );

                if let Err(e) = self.send_agent_notification(&repo, issue_number, invocation_url.as_deref(), &notification_type) {
                    log::error!("Failed to send notification to Rails: {}", e);
                }
            } else {
                log::debug!(
                    "Agent {} detected notification '{}' but has no issue_number or invocation_url - skipping",
                    session_key,
                    notification_type
                );
            }
        }
    }

    /// Send an agent notification to Rails to trigger a GitHub comment
    /// Prefers invocation_url if available, falls back to repo + issue_number
    fn send_agent_notification(
        &self,
        repo: &str,
        issue_number: Option<u32>,
        invocation_url: Option<&str>,
        notification_type: &str,
    ) -> Result<()> {
        let url = format!("{}/api/agent_notifications", self.config.server_url);

        // Build payload - include both old and new fields for backwards compatibility
        let payload = serde_json::json!({
            "repo": repo,
            "issue_number": issue_number,
            "invocation_url": invocation_url,
            "notification_type": notification_type,
        });

        let response = self
            .client
            .post(&url)
            .header("X-API-Key", &self.config.api_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()?;

        if response.status().is_success() {
            log::info!(
                "Sent notification to Rails: repo={}, issue={:?}, url={:?}, type={}",
                repo,
                issue_number,
                invocation_url,
                notification_type
            );
            Ok(())
        } else {
            anyhow::bail!(
                "Failed to send notification: {} - {}",
                response.status(),
                response.text().unwrap_or_default()
            )
        }
    }

    /// Send heartbeat to Rails server to register this hub and its agents
    /// Uses RESTful PUT /api/hubs/:identifier endpoint for upsert
    fn send_heartbeat(&mut self) -> Result<()> {
        // Skip if shutdown requested (prevents blocking during exit)
        if shutdown_requested() {
            return Ok(());
        }

        // Skip heartbeat in offline mode
        if std::env::var("BOTSTER_OFFLINE_MODE").is_ok() {
            return Ok(());
        }

        // Check if 30 seconds have passed since last heartbeat
        const HEARTBEAT_INTERVAL_SECS: u64 = 30;
        if self.last_heartbeat.elapsed() < Duration::from_secs(HEARTBEAT_INTERVAL_SECS) {
            return Ok(());
        }
        self.last_heartbeat = Instant::now();

        // Detect current repo
        let (_, repo_name) = match WorktreeManager::detect_current_repo() {
            Ok(result) => result,
            Err(e) => {
                log::debug!("Not in a git repository, skipping heartbeat: {}", e);
                return Ok(());
            }
        };

        // Build agents list for the heartbeat payload
        let agents_list: Vec<serde_json::Value> = self
            .agents
            .values()
            .map(|agent| {
                serde_json::json!({
                    "session_key": agent.session_key(),
                    "last_invocation_url": agent.last_invocation_url,
                })
            })
            .collect();

        let url = format!(
            "{}/api/hubs/{}",
            self.config.server_url, self.hub_identifier
        );

        let payload = serde_json::json!({
            "repo": repo_name,
            "agents": agents_list,
        });

        log::debug!("Sending heartbeat to {}", url);

        match self
            .client
            .put(&url)
            .header("X-API-Key", &self.config.api_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
        {
            Ok(response) => {
                if response.status().is_success() {
                    log::debug!(
                        "Heartbeat sent successfully: {} agents registered",
                        agents_list.len()
                    );
                } else {
                    log::warn!(
                        "Heartbeat failed: {} - {}",
                        response.status(),
                        response.text().unwrap_or_default()
                    );
                }
            }
            Err(e) => {
                log::warn!("Failed to send heartbeat: {}", e);
            }
        }

        Ok(())
    }

    fn spawn_agent_for_message(
        &mut self,
        message_id: i64,
        payload: &serde_json::Value,
        event_type: &str,
    ) -> Result<()> {
        // Handle cleanup messages (when issue/PR is closed)
        if event_type == "agent_cleanup" {
            return self.handle_cleanup_message(payload);
        }

        // Handle WebRTC signaling for P2P browser connections
        if event_type == "webrtc_offer" {
            return self.handle_webrtc_offer(payload);
        }

        // Enforce max_sessions limit to prevent unbounded memory growth
        if self.agents.len() >= self.config.max_sessions {
            log::warn!(
                "Max sessions limit ({}) reached. Cannot spawn new agent. Current agents: {}",
                self.config.max_sessions,
                self.agents.len()
            );
            anyhow::bail!(
                "Maximum concurrent sessions ({}) reached. Close some agents before spawning new ones.",
                self.config.max_sessions
            );
        }

        // Extract data from payload
        let issue_number = payload["issue_number"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Missing issue_number in payload"))?
            as u32;

        // Detect current repo
        let (repo_path, repo_name) = WorktreeManager::detect_current_repo()?;

        // Generate session key to check if agent already exists
        let repo_safe = repo_name.replace('/', "-");
        let session_key = format!("{}-{}", repo_safe, issue_number);

        // Check if an agent already exists for this issue
        if let Some(existing_agent) = self.agents.get_mut(&session_key) {
            // Agent exists - ping it with the new message
            log::info!(
                "Agent already exists for issue #{}, pinging with new message",
                issue_number
            );

            // Update last_invocation_url to track where this interaction came from
            // This ensures notifications go to the right place (issue, PR, etc.)
            if let Some(issue_url) = payload["issue_url"].as_str() {
                existing_agent.last_invocation_url = Some(issue_url.to_string());
                log::info!("Updated last_invocation_url to: {}", issue_url);
            }

            // Use the full prompt which includes routing information (where to respond)
            // This ensures the agent knows if the comment came from a PR and should respond there
            let full_prompt = payload["prompt"]
                .as_str()
                .or_else(|| payload["context"].as_str());

            let notification = if let Some(prompt) = full_prompt {
                // Use the full structured prompt which includes respond_to info
                format!(
                    "=== NEW MENTION (automated notification) ===\n\n{}\n\n==================",
                    prompt
                )
            } else {
                // Fallback to basic notification if no structured prompt
                let comment_body = payload["comment_body"].as_str().unwrap_or("New mention");
                let comment_author = payload["comment_author"].as_str().unwrap_or("unknown");
                format!(
                    "=== NEW MENTION (automated notification) ===\n{} mentioned you: {}\n==================",
                    comment_author, comment_body
                )
            };

            existing_agent.write_input_str(&notification)?;

            // Wait a bit for the text to be processed
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Send two Enters (first goes to new line, second submits on empty line)
            existing_agent.write_input(&[b'\r'])?;
            std::thread::sleep(std::time::Duration::from_millis(50));
            existing_agent.write_input(&[b'\r'])?;

            log::info!(
                "Sent notification to existing agent for issue #{}",
                issue_number
            );

            return Ok(());
        }

        // No existing in-memory agent - check if a worktree already exists
        // Get the user's task description from the payload
        let task_description = payload["prompt"]
            .as_str()
            .or_else(|| payload["comment_body"].as_str())
            .or_else(|| payload["context"].as_str())
            .unwrap_or("Work on this issue")
            .to_string();

        // Check for existing worktree first
        let (worktree_path, is_existing_worktree) = if let Ok(Some((existing_path, _branch))) = self.git_manager.find_existing_worktree_for_issue(issue_number) {
            log::info!(
                "Found existing worktree for issue #{}, reusing at {}",
                issue_number,
                existing_path.display()
            );
            (existing_path, true)
        } else {
            // Create a new git worktree from the current repo
            log::info!("No existing worktree for issue #{}, creating new one", issue_number);
            (self.git_manager.create_worktree_from_current(issue_number)?, false)
        };

        // For existing worktrees, append the new message to .botster_prompt
        // This preserves context from previous work while adding the new task
        let prompt_file_path = worktree_path.join(".botster_prompt");
        if is_existing_worktree {
            // Append new message to existing prompt file
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&prompt_file_path)?;

            writeln!(file, "\n\n---\n## New Message\n")?;
            writeln!(file, "{}", task_description)?;

            log::info!("Appended new message to existing .botster_prompt");
        } else {
            // New worktree - write initial prompt
            std::fs::write(&prompt_file_path, &task_description)?;
            log::info!("Created new .botster_prompt file");
        }

        // Copy fresh .botster_init from main repo to worktree
        // This ensures we always use the latest init script
        let source_init = repo_path.join(".botster_init");
        let dest_init = worktree_path.join(".botster_init");
        if source_init.exists() {
            std::fs::copy(&source_init, &dest_init)?;
            log::info!("Copied .botster_init from main repo to worktree");
        }

        // Kill any existing orphaned claude processes for this worktree
        // (Currently just logs diagnostics without killing)
        kill_orphaned_processes(&worktree_path);

        let id = uuid::Uuid::new_v4();
        let mut agent = Agent::new(
            id,
            repo_name.clone(),
            Some(issue_number),
            format!("botster-issue-{}", issue_number),
            worktree_path.clone(),
        );

        // Set last_invocation_url from payload to track where this agent was invoked from
        // Fall back to constructing URL from repo + issue_number if not provided
        if let Some(issue_url) = payload["issue_url"].as_str() {
            agent.last_invocation_url = Some(issue_url.to_string());
            log::info!("Set last_invocation_url from payload: {}", issue_url);
        } else {
            // Construct URL from repo and issue number
            let constructed_url = format!("https://github.com/{}/issues/{}", repo_name, issue_number);
            agent.last_invocation_url = Some(constructed_url.clone());
            log::info!("Constructed last_invocation_url: {}", constructed_url);
        }

        // Resize agent to match terminal dimensions
        agent.resize(self.terminal_rows, self.terminal_cols);

        // Create environment variables for the agent
        let mut env_vars = HashMap::new();
        // Set TERM to ensure Claude Code sends OSC 777 notifications
        env_vars.insert("TERM".to_string(), "xterm-256color".to_string());
        env_vars.insert("BOTSTER_REPO".to_string(), repo_name.clone());
        env_vars.insert("BOTSTER_ISSUE_NUMBER".to_string(), issue_number.to_string());
        env_vars.insert(
            "BOTSTER_BRANCH_NAME".to_string(),
            format!("botster-issue-{}", issue_number),
        );
        env_vars.insert(
            "BOTSTER_WORKTREE_PATH".to_string(),
            worktree_path.display().to_string(),
        );
        env_vars.insert(
            "BOTSTER_TASK_DESCRIPTION".to_string(),
            task_description.clone(),
        );
        env_vars.insert("BOTSTER_MESSAGE_ID".to_string(), message_id.to_string());

        // Add path to botster-hub binary for use in init scripts
        let bin_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "botster-hub".to_string());
        env_vars.insert("BOTSTER_HUB_BIN".to_string(), bin_path);

        // Allocate a tunnel port for this agent
        let tunnel_port = allocate_tunnel_port();
        if let Some(port) = tunnel_port {
            env_vars.insert("BOTSTER_TUNNEL_PORT".to_string(), port.to_string());
            log::info!("Allocated tunnel port {} for agent", port);
        }

        // Spawn agent with a shell
        // Just run 'source .botster_init' which handles everything:
        // - Reading .botster_prompt file
        // - Setting up MCP servers
        // - Starting Claude with the prompt
        let init_commands = vec!["source .botster_init".to_string()];
        agent.spawn("bash", "", init_commands, env_vars)?;

        log::info!("Spawned agent {} for issue #{}", id, issue_number);

        // Add agent to tracking structures using session key
        let session_key = agent.session_key();
        let has_tunnel = tunnel_port.is_some();

        // Store tunnel port on the agent
        agent.tunnel_port = tunnel_port;

        self.agent_keys_ordered.push(session_key.clone());
        self.agents.insert(session_key.clone(), agent);

        // For tunnel agents, send heartbeat FIRST to ensure Hub exists in Rails
        // before the tunnel manager tries to register the agent
        if has_tunnel {
            log::debug!("Sending immediate heartbeat for tunnel agent (from message)");
            self.last_heartbeat = Instant::now() - Duration::from_secs(60);
            if let Err(e) = self.send_heartbeat() {
                log::warn!("Failed to send pre-tunnel heartbeat: {}", e);
            }
        }

        // Register tunnel port with the tunnel manager (AFTER heartbeat creates the Hub)
        if let Some(port) = tunnel_port {
            let tunnel_manager = self.tunnel_manager.clone();
            let session_key_clone = session_key.clone();
            self.tokio_runtime.spawn(async move {
                tunnel_manager.register_agent(session_key_clone, port).await;
            });
        }

        Ok(())
    }

    /// Build the agent list for sending to WebRTC browsers
    fn build_web_agent_list(&self) -> Vec<WebAgentInfo> {
        let hub_identifier = self.hub_identifier.clone();
        self.agent_keys_ordered
            .iter()
            .enumerate()
            .filter_map(|(idx, key)| {
                self.agents.get(key).map(|agent| WebAgentInfo {
                    id: key.clone(),
                    repo: agent.repo.clone(),
                    issue_number: agent.issue_number,
                    branch_name: agent.branch_name.clone(),
                    status: format!("{:?}", agent.status),
                    selected: idx == self.selected,
                    tunnel_port: agent.tunnel_port,
                    hub_identifier: hub_identifier.clone(),
                    server_running: agent.is_server_running(),
                    has_server_pty: agent.has_server_pty(),
                    active_pty_view: match agent.active_pty {
                        PtyView::Cli => "cli".to_string(),
                        PtyView::Server => "server".to_string(),
                    },
                    scroll_offset: agent.get_scroll_offset(),
                })
            })
            .collect()
    }

    /// Close an agent by session key, optionally deleting the worktree
    fn close_agent(&mut self, session_key: &str, delete_worktree: bool) -> Result<()> {
        if let Some(agent) = self.agents.remove(session_key) {
            // Remove from ordered list
            if let Some(pos) = self.agent_keys_ordered.iter().position(|k| k == session_key) {
                self.agent_keys_ordered.remove(pos);

                // Adjust selection if needed
                if self.selected >= self.agent_keys_ordered.len() && self.selected > 0 {
                    self.selected = self.agent_keys_ordered.len() - 1;
                }
            }

            let label = if let Some(num) = agent.issue_number {
                format!("issue #{}", num)
            } else {
                format!("branch {}", agent.branch_name)
            };

            if delete_worktree {
                if let Err(e) = self
                    .git_manager
                    .delete_worktree_by_path(&agent.worktree_path, &agent.branch_name)
                {
                    log::error!("Failed to delete worktree for {}: {}", label, e);
                } else {
                    log::info!("Closed agent and deleted worktree for {}", label);
                }
            } else {
                log::info!("Closed agent for {} (worktree preserved)", label);
            }
        }
        Ok(())
    }

    fn handle_cleanup_message(&mut self, payload: &serde_json::Value) -> Result<()> {
        let repo = payload["repo"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing repo in cleanup payload"))?;
        let issue_number = payload["issue_number"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Missing issue_number in cleanup payload"))?
            as u32;
        let reason = payload["reason"].as_str().unwrap_or("closed");

        // Generate session key
        let repo_safe = repo.replace('/', "-");
        let session_key = format!("{}-{}", repo_safe, issue_number);

        log::info!(
            "Processing cleanup for {}#{} (reason: {})",
            repo,
            issue_number,
            reason
        );

        // Check if agent exists
        if let Some(agent) = self.agents.remove(&session_key) {
            // Remove from ordered list
            if let Some(pos) = self
                .agent_keys_ordered
                .iter()
                .position(|k| k == &session_key)
            {
                self.agent_keys_ordered.remove(pos);

                // Adjust selection if needed
                if self.selected >= self.agent_keys_ordered.len() && self.selected > 0 {
                    self.selected = self.agent_keys_ordered.len() - 1;
                }
            }

            // Delete the worktree
            if let Err(e) = self
                .git_manager
                .delete_worktree_by_path(&agent.worktree_path, &agent.branch_name)
            {
                log::error!(
                    "Failed to delete worktree for {}#{}: {}",
                    repo,
                    issue_number,
                    e
                );
            } else {
                log::info!(
                    "Closed agent and deleted worktree for {}#{} (reason: {})",
                    repo,
                    issue_number,
                    reason
                );
            }
        } else {
            log::info!(
                "No active agent found for {}#{}, skipping cleanup",
                repo,
                issue_number
            );
        }

        Ok(())
    }

    /// Handle a WebRTC offer from a browser client (via Rails signaling)
    /// Creates a peer connection, generates an answer, and posts it back to Rails
    fn handle_webrtc_offer(&mut self, payload: &serde_json::Value) -> Result<()> {
        let session_id = payload["session_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing session_id in webrtc_offer payload"))?;

        let offer_sdp = payload["offer"]["sdp"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing offer.sdp in webrtc_offer payload"))?;

        // Parse ICE servers from payload (provided by Rails signaling server)
        let ice_servers: Vec<IceServerConfig> = payload
            .get("ice_servers")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_else(|| {
                log::warn!("No ice_servers in payload - using default STUN servers");
                Vec::new()
            });

        log::info!(
            "Handling WebRTC offer for session {}, offer length: {} bytes, {} ICE server(s)",
            session_id,
            offer_sdp.len(),
            ice_servers.len()
        );

        // Create the answer using the WebRTC handler (async operation)
        let webrtc_handler = Arc::clone(&self.webrtc_handler);
        let offer_sdp_owned = offer_sdp.to_string();

        // Use timeout to prevent blocking if WebRTC handler hangs
        let answer_sdp = self.tokio_runtime.block_on(async move {
            match tokio::time::timeout(
                std::time::Duration::from_secs(10),
                async {
                    let mut handler = webrtc_handler.lock().unwrap();
                    handler.handle_offer(&offer_sdp_owned, &ice_servers).await
                }
            ).await {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("WebRTC offer handling timed out")),
            }
        })?;

        log::info!(
            "WebRTC answer created, length: {} bytes",
            answer_sdp.len()
        );

        // Post the answer back to the Rails signaling server
        let url = format!(
            "{}/api/webrtc/sessions/{}",
            self.config.server_url, session_id
        );

        let response = self
            .client
            .patch(&url)
            .header("X-API-Key", &self.config.api_key)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "answer": {
                    "type": "answer",
                    "sdp": answer_sdp
                }
            }))
            .send()?;

        if response.status().is_success() {
            log::info!(
                "Successfully posted WebRTC answer for session {}",
                session_id
            );
        } else {
            log::error!(
                "Failed to post WebRTC answer for session {}: {}",
                session_id,
                response.status()
            );
        }

        Ok(())
    }
}

/// Guard struct that ensures terminal cleanup on drop (including panics)
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Always attempt to restore terminal state
        let _ = disable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        // Try to show cursor
        let _ = execute!(std::io::stdout(), crossterm::cursor::Show);
    }
}

fn run_interactive() -> Result<()> {
    // Set up signal handlers for clean shutdown (Ctrl+C/SIGINT, SIGTERM, SIGHUP)
    // Using signal-hook's flag API for reliable signal handling in PTY environments
    use signal_hook::consts::signal::*;
    use signal_hook::flag;

    // Register signal handlers that directly set the SHUTDOWN_FLAG
    // This is more reliable than using a separate thread, especially in forked processes
    flag::register(SIGINT, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGTERM, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGHUP, Arc::clone(&SHUTDOWN_FLAG))?;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    // Create guard AFTER setup - it will cleanup on drop (including panics)
    let _terminal_guard = TerminalGuard;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Get initial terminal size and calculate widget dimensions
    let terminal_size = terminal.size()?;
    let terminal_cols = (terminal_size.width * 70 / 100).saturating_sub(2);
    let terminal_rows = terminal_size.height.saturating_sub(2);

    // Create app with initial dimensions
    let mut app = BotsterApp::new(terminal_rows, terminal_cols)?;

    // Main loop - check both app.quit and signal-triggered shutdown
    while !app.quit && !shutdown_requested() {
        // IMPORTANT: Handle keyboard input FIRST before any blocking operations
        // This ensures Ctrl+Q always works even if WebRTC operations are slow
        let _ = app.handle_events()?;

        // Check for shutdown after handling events (in case Ctrl+Q was pressed)
        if app.quit || shutdown_requested() {
            break;
        }

        // Get browser dimensions if WebRTC is connected (with timeout to prevent blocking)
        let browser_dims: Option<BrowserDimensions> = {
            let webrtc_handler = Arc::clone(&app.webrtc_handler);
            app.tokio_runtime.block_on(async move {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(10),
                    async {
                        let handler = webrtc_handler.lock().unwrap();
                        handler.get_browser_dimensions().await
                    }
                ).await {
                    Ok(dims) => dims,
                    Err(_) => None, // Timeout - skip this iteration
                }
            })
        };

        // Track browser connection state and resize agents accordingly
        {
            static LAST_DIMS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            static WAS_CONNECTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

            let is_connected = browser_dims.is_some();
            let was_connected = WAS_CONNECTED.swap(is_connected, std::sync::atomic::Ordering::Relaxed);

            if let Some(dims) = &browser_dims {
                // Browser connected - resize to browser dimensions when they change
                // Only resize if dimensions are reasonable (sanity check)
                if dims.cols >= 20 && dims.rows >= 5 {
                    // Include mode in the combined value to detect mode changes
                    let mode_bit = if dims.mode == BrowserMode::Gui { 1u32 << 31 } else { 0 };
                    let combined = mode_bit | ((dims.cols as u32) << 16) | (dims.rows as u32);
                    let last = LAST_DIMS.swap(combined, std::sync::atomic::Ordering::Relaxed);
                    if last != combined {
                        // Calculate agent terminal size based on mode
                        let (agent_cols, agent_rows) = match dims.mode {
                            BrowserMode::Gui => {
                                // GUI mode: use full browser dimensions for agent terminal
                                log::info!("GUI mode - using full browser dimensions: {}x{}", dims.cols, dims.rows);
                                (dims.cols, dims.rows)
                            }
                            BrowserMode::Tui => {
                                // TUI mode: terminal widget is 70% of width, minus borders
                                let tui_cols = (dims.cols * 70 / 100).saturating_sub(2);
                                let tui_rows = dims.rows.saturating_sub(2);
                                log::info!("TUI mode - using 70% width: {}x{} (from {}x{})", tui_cols, tui_rows, dims.cols, dims.rows);
                                (tui_cols, tui_rows)
                            }
                        };
                        for agent in app.agents.values() {
                            agent.resize(agent_rows, agent_cols);
                        }
                    }
                } else {
                    log::warn!("Ignoring small browser dimensions: {}x{}", dims.cols, dims.rows);
                }
            } else if was_connected {
                // Browser just disconnected - reset to local terminal dimensions
                log::info!("Browser disconnected, resetting agents to local terminal size");
                let terminal_size = terminal.size().unwrap_or_default();
                let terminal_cols = (terminal_size.width * 70 / 100).saturating_sub(2);
                let terminal_rows = terminal_size.height.saturating_sub(2);
                log::info!("Resizing agents to {}x{}", terminal_cols, terminal_rows);
                for agent in app.agents.values() {
                    agent.resize(terminal_rows, terminal_cols);
                }
                LAST_DIMS.store(0, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // Render current state and get ANSI output for WebRTC streaming
        let (ansi_output, rows, cols) = app.view(&mut terminal, browser_dims)?;

        // Stream TUI screen to WebRTC connected browsers (full TUI mode)
        // Use timeout to prevent blocking if WebRTC connection is in bad state
        {
            let webrtc_handler = Arc::clone(&app.webrtc_handler);
            let ansi_for_send = ansi_output.clone();
            app.tokio_runtime.block_on(async move {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    async {
                        let handler = webrtc_handler.lock().unwrap();
                        if handler.is_ready().await {
                            if let Err(e) = handler.send_screen(&ansi_for_send, rows, cols).await {
                                log::warn!("Failed to send screen to WebRTC: {}", e);
                            }
                        }
                    }
                ).await;
            });
        }

        // Stream selected agent's individual terminal output (for web GUI mode)
        // Only send when screen content actually changes to reduce bandwidth/lag
        {
            if let Some(key) = app.agent_keys_ordered.get(app.selected) {
                if let Some(agent) = app.agents.get(key) {
                    let current_hash = agent.get_screen_hash();
                    let last_hash = app.last_agent_screen_hash.get(key).copied();

                    // Only send if screen changed
                    if last_hash != Some(current_hash) {
                        app.last_agent_screen_hash
                            .insert(key.clone(), current_hash);

                        let agent_output = agent.get_screen_as_ansi();
                        let agent_id = key.clone();
                        let webrtc_handler = Arc::clone(&app.webrtc_handler);
                        // Use timeout to prevent blocking main loop
                        app.tokio_runtime.block_on(async move {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_millis(50),
                                async {
                                    let handler = webrtc_handler.lock().unwrap();
                                    if handler.is_ready().await {
                                        if let Err(e) =
                                            handler.send_agent_output(&agent_id, &agent_output).await
                                        {
                                            log::warn!("Failed to send agent output to WebRTC: {}", e);
                                        }
                                    }
                                }
                            ).await;
                        });
                    }
                }
            }
        }

        // Process keyboard input from WebRTC browser connections
        {
            let webrtc_handler = Arc::clone(&app.webrtc_handler);
            // Use timeout to prevent blocking main loop
            let inputs = app.tokio_runtime.block_on(async move {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(5),
                    async {
                        let handler = webrtc_handler.lock().unwrap();
                        handler.get_pending_inputs().await
                    }
                ).await {
                    Ok(inputs) => inputs,
                    Err(_) => vec![], // Timeout - no inputs this iteration
                }
            });

            for input in inputs {
                // Convert browser key input to crossterm KeyEvent
                let key_event = convert_browser_key_to_crossterm(&input);
                if let Some(key) = key_event {
                    if let Err(e) = app.handle_key_event(key) {
                        log::warn!("Error handling WebRTC key input: {}", e);
                    }
                }
            }
        }

        // Process commands from WebRTC browser connections
        {
            // Get pending commands with a short timeout to prevent blocking
            let webrtc_handler = Arc::clone(&app.webrtc_handler);
            let commands = app.tokio_runtime.block_on(async move {
                match tokio::time::timeout(
                    std::time::Duration::from_millis(5),
                    async {
                        let handler = webrtc_handler.lock().unwrap();
                        handler.get_pending_commands().await
                    }
                ).await {
                    Ok(cmds) => cmds,
                    Err(_) => vec![], // Timeout - no commands this iteration
                }
            });

            for command in commands {
                match command {
                    BrowserCommand::ListAgents => {
                        let agents = app.build_web_agent_list();
                        let webrtc_handler = Arc::clone(&app.webrtc_handler);
                        // Use timeout to prevent blocking main loop
                        app.tokio_runtime.block_on(async move {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_millis(50),
                                async {
                                    let handler = webrtc_handler.lock().unwrap();
                                    if let Err(e) = handler.send_agents(agents).await {
                                        log::warn!("Failed to send agent list: {}", e);
                                    }
                                }
                            ).await;
                        });
                    }
                    BrowserCommand::SelectAgent { id } => {
                        // Find the agent index by session key
                        if let Some(idx) = app.agent_keys_ordered.iter().position(|k| k == &id) {
                            app.selected = idx;
                            log::info!("WebRTC: Selected agent {}", id);

                            // Clear the hash to force immediate screen send
                            app.last_agent_screen_hash.remove(&id);

                            // Immediately send the agent's screen
                            if let Some(agent) = app.agents.get(&id) {
                                let agent_output = agent.get_screen_as_ansi();
                                let current_hash = agent.get_screen_hash();
                                app.last_agent_screen_hash.insert(id.clone(), current_hash);

                                let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                let id_clone = id.clone();
                                // Use timeout to prevent blocking main loop
                                app.tokio_runtime.block_on(async move {
                                    let _ = tokio::time::timeout(
                                        std::time::Duration::from_millis(50),
                                        async {
                                            let handler = webrtc_handler.lock().unwrap();
                                            if let Err(e) = handler.send_agent_selected(&id_clone).await {
                                                log::warn!("Failed to send agent selected: {}", e);
                                            }
                                            if let Err(e) =
                                                handler.send_agent_output(&id_clone, &agent_output).await
                                            {
                                                log::warn!("Failed to send agent output: {}", e);
                                            }
                                        }
                                    ).await;
                                });
                            }
                        } else {
                            log::warn!("WebRTC: Agent not found: {}", id);
                            let webrtc_handler = Arc::clone(&app.webrtc_handler);
                            let error_msg = format!("Agent not found: {}", id);
                            app.tokio_runtime.block_on(async move {
                                let _ = tokio::time::timeout(
                                    std::time::Duration::from_millis(50),
                                    async {
                                        let handler = webrtc_handler.lock().unwrap();
                                        let _ = handler.send_error(&error_msg).await;
                                    }
                                ).await;
                            });
                        }
                    }
                    BrowserCommand::ListWorktrees => {
                        log::info!("WebRTC: Listing available worktrees");
                        if let Err(e) = app.load_available_worktrees() {
                            log::error!("Failed to load worktrees: {}", e);
                        }
                        let (_, repo_name) = WorktreeManager::detect_current_repo()
                            .unwrap_or_else(|_| (std::path::PathBuf::new(), "unknown".to_string()));
                        let worktrees: Vec<WebWorktreeInfo> = app.available_worktrees
                            .iter()
                            .map(|(path, branch)| {
                                let issue_number = branch.strip_prefix("botster-issue-")
                                    .and_then(|n| n.parse::<u32>().ok());
                                WebWorktreeInfo {
                                    path: path.clone(),
                                    branch: branch.clone(),
                                    issue_number,
                                }
                            })
                            .collect();
                        let webrtc_handler = Arc::clone(&app.webrtc_handler);
                        // Use timeout to prevent blocking main loop
                        app.tokio_runtime.block_on(async move {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_millis(50),
                                async {
                                    let handler = webrtc_handler.lock().unwrap();
                                    if let Err(e) = handler.send_worktrees(&repo_name, worktrees).await {
                                        log::warn!("Failed to send worktrees: {}", e);
                                    }
                                }
                            ).await;
                        });
                    }
                    BrowserCommand::CreateAgent { issue_or_branch, prompt } => {
                        log::info!("WebRTC: Creating agent for {}", issue_or_branch);
                        // Store the input and create agent
                        app.input_buffer = issue_or_branch.clone();
                        if let Some(p) = prompt {
                            app.input_buffer = p; // Use the provided prompt
                        }
                        match app.create_and_spawn_agent() {
                            Ok(()) => {
                                // Get the last created agent's session key
                                if let Some(session_key) = app.agent_keys_ordered.last().cloned() {
                                    // Resize new agent to current browser dimensions
                                    let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                    // Use timeout to prevent blocking
                                    let browser_dims: Option<BrowserDimensions> = app.tokio_runtime.block_on(async {
                                        match tokio::time::timeout(
                                            std::time::Duration::from_millis(50),
                                            async {
                                                let handler = webrtc_handler.lock().unwrap();
                                                handler.get_browser_dimensions().await
                                            }
                                        ).await {
                                            Ok(dims) => dims,
                                            Err(_) => None,
                                        }
                                    });
                                    if let Some(dims) = browser_dims {
                                        if let Some(agent) = app.agents.get(&session_key) {
                                            let (agent_cols, agent_rows) = match dims.mode {
                                                BrowserMode::Gui => (dims.cols, dims.rows),
                                                BrowserMode::Tui => {
                                                    let tui_cols = (dims.cols * 70 / 100).saturating_sub(2);
                                                    let tui_rows = dims.rows.saturating_sub(2);
                                                    (tui_cols, tui_rows)
                                                }
                                            };
                                            log::info!("Resizing new agent to browser dims: {}x{}", agent_cols, agent_rows);
                                            agent.resize(agent_rows, agent_cols);
                                        }
                                    }

                                    // Notify browser
                                    let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                    // Use timeout to prevent blocking
                                    app.tokio_runtime.block_on(async move {
                                        let _ = tokio::time::timeout(
                                            std::time::Duration::from_millis(50),
                                            async {
                                                let handler = webrtc_handler.lock().unwrap();
                                                if let Err(e) = handler.send_agent_created(&session_key).await {
                                                    log::warn!("Failed to send agent created: {}", e);
                                                }
                                            }
                                        ).await;
                                    });
                                }
                            }
                            Err(e) => {
                                log::error!("WebRTC: Failed to create agent: {}", e);
                                let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                let error_msg = format!("Failed to create agent: {}", e);
                                // Use timeout to prevent blocking
                                app.tokio_runtime.block_on(async move {
                                    let _ = tokio::time::timeout(
                                        std::time::Duration::from_millis(50),
                                        async {
                                            let handler = webrtc_handler.lock().unwrap();
                                            let _ = handler.send_error(&error_msg).await;
                                        }
                                    ).await;
                                });
                            }
                        }
                        app.input_buffer.clear();
                    }
                    BrowserCommand::ReopenWorktree { path, branch, prompt } => {
                        log::info!("WebRTC: Reopening worktree {} ({})", path, branch);
                        // Find the worktree in available_worktrees and set selection
                        if let Some(idx) = app.available_worktrees.iter().position(|(p, _)| p == &path) {
                            app.worktree_selected = idx + 1; // +1 because "Create New" is at 0
                            app.input_buffer = prompt.unwrap_or_default();
                            match app.spawn_agent_from_worktree() {
                                Ok(()) => {
                                    if let Some(session_key) = app.agent_keys_ordered.last().cloned() {
                                        // Resize new agent to current browser dimensions
                                        let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                        // Use timeout to prevent blocking
                                        let browser_dims: Option<BrowserDimensions> = app.tokio_runtime.block_on(async {
                                            match tokio::time::timeout(
                                                std::time::Duration::from_millis(50),
                                                async {
                                                    let handler = webrtc_handler.lock().unwrap();
                                                    handler.get_browser_dimensions().await
                                                }
                                            ).await {
                                                Ok(dims) => dims,
                                                Err(_) => None,
                                            }
                                        });
                                        if let Some(dims) = browser_dims {
                                            if let Some(agent) = app.agents.get(&session_key) {
                                                let (agent_cols, agent_rows) = match dims.mode {
                                                    BrowserMode::Gui => (dims.cols, dims.rows),
                                                    BrowserMode::Tui => {
                                                        let tui_cols = (dims.cols * 70 / 100).saturating_sub(2);
                                                        let tui_rows = dims.rows.saturating_sub(2);
                                                        (tui_cols, tui_rows)
                                                    }
                                                };
                                                log::info!("Resizing reopened agent to browser dims: {}x{}", agent_cols, agent_rows);
                                                agent.resize(agent_rows, agent_cols);
                                            }
                                        }

                                        // Notify browser
                                        let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                        // Use timeout to prevent blocking
                                        app.tokio_runtime.block_on(async move {
                                            let _ = tokio::time::timeout(
                                                std::time::Duration::from_millis(50),
                                                async {
                                                    let handler = webrtc_handler.lock().unwrap();
                                                    if let Err(e) = handler.send_agent_created(&session_key).await {
                                                        log::warn!("Failed to send agent created: {}", e);
                                                    }
                                                }
                                            ).await;
                                        });
                                    }
                                }
                                Err(e) => {
                                    log::error!("WebRTC: Failed to reopen worktree: {}", e);
                                    let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                    let error_msg = format!("Failed to reopen worktree: {}", e);
                                    // Use timeout to prevent blocking
                                    app.tokio_runtime.block_on(async move {
                                        let _ = tokio::time::timeout(
                                            std::time::Duration::from_millis(50),
                                            async {
                                                let handler = webrtc_handler.lock().unwrap();
                                                let _ = handler.send_error(&error_msg).await;
                                            }
                                        ).await;
                                    });
                                }
                            }
                            app.input_buffer.clear();
                        } else {
                            log::error!("WebRTC: Worktree not found: {}", path);
                            let webrtc_handler = Arc::clone(&app.webrtc_handler);
                            let error_msg = format!("Worktree not found: {}", path);
                            // Use timeout to prevent blocking
                            app.tokio_runtime.block_on(async move {
                                let _ = tokio::time::timeout(
                                    std::time::Duration::from_millis(50),
                                    async {
                                        let handler = webrtc_handler.lock().unwrap();
                                        let _ = handler.send_error(&error_msg).await;
                                    }
                                ).await;
                            });
                        }
                    }
                    BrowserCommand::DeleteAgent { id, delete_worktree } => {
                        log::info!(
                            "WebRTC: Deleting agent {} (delete_worktree={})",
                            id,
                            delete_worktree
                        );
                        match app.close_agent(&id, delete_worktree) {
                            Ok(()) => {
                                let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                let id_clone = id.clone();
                                // Use timeout to prevent blocking
                                app.tokio_runtime.block_on(async move {
                                    let _ = tokio::time::timeout(
                                        std::time::Duration::from_millis(50),
                                        async {
                                            let handler = webrtc_handler.lock().unwrap();
                                            if let Err(e) = handler.send_agent_deleted(&id_clone).await {
                                                log::warn!("Failed to send agent deleted: {}", e);
                                            }
                                        }
                                    ).await;
                                });
                            }
                            Err(e) => {
                                log::error!("WebRTC: Failed to delete agent: {}", e);
                                let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                let error_msg = format!("Failed to delete agent: {}", e);
                                // Use timeout to prevent blocking
                                app.tokio_runtime.block_on(async move {
                                    let _ = tokio::time::timeout(
                                        std::time::Duration::from_millis(50),
                                        async {
                                            let handler = webrtc_handler.lock().unwrap();
                                            let _ = handler.send_error(&error_msg).await;
                                        }
                                    ).await;
                                });
                            }
                        }
                    }
                    BrowserCommand::SendInput { data } => {
                        // Send raw input to selected agent
                        if let Some(key) = app.agent_keys_ordered.get(app.selected).cloned() {
                            if let Some(agent) = app.agents.get_mut(&key) {
                                if let Err(e) = agent.write_input_str(&data) {
                                    log::warn!("Failed to send input to agent: {}", e);
                                }
                            }
                        }
                    }
                    BrowserCommand::Scroll { direction, lines } => {
                        // Scroll the selected agent's terminal
                        if let Some(key) = app.agent_keys_ordered.get(app.selected).cloned() {
                            if let Some(agent) = app.agents.get_mut(&key) {
                                match direction.as_str() {
                                    "up" => {
                                        agent.scroll_up(lines);
                                        log::debug!("WebRTC: Scrolled up {} lines, offset now {}", lines, agent.get_scroll_offset());
                                    }
                                    "down" => {
                                        agent.scroll_down(lines);
                                        log::debug!("WebRTC: Scrolled down {} lines, offset now {}", lines, agent.get_scroll_offset());
                                    }
                                    _ => {
                                        log::warn!("WebRTC: Unknown scroll direction: {}", direction);
                                    }
                                }
                                // Force screen refresh by clearing the hash
                                app.last_agent_screen_hash.remove(&key);
                            }
                        }
                    }
                    BrowserCommand::ScrollToTop => {
                        // Scroll to top of selected agent's terminal
                        if let Some(key) = app.agent_keys_ordered.get(app.selected).cloned() {
                            if let Some(agent) = app.agents.get_mut(&key) {
                                agent.scroll_to_top();
                                log::debug!("WebRTC: Scrolled to top, offset now {}", agent.get_scroll_offset());
                                // Force screen refresh
                                app.last_agent_screen_hash.remove(&key);
                            }
                        }
                    }
                    BrowserCommand::ScrollToBottom => {
                        // Scroll to bottom of selected agent's terminal (return to live view)
                        if let Some(key) = app.agent_keys_ordered.get(app.selected).cloned() {
                            if let Some(agent) = app.agents.get_mut(&key) {
                                agent.scroll_to_bottom();
                                log::debug!("WebRTC: Scrolled to bottom (live view)");
                                // Force screen refresh
                                app.last_agent_screen_hash.remove(&key);
                            }
                        }
                    }
                    BrowserCommand::TogglePtyView => {
                        // Toggle between CLI and Server PTY views for selected agent
                        if let Some(key) = app.agent_keys_ordered.get(app.selected).cloned() {
                            if let Some(agent) = app.agents.get_mut(&key) {
                                agent.toggle_pty_view();
                                log::info!("WebRTC: Toggled PTY view to {:?}", agent.active_pty);
                                // Force screen refresh
                                app.last_agent_screen_hash.remove(&key);
                            }
                        }
                    }
                }
            }
        }

        // Note: handle_events() is called at the START of the loop to ensure
        // keyboard input (especially Ctrl+Q) is always responsive

        // Poll for new messages from server
        if let Err(e) = app.poll_messages() {
            log::error!("Failed to poll messages: {}", e);
        }

        // Send heartbeat to register hub and agents with server
        if let Err(e) = app.send_heartbeat() {
            log::error!("Failed to send heartbeat: {}", e);
        }

        // Poll agents for terminal notifications (BEL, OSC) and send to Rails
        app.poll_agent_notifications();

        // Small sleep to prevent CPU spinning (60 FPS max)
        std::thread::sleep(Duration::from_millis(16));
    }

    // Cleanup is handled by TerminalGuard's Drop implementation
    // This ensures proper cleanup even on panic
    Ok(())
}

fn run_headless() -> Result<()> {
    println!("Starting Botster Hub v{} in headless mode...", VERSION);
    println!("Headless mode not yet implemented");
    Ok(())
}

// CLI
#[derive(Parser)]
#[command(name = "botster-hub")]
#[command(version = VERSION)]
#[command(about = "Interactive PTY-based daemon for GitHub automation")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Start {
        #[arg(long)]
        headless: bool,
    },
    Status,
    Config {
        key: Option<String>,
        value: Option<String>,
    },
    /// Get a value from a JSON file using dot notation (e.g., "projects.myproject.hasTrust")
    JsonGet {
        /// Path to the JSON file
        file: String,
        /// JSON path using dot notation
        key: String,
    },
    /// Set a value in a JSON file using dot notation
    JsonSet {
        /// Path to the JSON file
        file: String,
        /// JSON path using dot notation
        key: String,
        /// Value to set (will be parsed as JSON)
        value: String,
    },
    /// Delete a key from a JSON file using dot notation
    JsonDelete {
        /// Path to the JSON file
        file: String,
        /// JSON path using dot notation
        key: String,
    },
    /// Delete a git worktree and run teardown scripts
    DeleteWorktree {
        /// Issue number of the worktree to delete
        issue_number: u32,
    },
    /// List all git worktrees for the current repository
    ListWorktrees,
    /// Get the system prompt for an agent
    GetPrompt {
        /// Path to the worktree
        worktree_path: String,
    },
    /// Update botster-hub to the latest version
    Update {
        /// Show version without updating
        #[arg(long)]
        check: bool,
    },
}

fn main() -> Result<()> {
    // Set up file logging so TUI doesn't interfere with log output
    let log_file = std::fs::File::create("/tmp/botster-hub.log")
        .expect("Failed to create log file");
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .format_timestamp_secs()
        .init();

    // Set up panic hook to log panics and ensure terminal cleanup
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Log the panic
        log::error!("PANIC: {:?}", panic_info);

        // Ensure terminal is cleaned up before printing panic
        let _ = disable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            crossterm::cursor::Show
        );

        // Call the default panic handler
        default_hook(panic_info);
    }));

    let cli = Cli::parse();

    match cli.command {
        Commands::Start { headless } => {
            if headless {
                run_headless()?;
            } else {
                run_interactive()?;
            }
        }
        Commands::Status => {
            println!("Status command not yet implemented");
        }
        Commands::Config { key, value } => {
            let config = Config::load()?;
            match (key, value) {
                (None, None) => println!("{}", serde_json::to_string_pretty(&config)?),
                (Some(k), None) => println!("Config key '{}' query not implemented", k),
                (Some(k), Some(v)) => println!("Would set {} = {}", k, v),
                _ => {}
            }
        }
        Commands::JsonGet { file, key } => {
            commands::json::get(&file, &key)?;
        }
        Commands::JsonSet { file, key, value } => {
            commands::json::set(&file, &key, &value)?;
        }
        Commands::JsonDelete { file, key } => {
            commands::json::delete(&file, &key)?;
        }
        Commands::DeleteWorktree { issue_number } => {
            commands::worktree::delete(issue_number)?;
        }
        Commands::ListWorktrees => {
            commands::worktree::list()?;
        }
        Commands::GetPrompt { worktree_path } => {
            commands::prompt::get(&worktree_path)?;
        }
        Commands::Update { check } => {
            if check {
                commands::update::check()?;
            } else {
                commands::update::install()?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test AgentSpawnConfig invocation_url handling
    #[test]
    fn test_agent_spawn_config_has_invocation_url_field() {
        let config = AgentSpawnConfig {
            issue_number: Some(42),
            branch_name: "botster-issue-42".to_string(),
            worktree_path: std::path::PathBuf::from("/tmp/worktree"),
            repo_path: std::path::PathBuf::from("/tmp/repo"),
            repo_name: "owner/repo".to_string(),
            prompt: "Test prompt".to_string(),
            message_id: None,
            invocation_url: Some("https://github.com/owner/repo/issues/42".to_string()),
        };
        assert_eq!(config.invocation_url, Some("https://github.com/owner/repo/issues/42".to_string()));
    }

    #[test]
    fn test_agent_spawn_config_invocation_url_can_be_none() {
        let config = AgentSpawnConfig {
            issue_number: Some(42),
            branch_name: "botster-issue-42".to_string(),
            worktree_path: std::path::PathBuf::from("/tmp/worktree"),
            repo_path: std::path::PathBuf::from("/tmp/repo"),
            repo_name: "owner/repo".to_string(),
            prompt: "Test prompt".to_string(),
            message_id: None,
            invocation_url: None,
        };
        assert!(config.invocation_url.is_none());
    }
}
