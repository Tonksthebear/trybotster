use anyhow::Result;
use botster_hub::{Agent, Config, PromptManager, WorktreeManager};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
    Terminal,
};
use reqwest::blocking::Client;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Helper function to create a centered rect
fn centered_rect(
    percent_x: u16,
    percent_y: u16,
    r: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    use ratatui::layout::{Constraint, Direction, Layout};

    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

enum AppMode {
    Normal,
    Menu,
    NewAgentSelectWorktree,
    NewAgentCreateWorktree,
    NewAgentPrompt,
    CloseAgentConfirm,
}

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
}

impl BotsterApp {
    fn new(terminal_rows: u16, terminal_cols: u16) -> Result<Self> {
        let config = Config::load()?;
        let git_manager = WorktreeManager::new(config.worktree_base.clone());
        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

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
        };

        log::info!("Botster Hub started, waiting for messages...");

        Ok(app)
    }

    fn handle_events(&mut self) -> Result<bool> {
        // Check for events immediately (non-blocking)
        if !event::poll(Duration::from_millis(0))? {
            return Ok(false); // No events available
        }

        // Event available - read it immediately
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
                return Ok(true);
            }
            Event::Key(key) => {
                return self.handle_key_event(key);
            }
            _ => return Ok(false),
        }
    }

    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        use crossterm::event::KeyEventKind;

        // Only process key press events (not release or repeat)
        if key.kind != KeyEventKind::Press {
            return Ok(true);
        }

        match self.mode {
            AppMode::Normal => self.handle_normal_mode_key(key),
            AppMode::Menu => self.handle_menu_mode_key(key),
            AppMode::NewAgentSelectWorktree => self.handle_worktree_select_key(key),
            AppMode::NewAgentCreateWorktree => self.handle_create_worktree_key(key),
            AppMode::NewAgentPrompt => self.handle_prompt_input_key(key),
            AppMode::CloseAgentConfirm => self.handle_close_confirm_key(key),
        }
    }

    fn handle_normal_mode_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
                return Ok(true);
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+P: Open menu
                self.mode = AppMode::Menu;
                self.menu_selected = 0;
                return Ok(true);
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+J: next agent
                if self.selected < self.agent_keys_ordered.len().saturating_sub(1) {
                    self.selected += 1;
                }
                return Ok(true);
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+K: previous agent
                if self.selected > 0 {
                    self.selected -= 1;
                }
                return Ok(true);
            }
            KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+X: kill selected agent
                if let Some(key) = self.agent_keys_ordered.get(self.selected) {
                    self.agents.remove(key);
                    self.agent_keys_ordered.remove(self.selected);
                    // Adjust selection if needed
                    if self.selected >= self.agent_keys_ordered.len() && self.selected > 0 {
                        self.selected = self.agent_keys_ordered.len() - 1;
                    }
                }
                return Ok(true);
            }
            _ => {
                // Forward key input to selected agent's terminal
                let bytes_to_send = match key.code {
                    KeyCode::Char(c) => {
                        if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii_alphabetic()
                        {
                            // Send control character (Ctrl+A = 1, Ctrl+B = 2, etc.)
                            let ctrl_code = (c.to_ascii_uppercase() as u8) - b'@';
                            Some(vec![ctrl_code])
                        } else {
                            Some(c.to_string().into_bytes())
                        }
                    }
                    KeyCode::Backspace => Some(vec![8]),
                    KeyCode::Enter => Some(vec![b'\r']),
                    KeyCode::Esc => Some(vec![27]),
                    KeyCode::Left => Some(vec![27, 91, 68]),
                    KeyCode::Right => Some(vec![27, 91, 67]),
                    KeyCode::Up => Some(vec![27, 91, 65]),
                    KeyCode::Down => Some(vec![27, 91, 66]),
                    KeyCode::Home => Some(vec![27, 91, 72]),
                    KeyCode::End => Some(vec![27, 91, 70]),
                    KeyCode::PageUp => Some(vec![27, 91, 53, 126]),
                    KeyCode::PageDown => Some(vec![27, 91, 54, 126]),
                    KeyCode::Tab => Some(vec![9]),
                    KeyCode::BackTab => Some(vec![27, 91, 90]),
                    KeyCode::Delete => Some(vec![27, 91, 51, 126]),
                    KeyCode::Insert => Some(vec![27, 91, 50, 126]),
                    _ => None,
                };

                if let Some(bytes) = bytes_to_send {
                    if let Some(key) = self.agent_keys_ordered.get(self.selected) {
                        if let Some(agent) = self.agents.get_mut(key) {
                            let _ = agent.write_input(&bytes);
                        }
                    }
                }
                return Ok(true);
            }
        }
    }

    fn handle_menu_mode_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        const MENU_ITEMS: &[&str] = &["Toggle Polling", "New Agent", "Close Agent"];

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = AppMode::Normal;
                return Ok(true);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.menu_selected > 0 {
                    self.menu_selected -= 1;
                }
                return Ok(true);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.menu_selected < MENU_ITEMS.len() - 1 {
                    self.menu_selected += 1;
                }
                return Ok(true);
            }
            KeyCode::Enter => {
                // Execute selected menu item
                match self.menu_selected {
                    0 => {
                        // Toggle polling
                        self.polling_enabled = !self.polling_enabled;
                        self.mode = AppMode::Normal;
                    }
                    1 => {
                        // New agent - load worktrees
                        if let Err(e) = self.load_available_worktrees() {
                            log::error!("Failed to load worktrees: {}", e);
                            self.mode = AppMode::Normal;
                        } else {
                            self.mode = AppMode::NewAgentSelectWorktree;
                            self.worktree_selected = 0;
                        }
                    }
                    2 => {
                        // Close agent
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
                return Ok(true);
            }
            _ => return Ok(true),
        }
    }

    fn handle_worktree_select_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        // Total items = 1 (Create New) + available worktrees
        let total_items = self.available_worktrees.len() + 1;

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = AppMode::Normal;
                return Ok(true);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.worktree_selected > 0 {
                    self.worktree_selected -= 1;
                }
                return Ok(true);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.worktree_selected < total_items.saturating_sub(1) {
                    self.worktree_selected += 1;
                }
                return Ok(true);
            }
            KeyCode::Enter => {
                if self.worktree_selected == 0 {
                    // "Create New" option selected
                    self.mode = AppMode::NewAgentCreateWorktree;
                    self.input_buffer.clear();
                } else {
                    // Existing worktree selected
                    self.mode = AppMode::NewAgentPrompt;
                    self.input_buffer.clear();
                }
                return Ok(true);
            }
            _ => return Ok(true),
        }
    }

    fn handle_create_worktree_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Normal;
                self.input_buffer.clear();
                return Ok(true);
            }
            KeyCode::Char(c) if !c.is_control() && c != ' ' => {
                self.input_buffer.push(c);
                return Ok(true);
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
                return Ok(true);
            }
            KeyCode::Enter => {
                // Only create if buffer is not empty
                if !self.input_buffer.is_empty() {
                    if let Err(e) = self.create_and_spawn_agent() {
                        log::error!("Failed to create worktree and spawn agent: {}", e);
                    }
                }
                self.mode = AppMode::Normal;
                self.input_buffer.clear();
                return Ok(true);
            }
            _ => return Ok(true),
        }
    }

    fn handle_prompt_input_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Normal;
                self.input_buffer.clear();
                return Ok(true);
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
                return Ok(true);
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
                return Ok(true);
            }
            KeyCode::Enter => {
                // Create agent with the prompt
                if let Err(e) = self.spawn_agent_from_worktree() {
                    log::error!("Failed to spawn agent: {}", e);
                }
                self.mode = AppMode::Normal;
                self.input_buffer.clear();
                return Ok(true);
            }
            _ => return Ok(true),
        }
    }

    fn handle_close_confirm_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => {
                self.mode = AppMode::Normal;
                return Ok(true);
            }
            KeyCode::Char('y') => {
                // Close agent and ask about deleting worktree
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.remove(&key) {
                        self.agent_keys_ordered.remove(self.selected);

                        // Adjust selection
                        if self.selected >= self.agent_keys_ordered.len() && self.selected > 0 {
                            self.selected = self.agent_keys_ordered.len() - 1;
                        }

                        // TODO: Prompt whether to delete worktree
                        // For now, just close the agent without deleting
                        let label = if let Some(num) = agent.issue_number {
                            format!("issue #{}", num)
                        } else {
                            format!("branch {}", agent.branch_name)
                        };
                        log::info!("Closed agent for {}", label);
                    }
                }
                self.mode = AppMode::Normal;
                return Ok(true);
            }
            KeyCode::Char('d') => {
                // Close agent and delete worktree
                if let Some(key) = self.agent_keys_ordered.get(self.selected).cloned() {
                    if let Some(agent) = self.agents.remove(&key) {
                        self.agent_keys_ordered.remove(self.selected);

                        // Adjust selection
                        if self.selected >= self.agent_keys_ordered.len() && self.selected > 0 {
                            self.selected = self.agent_keys_ordered.len() - 1;
                        }

                        // Delete the worktree using the generic delete function
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
                return Ok(true);
            }
            _ => return Ok(true),
        }
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

            let (_, repo_name) = WorktreeManager::detect_current_repo()?;
            let worktree_path = std::path::PathBuf::from(&path);

            // Read init commands
            let (repo_path, _) = WorktreeManager::detect_current_repo()?;
            let init_commands = WorktreeManager::read_botster_init_commands(&repo_path)?;

            let id = uuid::Uuid::new_v4();
            let mut agent = Agent::new(
                id,
                repo_name.clone(),
                issue_number,
                branch.clone(),
                worktree_path.clone(),
            );
            agent.resize(self.terminal_rows, self.terminal_cols);

            let prompt = if self.input_buffer.is_empty() {
                if let Some(issue_num) = issue_number {
                    format!("Work on issue #{}", issue_num)
                } else {
                    format!("Work on {}", branch)
                }
            } else {
                self.input_buffer.clone()
            };

            let mut env_vars = HashMap::new();
            env_vars.insert("BOTSTER_REPO".to_string(), repo_name.clone());
            env_vars.insert(
                "BOTSTER_ISSUE_NUMBER".to_string(),
                issue_number
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "0".to_string()),
            );
            env_vars.insert("BOTSTER_BRANCH_NAME".to_string(), branch.clone());
            env_vars.insert(
                "BOTSTER_WORKTREE_PATH".to_string(),
                worktree_path.display().to_string(),
            );
            env_vars.insert("BOTSTER_PROMPT".to_string(), prompt.clone());

            let bin_path = std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "botster-hub".to_string());
            env_vars.insert("BOTSTER_HUB_BIN".to_string(), bin_path);

            agent.spawn("bash", &prompt, init_commands, env_vars)?;

            // Generate session key and add to tracking structures
            let session_key = agent.session_key();
            self.agent_keys_ordered.push(session_key.clone());
            self.agents.insert(session_key, agent);

            let label = if let Some(num) = issue_number {
                format!("issue #{}", num)
            } else {
                format!("branch {}", branch)
            };
            log::info!("Spawned agent for {}", label);
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
            (num, format!("botster-issue-{}", num))
        } else {
            (0, branch_name.to_string())
        };

        let (repo_path, repo_name) = WorktreeManager::detect_current_repo()?;

        // Create worktree with custom branch name
        let worktree_path = self
            .git_manager
            .create_worktree_with_branch(&actual_branch_name)?;

        // Read init commands
        let init_commands = WorktreeManager::read_botster_init_commands(&repo_path)?;

        let id = uuid::Uuid::new_v4();
        let mut agent = Agent::new(
            id,
            repo_name.clone(),
            if issue_number > 0 {
                Some(issue_number)
            } else {
                None
            },
            actual_branch_name.clone(),
            worktree_path.clone(),
        );
        agent.resize(self.terminal_rows, self.terminal_cols);

        let prompt = if issue_number > 0 {
            format!("Work on issue #{}", issue_number)
        } else {
            format!("Work on {}", actual_branch_name)
        };

        let mut env_vars = HashMap::new();
        env_vars.insert("BOTSTER_REPO".to_string(), repo_name.clone());
        env_vars.insert(
            "BOTSTER_ISSUE_NUMBER".to_string(),
            if issue_number > 0 {
                issue_number.to_string()
            } else {
                "0".to_string()
            },
        );
        env_vars.insert(
            "BOTSTER_BRANCH_NAME".to_string(),
            actual_branch_name.clone(),
        );
        env_vars.insert(
            "BOTSTER_WORKTREE_PATH".to_string(),
            worktree_path.display().to_string(),
        );
        env_vars.insert("BOTSTER_PROMPT".to_string(), prompt.clone());

        let bin_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "botster-hub".to_string());
        env_vars.insert("BOTSTER_HUB_BIN".to_string(), bin_path);

        agent.spawn("bash", &prompt, init_commands, env_vars)?;

        // Generate session key and add to tracking structures
        let session_key = agent.session_key();
        self.agent_keys_ordered.push(session_key.clone());
        self.agents.insert(session_key, agent);

        log::info!(
            "Created worktree and spawned agent for branch '{}'",
            actual_branch_name
        );

        Ok(())
    }

    fn view(&mut self, terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
        let agent_keys_ordered = &self.agent_keys_ordered;
        let agents = &self.agents;
        let selected = self.selected;
        let seconds_since_poll = self.last_poll.elapsed().as_secs();
        let poll_interval = self.config.poll_interval;
        let mode = &self.mode;
        let polling_enabled = self.polling_enabled;
        let menu_selected = self.menu_selected;
        let available_worktrees = &self.available_worktrees;
        let worktree_selected = self.worktree_selected;
        let input_buffer = &self.input_buffer;

        terminal.draw(|f| {
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
                    let text = if let Some(issue_num) = agent.issue_number {
                        format!("{}#{}", agent.repo, issue_num)
                    } else {
                        format!("{}/{}", agent.repo, agent.branch_name)
                    };
                    ListItem::new(text)
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

            let agent_title = format!(
                " Agents ({}) {} Poll: {}s [Ctrl+P menu | Ctrl+Q quit] ",
                agent_keys_ordered.len(),
                poll_status,
                if polling_enabled {
                    poll_interval - seconds_since_poll.min(poll_interval)
                } else {
                    0
                }
            );

            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(agent_title))
                .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
                .highlight_symbol("> ");

            f.render_stateful_widget(list, chunks[0], &mut state);

            // Render terminal view
            let selected_agent = agent_keys_ordered
                .get(selected)
                .and_then(|key| agents.get(key));
            if let Some(agent) = selected_agent {
                let parser = agent.vt100_parser.lock().unwrap();
                let screen = parser.screen();

                let terminal_title = if let Some(issue_num) = agent.issue_number {
                    format!(
                        " {}#{} [Ctrl+P menu | Ctrl+J/K switch] ",
                        agent.repo, issue_num
                    )
                } else {
                    format!(
                        " {}/{} [Ctrl+P menu | Ctrl+J/K switch] ",
                        agent.repo, agent.branch_name
                    )
                };

                let block = Block::default().borders(Borders::ALL).title(terminal_title);
                let pseudo_term = tui_term::widget::PseudoTerminal::new(screen).block(block);

                f.render_widget(pseudo_term, chunks[1]);
            }

            // Render modal overlays based on mode
            match mode {
                AppMode::Menu => {
                    let menu_items = vec![
                        format!(
                            "{} Toggle Polling ({})",
                            if menu_selected == 0 { ">" } else { " " },
                            if polling_enabled { "ON" } else { "OFF" }
                        ),
                        format!("{} New Agent", if menu_selected == 1 { ">" } else { " " }),
                        format!("{} Close Agent", if menu_selected == 2 { ">" } else { " " }),
                    ];

                    let area = centered_rect(50, 30, f.area());
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
        })?;

        Ok(())
    }

    fn poll_messages(&mut self) -> Result<()> {
        // Skip polling if disabled
        if !self.polling_enabled {
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
                // TODO: Acknowledge message
                log::info!(
                    "Successfully processed message {} ({})",
                    msg.id,
                    msg.event_type
                );
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

            let comment_body = payload["comment_body"].as_str().unwrap_or("New mention");
            let comment_author = payload["comment_author"].as_str().unwrap_or("unknown");

            // Send notification to existing agent
            // In Claude's TUI, we need to simulate Ctrl+D twice to submit the message
            let notification = format!(
                "=== NEW MENTION (automated notification) ===\n{} mentioned you: {}\n==================",
                comment_author, comment_body
            );

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

        // No existing agent - create new one
        // Get the user's task description from the payload
        let task_description = payload["prompt"]
            .as_str()
            .or_else(|| payload["comment_body"].as_str())
            .or_else(|| payload["context"].as_str())
            .unwrap_or("Work on this issue")
            .to_string();

        // Read init commands from .botster_init
        let init_commands = WorktreeManager::read_botster_init_commands(&repo_path)?;

        // Create a git worktree from the current repo
        let worktree_path = self
            .git_manager
            .create_worktree_from_current(issue_number)?;

        let id = uuid::Uuid::new_v4();
        let mut agent = Agent::new(
            id,
            repo_name.clone(),
            Some(issue_number),
            format!("botster-issue-{}", issue_number),
            worktree_path.clone(),
        );

        // Resize agent to match terminal dimensions
        agent.resize(self.terminal_rows, self.terminal_cols);

        // Create environment variables for the agent
        let mut env_vars = HashMap::new();
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

        // Spawn agent with a shell
        // Note: The actual prompt will be fetched by .botster_init script
        agent.spawn("bash", &task_description, init_commands, env_vars)?;

        log::info!("Spawned agent {} for issue #{}", id, issue_number);

        // Add agent to tracking structures using session key
        let session_key = agent.session_key();
        self.agent_keys_ordered.push(session_key.clone());
        self.agents.insert(session_key, agent);

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
}

fn run_interactive() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Get initial terminal size and calculate widget dimensions
    let terminal_size = terminal.size()?;
    let terminal_cols = (terminal_size.width * 70 / 100).saturating_sub(2);
    let terminal_rows = terminal_size.height.saturating_sub(2);

    // Create app with initial dimensions
    let mut app = BotsterApp::new(terminal_rows, terminal_cols)?;

    // Main loop
    while !app.quit {
        // Render current state
        app.view(&mut terminal)?;

        // Handle keyboard input (non-blocking)
        let _ = app.handle_events()?;

        // Poll for new messages from server
        if let Err(e) = app.poll_messages() {
            log::error!("Failed to poll messages: {}", e);
        }

        // Small sleep to prevent CPU spinning (60 FPS max)
        std::thread::sleep(Duration::from_millis(16));
    }

    // Cleanup
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

fn check_for_updates() -> Result<()> {
    use serde_json::Value;

    println!("Current version: {}", VERSION);
    println!("Checking for updates...");

    let client = reqwest::blocking::Client::new();
    let response = client
        .get("https://api.github.com/repos/Tonksthebear/trybotster/releases/latest")
        .header("User-Agent", "botster-hub")
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("Failed to check for updates: {}", response.status());
    }

    let release: Value = response.json()?;
    let latest_version = release["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid release data"))?
        .trim_start_matches('v');

    println!("Latest version: {}", latest_version);

    if latest_version == VERSION {
        println!("✓ You are running the latest version");
    } else {
        println!("→ Update available! Run 'botster-hub update' to install");
    }

    Ok(())
}

