use anyhow::Result;
use botster_hub::{Agent, Config, WorktreeManager};
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

struct BotsterApp {
    agents: Vec<Agent>,
    selected: usize,
    config: Config,
    git_manager: WorktreeManager,
    client: Client,
    quit: bool,
    last_poll: Instant,
}

impl BotsterApp {
    fn new(terminal_rows: u16, terminal_cols: u16) -> Result<Self> {
        let config = Config::load()?;
        let git_manager = WorktreeManager::new(config.worktree_base.clone());
        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        let mut app = Self {
            agents: Vec::new(),
            selected: 0,
            config,
            git_manager,
            client,
            quit: false,
            last_poll: Instant::now(),
        };

        // Spawn test agents with shells for testing
        app.spawn_test_agents(terminal_rows, terminal_cols)?;

        Ok(app)
    }

    fn spawn_test_agents(&mut self, terminal_rows: u16, terminal_cols: u16) -> Result<()> {
        // Detect the current repo
        let (repo_path, repo_name) = WorktreeManager::detect_current_repo()?;

        // Read init commands from .botster_init
        let init_commands = WorktreeManager::read_botster_init_commands(&repo_path)?;

        // Spawn 2 test agents in git worktrees from current repo
        // Use high issue numbers to ensure fresh worktrees
        for i in 300..=301 {
            let id = uuid::Uuid::new_v4();
            let issue_number = i as u32;

            // Create a git worktree from the current repo
            let worktree_path = match self.git_manager.create_worktree_from_current(issue_number) {
                Ok(path) => path,
                Err(e) => {
                    log::error!("Failed to create worktree for agent {}: {}", i, e);
                    continue;
                }
            };

            let mut agent = Agent::new(id, repo_name.clone(), issue_number, worktree_path.clone());

            // Resize agent to match terminal dimensions before spawning
            agent.resize(terminal_rows, terminal_cols);

            // Create environment variables for the agent
            let mut env_vars = HashMap::new();
            env_vars.insert("BOTSTER_REPO".to_string(), repo_name.clone());
            env_vars.insert("BOTSTER_ISSUE_NUMBER".to_string(), issue_number.to_string());
            env_vars.insert(
                "BOTSTER_WORKTREE_PATH".to_string(),
                worktree_path.display().to_string(),
            );
            // For test agents, use a placeholder prompt
            env_vars.insert(
                "BOTSTER_PROMPT".to_string(),
                format!("Test agent for issue #{}", issue_number),
            );
            // Add path to botster-hub binary for use in init scripts
            let bin_path = std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "botster-hub".to_string());
            env_vars.insert("BOTSTER_HUB_BIN".to_string(), bin_path);

            // Spawn a bash shell in the worktree
            let shell_cmd = if cfg!(target_os = "macos") {
                "/bin/bash"
            } else {
                "/bin/sh"
            };

            match agent.spawn(
                shell_cmd,
                "Agent shell - each agent has its own git worktree",
                init_commands.clone(),
                env_vars,
            ) {
                Ok(_) => {
                    log::info!(
                        "Spawned agent {} in worktree: {}",
                        i,
                        worktree_path.display()
                    );
                    self.agents.push(agent);
                }
                Err(e) => {
                    log::error!("Failed to spawn agent {}: {}", i, e);
                }
            }
        }

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

