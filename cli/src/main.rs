use anyhow::Result;
use botster_hub::{
    Agent, BrowserCommand, BrowserDimensions, Config, KeyInput, PromptManager, WebAgentInfo,
    WebRTCHandler, WorktreeManager,
};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{CrosstermBackend, TestBackend},
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame, Terminal,
};
use reqwest::blocking::Client;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Configuration for spawning a new agent
struct AgentSpawnConfig {
    issue_number: Option<u32>,
    branch_name: String,
    worktree_path: std::path::PathBuf,
    repo_path: std::path::PathBuf,
    repo_name: String,
    prompt: String,
    message_id: Option<i64>,
}

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

/// Convert a ratatui Buffer to ANSI escape sequences for streaming to xterm.js
/// If browser_dims is provided, output is clipped to those dimensions
fn buffer_to_ansi(
    buffer: &Buffer,
    width: u16,
    height: u16,
    browser_dims: Option<BrowserDimensions>,
) -> String {
    use std::fmt::Write;

    // Use browser dimensions if provided, otherwise use buffer dimensions
    let (out_width, out_height) = if let Some(dims) = browser_dims {
        (dims.cols.min(width), dims.rows.min(height))
    } else {
        (width, height)
    };

    let mut output = String::new();

    // Reset and clear screen, move cursor to home
    output.push_str("\x1b[0m\x1b[H\x1b[2J");

    let mut last_fg = Color::Reset;
    let mut last_bg = Color::Reset;
    let mut last_modifiers = Modifier::empty();

    for y in 0..out_height {
        // Move cursor to start of line
        write!(output, "\x1b[{};1H", y + 1).unwrap();

        for x in 0..out_width {
            let cell = buffer.cell((x, y));
            if cell.is_none() {
                output.push(' ');
                continue;
            }
            let cell = cell.unwrap();

            // Check if style changed
            let fg = cell.fg;
            let bg = cell.bg;
            let modifiers = cell.modifier;

            if fg != last_fg || bg != last_bg || modifiers != last_modifiers {
                // Build SGR sequence
                output.push_str("\x1b[0m"); // Reset first

                // Apply modifiers
                if modifiers.contains(Modifier::BOLD) {
                    output.push_str("\x1b[1m");
                }
                if modifiers.contains(Modifier::DIM) {
                    output.push_str("\x1b[2m");
                }
                if modifiers.contains(Modifier::ITALIC) {
                    output.push_str("\x1b[3m");
                }
                if modifiers.contains(Modifier::UNDERLINED) {
                    output.push_str("\x1b[4m");
                }
                if modifiers.contains(Modifier::REVERSED) {
                    output.push_str("\x1b[7m");
                }

                // Apply foreground color
                match fg {
                    Color::Reset => {}
                    Color::Black => output.push_str("\x1b[30m"),
                    Color::Red => output.push_str("\x1b[31m"),
                    Color::Green => output.push_str("\x1b[32m"),
                    Color::Yellow => output.push_str("\x1b[33m"),
                    Color::Blue => output.push_str("\x1b[34m"),
                    Color::Magenta => output.push_str("\x1b[35m"),
                    Color::Cyan => output.push_str("\x1b[36m"),
                    Color::Gray => output.push_str("\x1b[90m"),
                    Color::DarkGray => output.push_str("\x1b[90m"),
                    Color::LightRed => output.push_str("\x1b[91m"),
                    Color::LightGreen => output.push_str("\x1b[92m"),
                    Color::LightYellow => output.push_str("\x1b[93m"),
                    Color::LightBlue => output.push_str("\x1b[94m"),
                    Color::LightMagenta => output.push_str("\x1b[95m"),
                    Color::LightCyan => output.push_str("\x1b[96m"),
                    Color::White => output.push_str("\x1b[37m"),
                    Color::Rgb(r, g, b) => {
                        write!(output, "\x1b[38;2;{};{};{}m", r, g, b).unwrap();
                    }
                    Color::Indexed(i) => {
                        write!(output, "\x1b[38;5;{}m", i).unwrap();
                    }
                }

                // Apply background color
                match bg {
                    Color::Reset => {}
                    Color::Black => output.push_str("\x1b[40m"),
                    Color::Red => output.push_str("\x1b[41m"),
                    Color::Green => output.push_str("\x1b[42m"),
                    Color::Yellow => output.push_str("\x1b[43m"),
                    Color::Blue => output.push_str("\x1b[44m"),
                    Color::Magenta => output.push_str("\x1b[45m"),
                    Color::Cyan => output.push_str("\x1b[46m"),
                    Color::Gray => output.push_str("\x1b[100m"),
                    Color::DarkGray => output.push_str("\x1b[100m"),
                    Color::LightRed => output.push_str("\x1b[101m"),
                    Color::LightGreen => output.push_str("\x1b[102m"),
                    Color::LightYellow => output.push_str("\x1b[103m"),
                    Color::LightBlue => output.push_str("\x1b[104m"),
                    Color::LightMagenta => output.push_str("\x1b[105m"),
                    Color::LightCyan => output.push_str("\x1b[106m"),
                    Color::White => output.push_str("\x1b[47m"),
                    Color::Rgb(r, g, b) => {
                        write!(output, "\x1b[48;2;{};{};{}m", r, g, b).unwrap();
                    }
                    Color::Indexed(i) => {
                        write!(output, "\x1b[48;5;{}m", i).unwrap();
                    }
                }

                last_fg = fg;
                last_bg = bg;
                last_modifiers = modifiers;
            }

            // Write the character
            output.push_str(cell.symbol());
        }
    }

    // Reset at end
    output.push_str("\x1b[0m");

    output
}

