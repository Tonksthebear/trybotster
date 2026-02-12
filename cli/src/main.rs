//! Botster Hub CLI - manages autonomous Claude agents for GitHub issues.
//!
//! This is the main binary entry point. See the `botster_hub` library
//! for the core functionality.

use anyhow::Result;
use botster_hub::{commands, tui, Config, Hub};
use mimalloc::MiMalloc;

/// Global allocator configured per M-MIMALLOC-APPS guideline.
/// mimalloc provides better multi-threaded performance than the system allocator.
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;
use clap::{Parser, Subcommand};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, PopKeyboardEnhancementFlags},
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
///
/// This function validates the token against the server before returning.
/// After returning successfully, callers can be confident that:
/// 1. A valid token exists in the keyring (or env var)
/// 2. The token has been verified against the server
fn ensure_authenticated() -> Result<()> {
    use botster_hub::auth;

    // Skip auth validation in test mode (BOTSTER_ENV=test)
    if botster_hub::env::is_test_mode() {
        log::info!("Skipping authentication (BOTSTER_ENV=test)");
        return Ok(());
    }

    let mut config = Config::load()?;
    let using_env_var =
        std::env::var("BOTSTER_TOKEN").is_ok() || std::env::var("BOTSTER_API_KEY").is_ok();

    // Check if we have a token at all
    if !config.has_token() {
        // Name the hub first, then authenticate
        let hub_name = auth::prompt_hub_name()?;
        config.hub_name = Some(hub_name);

        let token_response = auth::device_flow(&config.server_url)?;
        save_tokens(&mut config, &token_response)?;
        config.save()?;
        println!("  Setup complete.");
        println!();

        // Verify the token was saved correctly by reloading
        let verify_config = Config::load()?;
        if !verify_config.has_token() {
            anyhow::bail!(
                "Token was not saved correctly to keyring. \
                 This may be a permissions issue with your system keychain."
            );
        }
        log::info!(
            "New token saved and verified: {}...{}",
            &verify_config.get_api_key()[..10.min(verify_config.get_api_key().len())],
            &verify_config.get_api_key()
                [verify_config.get_api_key().len().saturating_sub(4)..]
        );
        return Ok(());
    }

    // Validate the token - even if from env var, we need to check it works
    println!("Checking authentication...");
    let token_preview = format!(
        "{}...{}",
        &config.get_api_key()[..10.min(config.get_api_key().len())],
        &config.get_api_key()[config.get_api_key().len().saturating_sub(4)..]
    );
    log::info!("Validating token: {}", token_preview);

    if !auth::validate_token(&config.server_url, config.get_api_key()) {
        println!("Token invalid or expired. Re-authenticating...");
        let token_response = auth::device_flow(&config.server_url)?;

        if using_env_var {
            // Don't save to config - user is managing token via env var
            // Just print instructions
            println!();
            println!("New tokens obtained. Update your environment variables:");
            println!("  export BOTSTER_TOKEN={}", token_response.access_token);
            if let Some(ref mcp_token) = token_response.mcp_token {
                println!("  export BOTSTER_MCP_TOKEN={}", mcp_token);
            }
            println!();
            anyhow::bail!("Please update your environment variables and restart.");
        }
        save_tokens(&mut config, &token_response)?;
        println!("Tokens saved successfully.");

        // Verify the token was saved correctly by reloading
        let verify_config = Config::load()?;
        if !verify_config.has_token() {
            anyhow::bail!(
                "Token was not saved correctly to keyring. \
                 This may be a permissions issue with your system keychain."
            );
        }
        log::info!(
            "New token saved and verified: {}...{}",
            &verify_config.get_api_key()[..10.min(verify_config.get_api_key().len())],
            &verify_config.get_api_key()
                [verify_config.get_api_key().len().saturating_sub(4)..]
        );
    } else {
        println!("  Authentication valid.");
    }

    Ok(())
}

/// Save both hub and MCP tokens from auth response.
fn save_tokens(
    config: &mut Config,
    token_response: &botster_hub::auth::TokenResponse,
) -> Result<()> {
    use botster_hub::keyring::Credentials;

    // Save hub token via config (which updates Credentials internally)
    config.save_token(&token_response.access_token)?;

    // Save MCP token if provided
    if let Some(ref mcp_token) = token_response.mcp_token {
        let mut creds = Credentials::load().unwrap_or_default();
        creds.set_mcp_token(mcp_token.clone());
        creds.save()?;
        log::info!("Saved MCP token to credentials");
    }

    Ok(())
}