                // Resize all agents
                for agent in &self.agents {
                    agent.resize(terminal_rows, terminal_cols);
                }
                return Ok(true);
            }
            Event::Key(key) => {
                match key.code {
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.quit = true;
                        return Ok(true);
                    }
                    KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Ctrl+J: next agent
                        if self.selected < self.agents.len().saturating_sub(1) {
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
                    _ => {
                        // Forward all other keys to the selected agent
                        if let Some(bytes) = self.key_to_bytes(&key) {
                            if let Some(agent) = self.agents.get_mut(self.selected) {
                                let _ = agent.write_input(&bytes);
                            }
                        }
                        return Ok(true);
                    }
                }
            }
            _ => return Ok(false),
        }
    }

    fn key_to_bytes(&self, key: &crossterm::event::KeyEvent) -> Option<Vec<u8>> {
        match key.code {
            KeyCode::Char(c) => Some(c.to_string().into_bytes()),
            KeyCode::Enter => Some(vec![b'\r']),
            KeyCode::Backspace => Some(vec![127]), // DEL character
            KeyCode::Tab => Some(vec![9]),
            KeyCode::Left => Some(vec![27, 91, 68]),
            KeyCode::Right => Some(vec![27, 91, 67]),
            KeyCode::Up => Some(vec![27, 91, 65]),
            KeyCode::Down => Some(vec![27, 91, 66]),
            KeyCode::Home => Some(vec![27, 91, 72]),
            KeyCode::End => Some(vec![27, 91, 70]),
            KeyCode::PageUp => Some(vec![27, 91, 53, 126]),
            KeyCode::PageDown => Some(vec![27, 91, 54, 126]),
            KeyCode::Delete => Some(vec![27, 91, 51, 126]),
            KeyCode::Insert => Some(vec![27, 91, 50, 126]),
            KeyCode::BackTab => Some(vec![27, 91, 90]),
            _ => None,
        }
    }

    fn view(&mut self, terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
        let agents = &self.agents;
        let selected = self.selected;

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
                .split(f.area());

            // Render agent list
            let items: Vec<ListItem> = agents
                .iter()
                .map(|agent| {
                    let text = format!("{}#{}", agent.repo, agent.issue_number);
                    ListItem::new(text)
                })
                .collect();

            let mut state = ListState::default();
            state.select(Some(selected.min(agents.len().saturating_sub(1))));

            let agent_title = format!(" Agents ({}) [Ctrl+J/K] ", agents.len());

            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(agent_title))
                .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
                .highlight_symbol("> ");

            f.render_stateful_widget(list, chunks[0], &mut state);

            // Render terminal view
            if let Some(agent) = agents.get(selected) {
                let parser = agent.vt100_parser.lock().unwrap();
                let screen = parser.screen();

                let terminal_title = format!(
                    " {}#{} [Ctrl+Q quit | Ctrl+J/K switch] ",
                    agent.repo, agent.issue_number
                );

                let block = Block::default().borders(Borders::ALL).title(terminal_title);
                let pseudo_term = tui_term::widget::PseudoTerminal::new(screen).block(block);

                f.render_widget(pseudo_term, chunks[1]);
            }
        })?;

        Ok(())
    }

    fn poll_messages(&mut self) -> Result<()> {
        // Poll every N seconds
        if self.last_poll.elapsed() < Duration::from_secs(self.config.poll_interval as u64) {
            return Ok(());
        }

        self.last_poll = Instant::now();

        let url = format!("{}/bot/messages/pending", self.config.server_url);
        let response = self
            .client
            .get(&url)
            .header("X-API-Key", &self.config.api_key)
            .send()?;

        if !response.status().is_success() {
            log::warn!("Failed to poll messages: {}", response.status());
            return Ok(());
        }

        let messages: Vec<serde_json::Value> = response.json()?;
        log::info!("Polled {} pending messages", messages.len());

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
    /// Delete a git worktree and run teardown scripts
    DeleteWorktree {
        /// Issue number of the worktree to delete
        issue_number: u32,
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
        Commands::DeleteWorktree { issue_number } => {
            delete_worktree(issue_number)?;
        }
    }

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

fn delete_worktree(issue_number: u32) -> Result<()> {
    let config = Config::load()?;
    let git_manager = WorktreeManager::new(config.worktree_base);

    git_manager.delete_worktree_by_issue_number(issue_number)?;

    println!("Successfully deleted worktree for issue #{}", issue_number);
    Ok(())
}