fn update_binary() -> Result<()> {
    use serde_json::Value;
    use std::env;
    use std::fs;

    println!("Current version: {}", VERSION);
    println!("Checking for updates...");

    let client = reqwest::blocking::Client::new();
    let response = client
        .get("https://api.github.com/repos/Tonksthebear/trybotster/releases/latest")
        .header("User-Agent", "botster-hub")
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("Failed to check for updates: {}", response.status());
    }

    let release: Value = response.json()?;
    let latest_version = release["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid release data"))?
        .trim_start_matches('v');

    println!("Latest version: {}", latest_version);

    if latest_version == VERSION {
        println!("✓ Already running the latest version");
        return Ok(());
    }

    // Determine platform
    let platform = if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        "macos-arm64"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        "macos-x86_64"
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        "linux-x86_64"
    } else {
        anyhow::bail!("Unsupported platform");
    };

    let binary_name = format!("botster-hub-{}", platform);
    let download_url = format!(
        "https://github.com/Tonksthebear/trybotster/releases/download/v{}/{}",
        latest_version, binary_name
    );
    let checksum_url = format!("{}.sha256", download_url);

    println!("Downloading version {}...", latest_version);

    // Download binary
    let binary_response = client
        .get(&download_url)
        .header("User-Agent", "botster-hub")
        .send()?;

    if !binary_response.status().is_success() {
        anyhow::bail!("Failed to download update: {}", binary_response.status());
    }

    let binary_data = binary_response.bytes()?;

    // Download checksum
    let checksum_response = client
        .get(&checksum_url)
        .header("User-Agent", "botster-hub")
        .send()?;

    if checksum_response.status().is_success() {
        let checksum_text = checksum_response.text()?;
        let expected_checksum = checksum_text
            .split_whitespace()
            .next()
            .ok_or_else(|| anyhow::anyhow!("Invalid checksum format"))?;

        // Verify checksum
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&binary_data);
        let actual_checksum = format!("{:x}", hasher.finalize());

        if actual_checksum != expected_checksum {
            anyhow::bail!("Checksum verification failed!");
        }
        println!("✓ Checksum verified");
    } else {
        log::warn!("Could not verify checksum (not found)");
    }

    // Get current binary path
    let current_exe = env::current_exe()?;
    let temp_path = current_exe.with_extension("new");

    // Write new binary to temp location
    fs::write(&temp_path, &binary_data)?;

    // Make it executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&temp_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&temp_path, perms)?;
    }

    // Replace current binary
    fs::rename(&temp_path, &current_exe)?;

    println!("✓ Successfully updated to version {}", latest_version);
    println!("Please restart botster-hub to use the new version");

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
    env_logger::init();
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
            json_get(&file, &key)?;
        }
        Commands::JsonSet { file, key, value } => {
            json_set(&file, &key, &value)?;
        }
        Commands::JsonDelete { file, key } => {
            json_delete(&file, &key)?;
        }
        Commands::DeleteWorktree { issue_number } => {
            delete_worktree(issue_number)?;
        }
        Commands::ListWorktrees => {
            list_worktrees()?;
        }
        Commands::GetPrompt { worktree_path } => {
            get_prompt(&worktree_path)?;
        }
        Commands::Update { check } => {
            if check {
                check_for_updates()?;
            } else {
                update_binary()?;
            }
        }
    }

    Ok(())
}