/// Convert browser keyboard input to crossterm KeyEvent
fn convert_browser_key_to_crossterm(input: &KeyInput) -> Option<crossterm::event::KeyEvent> {
    use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState};

    let mut modifiers = KeyModifiers::empty();
    if input.ctrl {
        modifiers |= KeyModifiers::CONTROL;
    }
    if input.alt {
        modifiers |= KeyModifiers::ALT;
    }
    if input.shift {
        modifiers |= KeyModifiers::SHIFT;
    }

    // Map browser key names to crossterm KeyCode
    let key_code = match input.key.as_str() {
        // Single character keys
        k if k.len() == 1 => {
            let c = k.chars().next().unwrap();
            KeyCode::Char(c)
        }
        // Special keys
        "Enter" => KeyCode::Enter,
        "Escape" => KeyCode::Esc,
        "Backspace" => KeyCode::Backspace,
        "Tab" => KeyCode::Tab,
        "ArrowUp" => KeyCode::Up,
        "ArrowDown" => KeyCode::Down,
        "ArrowLeft" => KeyCode::Left,
        "ArrowRight" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Delete" => KeyCode::Delete,
        "Insert" => KeyCode::Insert,
        // Function keys
        "F1" => KeyCode::F(1),
        "F2" => KeyCode::F(2),
        "F3" => KeyCode::F(3),
        "F4" => KeyCode::F(4),
        "F5" => KeyCode::F(5),
        "F6" => KeyCode::F(6),
        "F7" => KeyCode::F(7),
        "F8" => KeyCode::F(8),
        "F9" => KeyCode::F(9),
        "F10" => KeyCode::F(10),
        "F11" => KeyCode::F(11),
        "F12" => KeyCode::F(12),
        // Space
        " " => KeyCode::Char(' '),
        // Unknown keys - ignore
        _ => {
            log::debug!("Unknown browser key: {}", input.key);
            return None;
        }
    };

    Some(KeyEvent {
        code: key_code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    })
}

