use anyhow::Result;
use botster_hub::{commands, tui, Config, Hub};
use mimalloc::MiMalloc;

/// Global allocator configured per M-MIMALLOC-APPS guideline.
/// mimalloc provides better multi-threaded performance than the system allocator.
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
use clap::{Parser, Subcommand};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Version constant re-exported from commands module.
use commands::update::VERSION;

/// Global flag for signal-triggered shutdown (as Arc for signal-hook compatibility)
static SHUTDOWN_FLAG: std::sync::LazyLock<Arc<AtomicBool>> =
    std::sync::LazyLock::new(|| Arc::new(AtomicBool::new(false)));

/// Ensure user is authenticated, running device flow if needed.
fn ensure_authenticated() -> Result<()> {
    use botster_hub::auth;

    let mut config = Config::load()?;
    let using_env_var =
        std::env::var("BOTSTER_TOKEN").is_ok() || std::env::var("BOTSTER_API_KEY").is_ok();

    // Check if we have a token at all
    if !config.has_token() {
        println!("No authentication token found. Starting device authorization...");
        let token = auth::device_flow(&config.server_url)?;
        config.save_token(&token)?;
        println!("Token saved successfully.");
        return Ok(());
    }

    // Validate the token - even if from env var, we need to check it works
    println!("Checking authentication...");
    if !auth::validate_token(&config.server_url, config.get_api_key()) {
        println!("Token invalid or expired. Re-authenticating...");
        let token = auth::device_flow(&config.server_url)?;

        if using_env_var {
            // Don't save to config - user is managing token via env var
            // Just print instructions
            println!();
            println!("New token obtained. Update your environment variable:");
            println!("  export BOTSTER_TOKEN={}", token);
            println!();
            anyhow::bail!("Please update your environment variable and restart.");
        } else {
            config.save_token(&token)?;
            println!("Token saved successfully.");
        }
    }

    Ok(())
}

fn run_headless() -> Result<()> {
    println!("Starting Botster Hub v{} in headless mode...", VERSION);
    println!("Headless mode not yet implemented");
    Ok(())
}

/// Run the hub using the Hub architecture.
///
/// This sets up the terminal and delegates to Hub::run() for the event loop.
fn run_with_hub() -> Result<()> {
    // Ensure we have a valid authentication token
    ensure_authenticated()?;

    // Set up signal handlers
    use signal_hook::consts::signal::*;
    use signal_hook::flag;
    flag::register(SIGINT, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGTERM, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGHUP, Arc::clone(&SHUTDOWN_FLAG))?;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let _terminal_guard = tui::TerminalGuard::new();

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Get initial terminal size
    let terminal_size = terminal.size()?;
    let terminal_cols = (terminal_size.width * 70 / 100).saturating_sub(2);
    let terminal_rows = terminal_size.height.saturating_sub(2);

    // Create Hub and perform setup
    let config = Config::load()?;
    let mut hub = Hub::new(config, (terminal_rows, terminal_cols))?;
    hub.setup();

    log::info!("Botster Hub v{} started with Hub architecture", VERSION);

    // Run the event loop - Hub owns this now
    hub.run(&mut terminal, &SHUTDOWN_FLAG)?;

    // Shutdown
    hub.shutdown();

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
                // Use the new Hub-based architecture
                run_with_hub()?;
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
    use botster_hub::AgentSpawnConfig;

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
