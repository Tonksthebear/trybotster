use anyhow::Result;
use botster_hub::{Agent, Config, WorktreeManager};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use reqwest::blocking::Client;
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
        use std::path::PathBuf;

        // Spawn 2 test agents running bash shells
        for i in 1..=2 {
            let id = uuid::Uuid::new_v4();
            let repo = format!("test/repo{}", i);
            let issue_number = i as u32;
            let worktree_path = PathBuf::from(format!("/tmp/test-agent-{}", i));

            let mut agent = Agent::new(id, repo, issue_number, worktree_path.clone());

            // Resize agent to match terminal dimensions before spawning
            agent.resize(terminal_rows, terminal_cols);

            // Spawn a bash shell for testing
            let shell_cmd = if cfg!(target_os = "macos") {
                "/bin/bash"
            } else {
                "/bin/sh"
            };

            match agent.spawn(shell_cmd, "Test shell - type commands and press Enter") {
                Ok(_) => {
                    log::info!("Spawned test agent {}", i);
                    self.agents.push(agent);
                }
                Err(e) => {
                    log::error!("Failed to spawn test agent {}: {}", i, e);
                }
            }
        }

        Ok(())
    }

    fn handle_events(&mut self) -> Result<bool> {
        use crossterm::event::{self, Event, KeyCode, KeyModifiers};

        // Check for events immediately (non-blocking)
        if !event::poll(std::time::Duration::from_millis(0))? {
            return Ok(false); // No events available
        }

        // Event available - read it immediately
        match event::read()? {
            Event::Resize(cols, rows) => {
                // Calculate terminal widget dimensions
                // Layout: 30% left panel, 70% right terminal panel
                // Right panel dimensions (matching smux.rs pattern):
                let terminal_cols = (cols * 70 / 100).saturating_sub(2); // Account for borders
                let terminal_rows = rows.saturating_sub(2); // Account for borders

                // Resize all agents' parsers AND PTYs
                for agent in &self.agents {
                    agent.resize(terminal_rows, terminal_cols);
                }
                return Ok(true);
            }
            Event::Mouse(mouse) => {
                use crossterm::event::{MouseButton, MouseEventKind};

                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        // Click on agent list to select
                        // Agent list is left 30%, so check if x is in that range
                        // Each agent is one row, starting after the title row
                        let agents_width_percent = 30;

                        if mouse.column < (agents_width_percent * mouse.column / 100) + 10 {
                            // Click is in agent list area
                            // Row 0 is title, rows 1+ are agents
                            if mouse.row > 0 {
                                let clicked_agent = (mouse.row - 1) as usize;
                                if clicked_agent < self.agents.len() {
                                    self.selected = clicked_agent;
                                    self.scroll_offset = 0; // Reset scroll when switching
                                }
                            }
                        }
                        return Ok(true);
                    }
                    MouseEventKind::ScrollUp => {
                        // Scroll wheel up: scroll back in history
                        if !self.scroll_mode {
                            // Auto-enter scroll mode on scroll wheel
                            self.scroll_mode = true;
                        }
                        self.scroll_offset = self.scroll_offset.saturating_add(3);
                        return Ok(true);
                    }
                    MouseEventKind::ScrollDown => {
                        // Scroll wheel down: scroll forward
                        if self.scroll_offset > 0 {
                            self.scroll_offset = self.scroll_offset.saturating_sub(3);
                            if self.scroll_offset == 0 {
                                // Auto-exit scroll mode when at bottom
                                self.scroll_mode = false;
                            }
                        }
                        return Ok(true);
                    }
                    _ => return Ok(false),
                }
            }
            Event::Key(key) => {
                // Special keys for app control (with Ctrl modifier to avoid conflicts)
                match key.code {
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.quit = true;
                        return Ok(true);
                    }
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Ctrl+S: toggle scroll mode
                        self.scroll_mode = !self.scroll_mode;
                        if !self.scroll_mode {
                            self.scroll_offset = 0; // Reset scroll when exiting scroll mode
                        }
                        return Ok(true);
                    }
                    KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Ctrl+J: next agent
                        if self.selected < self.agents.len().saturating_sub(1) {
                            self.selected += 1;
                            self.scroll_offset = 0; // Reset scroll when switching agents
                        }
                        return Ok(true);
                    }
                    KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        // Ctrl+K: previous agent
                        if self.selected > 0 {
                            self.selected -= 1;
                            self.scroll_offset = 0; // Reset scroll when switching agents
                        }
                        return Ok(true);
                    }
                    _ => {
                        // In scroll mode, handle navigation keys
                        if self.scroll_mode {
                            match key.code {
                                KeyCode::Esc => {
                                    // Esc exits scroll mode
                                    self.scroll_mode = false;
                                    self.scroll_offset = 0;
                                    return Ok(true);
                                }
                                KeyCode::Up => {
                                    // Scroll up one line (VT100 clamps automatically)
                                    self.scroll_offset = self.scroll_offset.saturating_add(1);
                                    return Ok(true);
                                }
                                KeyCode::Down => {
                                    // Scroll down one line
                                    self.scroll_offset = self.scroll_offset.saturating_sub(1);
                                    return Ok(true);
                                }
                                KeyCode::PageUp => {
                                    // Scroll up one page (20 lines)
                                    self.scroll_offset = self.scroll_offset.saturating_add(20);
                                    return Ok(true);
                                }
                                KeyCode::PageDown => {
                                    // Scroll down one page (20 lines)
                                    self.scroll_offset = self.scroll_offset.saturating_sub(20);
                                    return Ok(true);
                                }
                                KeyCode::Home => {
                                    // Jump to top of scrollback (use a large number, VT100 clamps)
                                    self.scroll_offset = 100000;
                                    return Ok(true);
                                }
                                KeyCode::End => {
                                    // Jump to bottom (live view)
                                    self.scroll_offset = 0;
                                    return Ok(true);
                                }
                                _ => {
                                    // Ignore other keys in scroll mode
                                    return Ok(true);
                                }
                            }
                        } else {
                            // Normal mode: Everything goes to the selected agent's PTY
                            if let Some(bytes) = self.key_to_bytes(&key) {
                                if let Some(agent) = self.agents.get_mut(self.selected) {
                                    let _ = agent.write_input(&bytes);
                                }
                            }
                            return Ok(true);
                        }
                    }
                }
            }
            _ => return Ok(false),
        }
    }

    fn key_to_bytes(&self, key: &crossterm::event::KeyEvent) -> Option<Vec<u8>> {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Char(c) => Some(c.to_string().into_bytes()),
            KeyCode::Enter => Some(vec![b'\n']),
            KeyCode::Backspace => Some(vec![8]),
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
        let scroll_mode = self.scroll_mode;
        let scroll_offset = self.scroll_offset;

        terminal.draw(|f| {
            // Create layout
            use ratatui::layout::{Constraint, Direction, Layout};
            use ratatui::style::{Color, Modifier, Style};
            use ratatui::text::{Line, Span};
            use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

            // Main layout: body + status bar
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)].as_ref())
                .split(f.area());

            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
                .split(main_chunks[0]);

            // Render agent list with real data
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
                // Use VT100's built-in scrollback by setting the scrollback offset
                let mut parser = agent.vt100_parser.lock().unwrap();
                parser.set_scrollback(scroll_offset);
                let screen = parser.screen();

                let scroll_indicator = if scroll_mode && scroll_offset > 0 {
                    format!(" SCROLL (↑{} lines) [Esc=exit] ", scroll_offset)
                } else if scroll_mode {
                    " SCROLL (at bottom) [Esc to exit] ".to_string()
                } else {
                    " [Ctrl+Q quit | Ctrl+S scroll] ".to_string()
                };

                let terminal_title = format!(
                    " {}#{} {} ",
                    agent.repo, agent.issue_number, scroll_indicator
                );

                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(terminal_title)
                    .border_style(if scroll_mode {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    });

                let pseudo_term = tui_term::widget::PseudoTerminal::new(screen).block(block);

                f.render_widget(pseudo_term, chunks[1]);
            } else {
                let para = Paragraph::new("No agents running")
                    .block(Block::default().borders(Borders::ALL).title(" Terminal "));
                f.render_widget(para, chunks[1]);
            }

            // Render status bar
            let status_text = if let Some(agent) = agents.get(selected) {
                let uptime = agent.age();
                let uptime_str = if uptime.as_secs() < 60 {
                    format!("{}s", uptime.as_secs())
                } else if uptime.as_secs() < 3600 {
                    format!("{}m", uptime.as_secs() / 60)
                } else {
                    format!(
                        "{}h{}m",
                        uptime.as_secs() / 3600,
                        (uptime.as_secs() % 3600) / 60
                    )
                };

                let mode_indicator = if scroll_mode {
                    format!("SCROLL (↑{}) ", scroll_offset)
                } else {
                    "LIVE ".to_string()
                };

                let status_line = vec![
                    Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(agent.status.to_string(), Style::default().fg(Color::Green)),
                    Span::raw(" | "),
                    Span::styled("Uptime: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(uptime_str, Style::default().fg(Color::Cyan)),
                    Span::raw(" | "),
                    Span::styled("Mode: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        mode_indicator,
                        if scroll_mode {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::Green)
                        },
                    ),
                    Span::raw(" | "),
                    Span::styled("Mouse: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        "Click=Select  Wheel=Scroll",
                        Style::default().fg(Color::Blue),
                    ),
                ];
                Line::from(status_line)
            } else {
                Line::from(vec![Span::raw("No agents running")])
            };

            let status_bar = Paragraph::new(status_text).style(Style::default().bg(Color::Black));
            f.render_widget(status_bar, main_chunks[1]);
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

        // TODO: Spawn agents for new messages

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
    println!("Headless mode not yet implemented in tui-realm version");
    println!("Use non-headless mode for now");
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
    }

    Ok(())
}