/// Runs the hub in headless mode (no TUI).
///
/// This mode is useful for:
/// - Integration testing (system tests spawn CLI headless)
/// - Running as a background daemon
/// - CI/CD environments without a terminal
fn run_headless() -> Result<()> {
    println!("Starting Botster Hub v{} in headless mode...", VERSION);

    // Ensure we have a valid authentication token
    ensure_authenticated()?;

    // Check for updates (non-interactive — logs warning only)
    let _ = commands::update::check_on_boot_headless();

    // Set up signal handlers
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::flag;
    flag::register(SIGINT, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGTERM, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGHUP, Arc::clone(&SHUTDOWN_FLAG))?;

    // Create Hub with default terminal size (80x24 for headless)
    let config = Config::load()?;

    // Verify token is available after load - catches keyring save/load issues
    // Skip in test mode since ensure_authenticated() skips auth entirely
    if !botster_hub::env::is_test_mode() && !config.has_token() {
        anyhow::bail!(
            "Authentication token not found after auth flow. \
             This may indicate a keyring access issue. \
             Try running 'botster-hub reset' and re-authenticating."
        );
    }

    if config.has_token() {
        log::info!(
            "Token loaded: {}...{} (valid format)",
            &config.get_api_key()[..10.min(config.get_api_key().len())],
            &config.get_api_key()[config.get_api_key().len().saturating_sub(4)..]
        );
    }

    let mut hub = Hub::new(config)?;

    println!("Setting up connections...");
    hub.setup();

    // In headless mode, eagerly generate the connection URL so external
    // tools (system tests, automation) can read it from connection_url.txt
    // without needing a TUI interaction to trigger lazy generation.
    hub.eager_generate_connection_url();

    println!("Hub ready. Waiting for connections...");
    log::info!("Botster Hub v{} started in headless mode", VERSION);

    // In headless mode, run a simplified event loop
    // - Poll for messages and send heartbeats via tick()
    // - Route PTY output to viewing clients
    while !SHUTDOWN_FLAG.load(std::sync::atomic::Ordering::Relaxed) {
        // Poll for messages and send heartbeats
        hub.tick();

        // Sleep to avoid busy-looping (100ms = 10 ticks/sec is plenty for headless)
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("Shutting down...");
    hub.shutdown();

    Ok(())
}

/// Run the hub using the Hub architecture with TUI.
///
/// This sets up the terminal and delegates to tui::run_with_hub() for the event loop.
/// The TUI module now owns TuiRunner instantiation, maintaining proper layer separation
/// (Hub should not know about TUI implementation details).
fn run_with_tui() -> Result<()> {
    // Ensure we have a valid authentication token
    ensure_authenticated()?;

    // Check for updates (at most once per 24h, never blocks startup on failure)
    let _ = commands::update::check_on_boot();

    // Set up signal handlers
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::flag;
    flag::register(SIGINT, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGTERM, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGHUP, Arc::clone(&SHUTDOWN_FLAG))?;

    // Create Hub BEFORE entering raw mode so errors are visible
    println!("Initializing hub...");
    let config = Config::load()?;

    // Verify token is available after load - catches keyring save/load issues
    // Skip in test mode since ensure_authenticated() skips auth entirely
    if !botster_hub::env::is_test_mode() && !config.has_token() {
        anyhow::bail!(
            "Authentication token not found after auth flow. \
             This may indicate a keyring access issue. \
             Try running 'botster-hub reset' and re-authenticating."
        );
    }

    if config.has_token() {
        log::info!(
            "Token loaded: {}...{} (valid format)",
            &config.get_api_key()[..10.min(config.get_api_key().len())],
            &config.get_api_key()[config.get_api_key().len().saturating_sub(4)..]
        );
    }
    let mut hub = Hub::new(config)?;

    // Perform setup BEFORE entering raw mode so errors are visible
    println!("Setting up connections...");
    hub.setup();

    println!("Starting TUI...");

    // NOW setup terminal (after all initialization that could fail)
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    // Kitty keyboard protocol is NOT pushed here — it's mirrored dynamically
    // from the inner PTY's state by sync_terminal_modes() in the event loop.

    let _terminal_guard = tui::TerminalGuard::new();

    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;

    log::info!("Botster Hub v{} started with TUI", VERSION);

    // Run the event loop - TUI module now owns TuiRunner instantiation
    tui::run_with_hub(&mut hub, terminal, &*SHUTDOWN_FLAG)?;

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
    /// Get the connection URL for a running hub (for testing/automation)
    GetConnectionUrl {
        /// Hub identifier
        #[arg(long)]
        hub: String,
    },
    /// Remove all botster data (credentials, config, device identity)
    Reset {
        /// Skip confirmation prompt
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

fn main() -> Result<()> {
    // Set up file logging so TUI doesn't interfere with log output
    // Use BOTSTER_LOG_FILE or BOTSTER_CONFIG_DIR/botster-hub.log or fallback
    let log_path = if let Ok(path) = std::env::var("BOTSTER_LOG_FILE") {
        std::path::PathBuf::from(path)
    } else if let Ok(config_dir) = std::env::var("BOTSTER_CONFIG_DIR") {
        std::path::PathBuf::from(config_dir).join("botster-hub.log")
    } else if botster_hub::env::is_any_test() {
        // Test mode: use project tmp/ to avoid leaking outside the project
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.join("tmp/botster-hub.log"))
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp/botster-hub.log"))
    } else {
        std::path::PathBuf::from("/tmp/botster-hub.log")
    };
    let log_file = std::fs::File::create(&log_path)
        .unwrap_or_else(|_| panic!("Failed to create log file at {:?}", log_path));
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

        // Reset mirrored terminal modes
        let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x1b[?1l");    // Reset DECCKM
        let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x1b[?2004l"); // Reset bracketed paste
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);

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
                // Use the TUI-based architecture (proper layer separation)
                run_with_tui()?;
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
        Commands::GetConnectionUrl { hub } => {
            use botster_hub::relay::read_connection_url;
            match read_connection_url(&hub)? {
                Some(url) => {
                    println!("{}", url);
                }
                None => {
                    eprintln!(
                        "No connection URL found for hub '{}'. Is the CLI running?",
                        hub
                    );
                    std::process::exit(1);
                }
            }
        }
        Commands::Reset { yes } => {
            commands::reset::run(yes)?;
        }
    }

    Ok(())
}