fn get_prompt(worktree_path: &str) -> Result<()> {
    use std::path::PathBuf;

    let path = PathBuf::from(worktree_path);
    let prompt = PromptManager::get_prompt(&path)?;

    // Print the prompt to stdout so it can be captured
    print!("{}", prompt);

    Ok(())
}

fn json_get(file_path: &str, key_path: &str) -> Result<()> {
    use anyhow::Context;
    use std::fs;
    use std::path::Path;

    let path = shellexpand::tilde(file_path);
    let content = fs::read_to_string(Path::new(path.as_ref()))
        .with_context(|| format!("Failed to read {}", file_path))?;

    let mut value: serde_json::Value = serde_json::from_str(&content)?;

    // Navigate through the key path
    for key in key_path.split('.') {
        value = value
            .get(key)
            .with_context(|| format!("Key '{}' not found in path '{}'", key, key_path))?
            .clone();
    }

    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn json_set(file_path: &str, key_path: &str, new_value: &str) -> Result<()> {
    use anyhow::Context;
    use std::fs;
    use std::path::Path;

    let path = shellexpand::tilde(file_path);
    let content = fs::read_to_string(Path::new(path.as_ref()))
        .with_context(|| format!("Failed to read {}", file_path))?;

    let mut root: serde_json::Value = serde_json::from_str(&content)?;

    // Parse the new value as JSON
    let parsed_value: serde_json::Value = serde_json::from_str(new_value)
        .unwrap_or_else(|_| serde_json::Value::String(new_value.to_string()));

    // Split the path and navigate/create structure
    let keys: Vec<&str> = key_path.split('.').collect();
    let mut current = &mut root;

    for (i, key) in keys.iter().enumerate() {
        if i == keys.len() - 1 {
            // Last key - set the value
            if let Some(obj) = current.as_object_mut() {
                obj.insert(key.to_string(), parsed_value.clone());
            } else {
                anyhow::bail!("Cannot set key '{}' - parent is not an object", key);
            }
        } else {
            // Navigate/create intermediate objects
            if !current.is_object() {
                anyhow::bail!("Cannot navigate through '{}' - not an object", key);
            }

            let obj = current.as_object_mut().unwrap();

            // If key doesn't exist or exists but isn't an object, create/replace with empty object
            if !obj.contains_key(*key) || !obj[*key].is_object() {
                obj.insert(key.to_string(), serde_json::json!({}));
            }
            current = obj.get_mut(*key).unwrap();
        }
    }

    // Write back to file
    fs::write(
        Path::new(path.as_ref()),
        serde_json::to_string_pretty(&root)?,
    )?;
    Ok(())
}

fn json_delete(file_path: &str, key_path: &str) -> Result<()> {
    use anyhow::Context;
    use std::fs;
    use std::path::Path;

    let path = shellexpand::tilde(file_path);
    let content = fs::read_to_string(Path::new(path.as_ref()))
        .with_context(|| format!("Failed to read {}", file_path))?;

    let mut root: serde_json::Value = serde_json::from_str(&content)?;

    // Split the path and navigate to parent
    let keys: Vec<&str> = key_path.split('.').collect();
    if keys.is_empty() {
        anyhow::bail!("Cannot delete root");
    }

    let mut current = &mut root;

    // Navigate to the parent of the key we want to delete
    for (i, key) in keys.iter().enumerate() {
        if i == keys.len() - 1 {
            // Last key - delete it
            if let Some(obj) = current.as_object_mut() {
                obj.remove(*key);
            } else {
                anyhow::bail!("Cannot delete key '{}' - parent is not an object", key);
            }
        } else {
            // Navigate to next level
            if !current.is_object() {
                anyhow::bail!("Cannot navigate through '{}' - not an object", key);
            }

            let obj = current.as_object_mut().unwrap();
            if !obj.contains_key(*key) {
                // Key doesn't exist, nothing to delete
                return Ok(());
            }

            current = obj.get_mut(*key).unwrap();
        }
    }

    // Write back to file
    fs::write(
        Path::new(path.as_ref()),
        serde_json::to_string_pretty(&root)?,
    )?;
    Ok(())
}

fn delete_worktree(issue_number: u32) -> Result<()> {
    let config = Config::load()?;
    let git_manager = WorktreeManager::new(config.worktree_base);

    git_manager.delete_worktree_by_issue_number(issue_number)?;

    println!("Successfully deleted worktree for issue #{}", issue_number);
    Ok(())
}

fn list_worktrees() -> Result<()> {
    use std::process::Command;

    // Detect current repository
    let (repo_path, repo_name) = WorktreeManager::detect_current_repo()?;

    println!("Worktrees for repository: {}", repo_name);
    println!();

    // Run `git worktree list --porcelain` for machine-readable output
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

    // Parse porcelain output
    // Format is:
    // worktree <path>
    // HEAD <sha>
    // branch <ref>
    // <blank line>

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
            // End of worktree entry
            worktrees.push((current_path.clone(), current_branch.clone()));
            current_path.clear();
            current_branch.clear();
        }
    }

    // Handle last entry if file doesn't end with blank line
    if !current_path.is_empty() {
        worktrees.push((current_path, current_branch));
    }

    // Display worktrees in a formatted way
    if worktrees.is_empty() {
        println!("No worktrees found");
    } else {
        println!("{:<40} {}", "Path", "Branch");
        println!("{}", "-".repeat(70));

        for (path, branch) in worktrees {
            // Try to extract issue number from branch name if it follows botster pattern
            let display_branch = if branch.starts_with("issue-") {
                branch
            } else if branch.is_empty() {
                "(detached)".to_string()
            } else {
                branch
            };

            println!("{:<40} {}", path, display_branch);
        }
    }

    Ok(())
}