#[derive(Clone)]
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
    // WebRTC P2P support for browser connections
    tokio_runtime: tokio::runtime::Runtime,
    webrtc_handler: Arc<StdMutex<WebRTCHandler>>,
    // Track last agent screen hash for change detection (reduces bandwidth)
    last_agent_screen_hash: HashMap<String, u64>,
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

        // Spawn the agent
        let init_commands = vec!["source .botster_init".to_string()];
        agent.spawn("bash", "", init_commands, env_vars)?;

        // Register the agent
        let session_key = agent.session_key();
        self.agent_keys_ordered.push(session_key.clone());
        self.agents.insert(session_key, agent);

        let label = if let Some(num) = config.issue_number {
            format!("issue #{}", num)
        } else {
            format!("branch {}", config.branch_name)
        };
        log::info!("Spawned agent for {}", label);

        Ok(())
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
            );
            (ansi, dims.rows, dims.cols)
        } else {
            // No browser connected, return empty output
            (String::new(), 0, 0)
        };

        Ok((ansi_output, out_rows, out_cols))
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
        // Just run 'source .botster_init' which handles everything:
        // - Reading .botster_prompt file
        // - Setting up MCP servers
        // - Starting Claude with the prompt
        let init_commands = vec!["source .botster_init".to_string()];
        agent.spawn("bash", "", init_commands, env_vars)?;

        log::info!("Spawned agent {} for issue #{}", id, issue_number);

        // Add agent to tracking structures using session key
        let session_key = agent.session_key();
        self.agent_keys_ordered.push(session_key.clone());
        self.agents.insert(session_key, agent);

        Ok(())
    }

    /// Build the agent list for sending to WebRTC browsers
    fn build_web_agent_list(&self) -> Vec<WebAgentInfo> {
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

        log::info!(
            "Handling WebRTC offer for session {}, offer length: {} bytes",
            session_id,
            offer_sdp.len()
        );

        // Create the answer using the WebRTC handler (async operation)
        let webrtc_handler = Arc::clone(&self.webrtc_handler);
        let offer_sdp_owned = offer_sdp.to_string();

        let answer_sdp = self.tokio_runtime.block_on(async move {
            let mut handler = webrtc_handler.lock().unwrap();
            handler.handle_offer(&offer_sdp_owned).await
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
        // Get browser dimensions if WebRTC is connected
        let browser_dims: Option<BrowserDimensions> = {
            let webrtc_handler = Arc::clone(&app.webrtc_handler);
            app.tokio_runtime.block_on(async move {
                let handler = webrtc_handler.lock().unwrap();
                handler.get_browser_dimensions().await
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
                let combined = ((dims.cols as u32) << 16) | (dims.rows as u32);
                let last = LAST_DIMS.swap(combined, std::sync::atomic::Ordering::Relaxed);
                if last != combined {
                    log::info!("Using browser dimensions: {}x{} (cols x rows)", dims.cols, dims.rows);
                    // Resize all agents to browser dimensions (accounting for TUI chrome)
                    let agent_cols = (dims.cols * 70 / 100).saturating_sub(2);
                    let agent_rows = dims.rows.saturating_sub(2);
                    log::info!("Resizing agents to {}x{}", agent_cols, agent_rows);
                    for agent in app.agents.values() {
                        agent.resize(agent_rows, agent_cols);
                    }
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
        {
            let webrtc_handler = Arc::clone(&app.webrtc_handler);
            let ansi_for_send = ansi_output.clone();
            app.tokio_runtime.block_on(async move {
                let handler = webrtc_handler.lock().unwrap();
                if handler.is_ready().await {
                    if let Err(e) = handler.send_screen(&ansi_for_send, rows, cols).await {
                        log::warn!("Failed to send screen to WebRTC: {}", e);
                    }
                }
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
                        app.tokio_runtime.block_on(async move {
                            let handler = webrtc_handler.lock().unwrap();
                            if handler.is_ready().await {
                                if let Err(e) =
                                    handler.send_agent_output(&agent_id, &agent_output).await
                                {
                                    log::warn!("Failed to send agent output to WebRTC: {}", e);
                                }
                            }
                        });
                    }
                }
            }
        }

        // Process keyboard input from WebRTC browser connections
        {
            let webrtc_handler = Arc::clone(&app.webrtc_handler);
            let inputs = app.tokio_runtime.block_on(async move {
                let handler = webrtc_handler.lock().unwrap();
                handler.get_pending_inputs().await
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
            let webrtc_handler = Arc::clone(&app.webrtc_handler);
            let commands = app.tokio_runtime.block_on(async move {
                let handler = webrtc_handler.lock().unwrap();
                handler.get_pending_commands().await
            });

            for command in commands {
                match command {
                    BrowserCommand::ListAgents => {
                        let agents = app.build_web_agent_list();
                        let webrtc_handler = Arc::clone(&app.webrtc_handler);
                        app.tokio_runtime.block_on(async move {
                            let handler = webrtc_handler.lock().unwrap();
                            if let Err(e) = handler.send_agents(agents).await {
                                log::warn!("Failed to send agent list: {}", e);
                            }
                        });
                    }
                    BrowserCommand::SelectAgent { id } => {
                        // Find the agent index by session key
                        if let Some(idx) = app.agent_keys_ordered.iter().position(|k| k == &id) {
                            app.selected = idx;
                            log::info!("WebRTC: Selected agent {}", id);
                            let webrtc_handler = Arc::clone(&app.webrtc_handler);
                            let id_clone = id.clone();
                            app.tokio_runtime.block_on(async move {
                                let handler = webrtc_handler.lock().unwrap();
                                if let Err(e) = handler.send_agent_selected(&id_clone).await {
                                    log::warn!("Failed to send agent selected: {}", e);
                                }
                            });
                        } else {
                            log::warn!("WebRTC: Agent not found: {}", id);
                            let webrtc_handler = Arc::clone(&app.webrtc_handler);
                            let error_msg = format!("Agent not found: {}", id);
                            app.tokio_runtime.block_on(async move {
                                let handler = webrtc_handler.lock().unwrap();
                                let _ = handler.send_error(&error_msg).await;
                            });
                        }
                    }
                    BrowserCommand::CreateAgent { repo, issue_number } => {
                        log::info!("WebRTC: Creating agent for {}#{}", repo, issue_number);
                        // Create a synthetic payload for spawn_agent_for_message
                        let payload = serde_json::json!({
                            "issue_number": issue_number,
                            "prompt": format!("Work on issue #{}", issue_number)
                        });
                        match app.spawn_agent_for_message(0, &payload, "web_create") {
                            Ok(()) => {
                                // Find the newly created agent's session key
                                let repo_safe = repo.replace('/', "-");
                                let session_key = format!("{}-{}", repo_safe, issue_number);
                                let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                app.tokio_runtime.block_on(async move {
                                    let handler = webrtc_handler.lock().unwrap();
                                    if let Err(e) = handler.send_agent_created(&session_key).await {
                                        log::warn!("Failed to send agent created: {}", e);
                                    }
                                });
                            }
                            Err(e) => {
                                log::error!("WebRTC: Failed to create agent: {}", e);
                                let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                let error_msg = format!("Failed to create agent: {}", e);
                                app.tokio_runtime.block_on(async move {
                                    let handler = webrtc_handler.lock().unwrap();
                                    let _ = handler.send_error(&error_msg).await;
                                });
                            }
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
                                app.tokio_runtime.block_on(async move {
                                    let handler = webrtc_handler.lock().unwrap();
                                    if let Err(e) = handler.send_agent_deleted(&id_clone).await {
                                        log::warn!("Failed to send agent deleted: {}", e);
                                    }
                                });
                            }
                            Err(e) => {
                                log::error!("WebRTC: Failed to delete agent: {}", e);
                                let webrtc_handler = Arc::clone(&app.webrtc_handler);
                                let error_msg = format!("Failed to delete agent: {}", e);
                                app.tokio_runtime.block_on(async move {
                                    let handler = webrtc_handler.lock().unwrap();
                                    let _ = handler.send_error(&error_msg).await;
                                });
                            }
                        }
                    }
                    BrowserCommand::SendInput { data } => {
                        // Send raw input to selected agent
                        if let Some(key) = app.agent_keys_ordered.get(app.selected) {
                            if let Some(agent) = app.agents.get_mut(key) {
                                if let Err(e) = agent.write_input_str(&data) {
                                    log::warn!("Failed to send input to agent: {}", e);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Handle local keyboard input (non-blocking)
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
    use semver::Version;
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
    let latest_version_str = release["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid release data"))?
        .trim_start_matches('v');

    println!("Latest version: {}", latest_version_str);

    let current = Version::parse(VERSION)?;
    let latest = Version::parse(latest_version_str)?;

    if latest > current {
        println!("→ Update available! Run 'botster-hub update' to install");
    } else if latest == current {
        println!("✓ You are running the latest version");
    } else {
        println!("✓ You are running a newer version than the latest release");
    }

    Ok(())
}

fn update_binary() -> Result<()> {
    use semver::Version;
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
    let latest_version_str = release["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid release data"))?
        .trim_start_matches('v');

    println!("Latest version: {}", latest_version_str);

    let current = Version::parse(VERSION)?;
    let latest = Version::parse(latest_version_str)?;

    if latest <= current {
        println!("✓ Already running the latest version (or newer)");
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
        latest_version_str, binary_name
    );
    let checksum_url = format!("{}.sha256", download_url);

    println!("Downloading version {}...", latest_version_str);

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

    println!("✓ Successfully updated to version {}", latest_version_str);
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
    // Set up file logging so TUI doesn't interfere with log output
    let log_file = std::fs::File::create("/tmp/botster-hub.log")
        .expect("Failed to create log file");
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .format_timestamp_secs()
        .init();

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
