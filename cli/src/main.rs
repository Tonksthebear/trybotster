//! Botster Hub CLI - manages autonomous Claude agents for GitHub issues.
//!
//! This is the main binary entry point. See the `botster` library
//! for the core functionality.

use anyhow::{Context, Result};
use botster::{commands, tui, Config, Hub, HubRegistry};
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
    use botster::auth;

    // Skip auth validation in test mode (BOTSTER_ENV=test)
    if botster::env::is_test_mode() {
        log::info!("Skipping authentication (BOTSTER_ENV=test)");
        return Ok(());
    }

    let mut config = Config::load()?;
    let using_env_var =
        std::env::var("BOTSTER_TOKEN").is_ok() || std::env::var("BOTSTER_API_KEY").is_ok();

    // Check if we have a token at all
    if !config.has_token() {
        // Check credential storage before first save — warns once during auth
        botster::keyring::check_credential_storage()?;

        // Name the device, then authenticate
        let device_name = auth::prompt_device_name()?;

        // Update device identity with user-chosen name
        if let Ok(mut device) = botster::device::Device::load_or_create() {
            device.name = device_name;
            if let Err(e) = device.save() {
                log::warn!("Failed to save device name: {e}");
            }
        }

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
        // Check credential storage before saving — no-op if already checked
        botster::keyring::check_credential_storage()?;
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
    token_response: &botster::auth::TokenResponse,
) -> Result<()> {
    use botster::keyring::Credentials;

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

/// Ensure the current directory has a hub name in the registry.
///
/// Computes the hub_identifier for the current directory, checks the registry,
/// and prompts the user if this is a new directory. Sets `config.hub_name` to
/// the looked-up or newly-chosen name so `Hub::new()` can pass it to the server.
fn ensure_hub_named(config: &mut Config) -> Result<()> {
    use botster::auth;
    use botster::hub::hub_id_for_repo;

    if botster::env::is_test_mode() {
        return Ok(());
    }

    // Compute hub_identifier using the same logic as Hub::new()
    let (hub_id, repo_path) = if let Ok(id) = std::env::var("BOTSTER_HUB_ID") {
        (id, None)
    } else {
        match botster::WorktreeManager::detect_current_repo() {
            Ok((path, _)) => {
                let id = hub_id_for_repo(&path);
                let canonical = path.canonicalize().unwrap_or(path);
                (id, Some(canonical.to_string_lossy().to_string()))
            }
            Err(_) => {
                let cwd = std::env::current_dir()?;
                let id = hub_id_for_repo(&cwd);
                let canonical = cwd.canonicalize().unwrap_or(cwd);
                (id, Some(canonical.to_string_lossy().to_string()))
            }
        }
    };

    let mut registry = HubRegistry::load();

    if let Some(name) = registry.get_hub_name(&hub_id) {
        // Already registered — use cached name
        config.hub_name = Some(name.to_string());
    } else if registry.is_empty() && config.hub_name.is_some() {
        // Migrate from old global hub_name to per-directory registry.
        // Only if registry is empty (first migration) — otherwise the legacy
        // name was for a different directory and should not be reused.
        let legacy_name = config.hub_name.as_ref().unwrap();
        log::info!("Migrating legacy hub_name '{}' to registry", legacy_name);
        registry.set_hub_name(&hub_id, legacy_name.clone(), repo_path);
        registry.save()?;
    } else if atty::is(atty::Stream::Stdin) {
        // New directory, interactive — prompt for hub name
        let name = auth::prompt_hub_name()?;
        registry.set_hub_name(&hub_id, name.clone(), repo_path);
        registry.save()?;
        config.hub_name = Some(name);
    } else {
        // New directory, non-interactive — auto-name from repo or dir basename
        let name = std::env::var("BOTSTER_REPO")
            .ok()
            .or_else(|| {
                botster::WorktreeManager::detect_current_repo()
                    .map(|(_, name)| name)
                    .ok()
            })
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            })
            .unwrap_or_else(|| "my-hub".to_string());
        log::info!("Auto-naming hub: {name} (non-interactive)");
        registry.set_hub_name(&hub_id, name.clone(), repo_path);
        registry.save()?;
        config.hub_name = Some(name);
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

    // Check for updates first (non-interactive — logs warning only)
    let _ = commands::update::check_on_boot_headless();

    // Ensure we have a valid authentication token
    ensure_authenticated()?;

    // Set up signal handlers
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::flag;
    flag::register(SIGINT, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGTERM, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGHUP, Arc::clone(&SHUTDOWN_FLAG))?;

    // Create Hub with default terminal size (80x24 for headless)
    let mut config = Config::load()?;

    // Verify token is available after load - catches keyring save/load issues
    // Skip in test mode since ensure_authenticated() skips auth entirely
    if !botster::env::is_test_mode() && !config.has_token() {
        anyhow::bail!(
            "Authentication token not found after auth flow. \
             This may indicate a keyring access issue. \
             Try running 'botster reset' and re-authenticating."
        );
    }

    if config.has_token() {
        log::info!(
            "Token loaded: {}...{} (valid format)",
            &config.get_api_key()[..10.min(config.get_api_key().len())],
            &config.get_api_key()[config.get_api_key().len().saturating_sub(4)..]
        );
    }

    // Ensure this directory has a hub name in the registry
    ensure_hub_named(&mut config)?;

    let mut hub = Hub::new(config)?;

    println!("Setting up connections...");
    hub.setup();

    // Start socket server for IPC (allows `botster attach` and plugin access)
    hub.start_socket_server();

    // In headless mode, eagerly generate the connection URL so external
    // tools (system tests, automation) can read it from connection_url.txt
    // without needing a TUI interaction to trigger lazy generation.
    hub.eager_generate_connection_url();

    println!("Hub ready. Waiting for connections...");
    log::info!("Botster Hub v{} started in headless mode", VERSION);

    // Fully event-driven headless loop — uses tokio::select! to sleep
    // between events. No periodic polling.
    hub.run_headless(&SHUTDOWN_FLAG)?;

    println!("Shutting down...");
    let should_restart = hub.exec_restart;
    hub.shutdown();

    if should_restart {
        commands::update::exec_restart()?;
    }

    Ok(())
}

/// Run the hub using the Hub architecture with TUI.
///
/// This sets up the terminal and delegates to tui::run_with_hub() for the event loop.
/// The TUI module now owns TuiRunner instantiation, maintaining proper layer separation
/// (Hub should not know about TUI implementation details).
fn run_with_tui() -> Result<()> {
    // Require an interactive terminal for TUI mode
    if !atty::is(atty::Stream::Stdin) {
        anyhow::bail!(
            "Error: 'start' requires an interactive terminal (stdin is not a TTY).\n\
             Use 'botster start --headless' for non-interactive mode."
        );
    }

    // Check for updates first — show errors so the user knows if an update failed
    if let Err(e) = commands::update::check_on_boot() {
        eprintln!("Update failed: {e:#}");
    }

    // Ensure we have a valid authentication token
    ensure_authenticated()?;

    // Set up signal handlers
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::flag;
    flag::register(SIGINT, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGTERM, Arc::clone(&SHUTDOWN_FLAG))?;
    flag::register(SIGHUP, Arc::clone(&SHUTDOWN_FLAG))?;

    // Create Hub BEFORE entering raw mode so errors are visible
    println!("Initializing hub...");
    let mut config = Config::load()?;

    // Verify token is available after load - catches keyring save/load issues
    // Skip in test mode since ensure_authenticated() skips auth entirely
    if !botster::env::is_test_mode() && !config.has_token() {
        anyhow::bail!(
            "Authentication token not found after auth flow. \
             This may indicate a keyring access issue. \
             Try running 'botster reset' and re-authenticating."
        );
    }

    if config.has_token() {
        log::info!(
            "Token loaded: {}...{} (valid format)",
            &config.get_api_key()[..10.min(config.get_api_key().len())],
            &config.get_api_key()[config.get_api_key().len().saturating_sub(4)..]
        );
    }

    // Ensure this directory has a hub name in the registry
    ensure_hub_named(&mut config)?;

    let mut hub = Hub::new(config)?;

    // Perform setup BEFORE entering raw mode so errors are visible
    println!("Setting up connections...");
    hub.setup();

    // Start socket server for IPC (allows `botster attach` and plugin access)
    hub.start_socket_server();

    println!("Starting TUI...");

    // NOW setup terminal (after all initialization that could fail)
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, crossterm::event::EnableFocusChange)?;

    // Kitty keyboard protocol is NOT pushed here — it's mirrored dynamically
    // from the inner PTY's state by sync_terminal_modes() in the event loop.

    let _terminal_guard = tui::TerminalGuard::new();

    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;

    log::info!("Botster Hub v{} started with TUI", VERSION);

    // Run the event loop - TUI module now owns TuiRunner instantiation
    tui::run_with_hub(&mut hub, terminal, &*SHUTDOWN_FLAG)?;

    // Shutdown
    let should_restart = hub.exec_restart;
    hub.shutdown();

    if should_restart {
        commands::update::exec_restart()?;
    }

    Ok(())
}

// CLI
#[derive(Parser)]
#[command(name = "botster")]
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
    /// Update botster to the latest version
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
    /// Attach a TUI to a running headless hub (like tmux attach)
    Attach {
        /// Hub identifier or name (defaults to current directory)
        #[arg(long)]
        hub: Option<String>,
    },
}

/// Raise the process file descriptor limit to accommodate WebRTC connections.
///
/// Each WebRTC peer connection opens ~15 UDP sockets for ICE candidate
/// gathering. Combined with webrtc-rs's 60s graceful SCTP shutdown, the
/// default macOS limit of 256 fds exhausts after just a few rapid reconnects.
fn raise_fd_limit() {
    unsafe {
        let mut rlim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) == 0 {
            let target = 4096.min(rlim.rlim_max);
            if rlim.rlim_cur < target {
                rlim.rlim_cur = target;
                libc::setrlimit(libc::RLIMIT_NOFILE, &rlim);
            }
        }
    }
}

/// RAII guard for a pair of pipe file descriptors.
///
/// Closes both fds on drop, preventing leaks if `run_attach` exits early.
struct WakePipe {
    read_fd: Option<i32>,
    write_fd: Option<i32>,
}

impl WakePipe {
    fn new() -> Self {
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } == 0 {
            unsafe {
                let flags = libc::fcntl(fds[0], libc::F_GETFL);
                libc::fcntl(fds[0], libc::F_SETFL, flags | libc::O_NONBLOCK);
                let flags = libc::fcntl(fds[1], libc::F_GETFL);
                libc::fcntl(fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
            Self { read_fd: Some(fds[0]), write_fd: Some(fds[1]) }
        } else {
            Self { read_fd: None, write_fd: None }
        }
    }

    fn read_fd(&self) -> Option<i32> { self.read_fd }
    fn write_fd(&self) -> Option<i32> { self.write_fd }
}

impl Drop for WakePipe {
    fn drop(&mut self) {
        if let Some(fd) = self.read_fd {
            unsafe { libc::close(fd); }
        }
        if let Some(fd) = self.write_fd {
            unsafe { libc::close(fd); }
        }
    }
}

/// Attach a TUI to a running headless hub via Unix domain socket.
/// Derive the hub_id for the current working directory.
///
/// Returns `None` only if both repo detection and `current_dir()` fail.
fn resolve_hub_id_for_cwd() -> Option<String> {
    let repo_path = match botster::WorktreeManager::detect_current_repo() {
        Ok((path, _)) => path,
        Err(_) => std::env::current_dir().ok()?,
    };
    Some(botster::hub::hub_id_for_repo(&repo_path))
}

///
/// Discovers a running hub (by directory or explicit `--hub` arg),
/// connects to its socket, and runs the TUI with a bridge adapter.
fn run_attach(hub_arg: Option<String>) -> Result<()> {
    use std::sync::atomic::Ordering;
    use botster::hub::daemon;
    use botster::socket::tui_bridge::TuiBridge;

    // Require an interactive terminal
    if !atty::is(atty::Stream::Stdin) {
        anyhow::bail!("Error: 'attach' requires an interactive terminal (stdin is not a TTY).");
    }

    // Resolve hub_id
    let hub_id = if let Some(ref arg) = hub_arg {
        arg.clone()
    } else {
        // Derive from current directory (same logic as Hub::new)
        let repo_path = match botster::WorktreeManager::detect_current_repo() {
            Ok((path, _)) => path,
            Err(_) => std::env::current_dir()?,
        };
        botster::hub::hub_id_for_repo(&repo_path)
    };

    // Check if hub is running
    if !daemon::is_hub_running(&hub_id) {
        // Try to find any running hub
        let running = daemon::discover_running_hubs();
        if running.is_empty() {
            anyhow::bail!(
                "No running hub found for this directory.\n\
                 Start one with: botster start --headless"
            );
        } else {
            eprintln!("No running hub found for this directory.");
            eprintln!("Running hubs:");
            for (id, pid) in &running {
                eprintln!("  {} (pid={})", &id[..id.len().min(8)], pid);
            }
            anyhow::bail!("Use --hub <id> to specify which hub to attach to.");
        }
    }

    let socket_path = daemon::socket_path(&hub_id)?;
    if !socket_path.exists() {
        anyhow::bail!(
            "Hub is running (pid={}) but socket not found at {}",
            daemon::read_pid_file(&hub_id).unwrap_or(0),
            socket_path.display()
        );
    }

    println!("Connecting to hub {}...", &hub_id[..hub_id.len().min(8)]);

    // Create tokio runtime for the bridge
    let rt = tokio::runtime::Runtime::new()?;

    // Connect to socket
    let stream = rt.block_on(async {
        tokio::net::UnixStream::connect(&socket_path).await
    }).with_context(|| format!("Failed to connect to socket: {}", socket_path.display()))?;

    // Create wake pipe for TuiRunner (RAII guard ensures cleanup on any exit path)
    let pipe = WakePipe::new();

    let shutdown = Arc::new(AtomicBool::new(false));

    // Create bridge
    let (bridge, channels) = TuiBridge::connect(stream, pipe.write_fd(), Arc::clone(&shutdown));

    // Set up terminal — create guard BEFORE execute! so raw mode is restored
    // even if the crossterm commands fail.
    enable_raw_mode()?;
    let _terminal_guard = tui::TerminalGuard::new();

    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, crossterm::event::EnableFocusChange)?;

    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;

    // Calculate terminal dimensions
    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let (inner_rows, inner_cols) = tui::terminal_widget_inner_area(term_cols, term_rows);

    // Create TuiRunner with bridge channels (same as run_with_hub)
    let tui_shutdown = Arc::clone(&shutdown);
    let mut tui_runner = tui::TuiRunner::new(
        terminal,
        channels.request_tx,
        channels.output_rx,
        tui_shutdown,
        (inner_rows, inner_cols),
        pipe.read_fd(),
    );
    tui_runner.set_lua_bootstrap(tui::hot_reload::LuaBootstrap::load());

    // Register SIGWINCH
    #[cfg(unix)]
    {
        use signal_hook::consts::signal::SIGWINCH;
        if let Err(e) = signal_hook::flag::register(SIGWINCH, tui_runner.resize_flag()) {
            log::warn!("Failed to register SIGWINCH handler: {e}");
        }
    }

    // Register SIGINT/SIGTERM
    {
        use signal_hook::consts::signal::{SIGINT, SIGTERM};
        use signal_hook::flag;
        flag::register(SIGINT, Arc::clone(&SHUTDOWN_FLAG))?;
        flag::register(SIGTERM, Arc::clone(&SHUTDOWN_FLAG))?;
    }

    // Spawn TUI thread
    let tui_handle = std::thread::Builder::new()
        .name("tui-runner".to_string())
        .spawn(move || {
            if let Err(e) = tui_runner.run() {
                log::error!("TuiRunner error: {}", e);
            }
        })?;

    // Run bridge in tokio runtime (blocks until shutdown)
    let bridge_shutdown = Arc::clone(&shutdown);
    rt.block_on(async move {
        tokio::select! {
            _ = bridge.run() => {}
            _ = async {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    if bridge_shutdown.load(Ordering::Relaxed) || SHUTDOWN_FLAG.load(Ordering::Relaxed) {
                        break;
                    }
                }
            } => {}
        }
    });

    // Signal TUI shutdown
    shutdown.store(true, Ordering::SeqCst);

    // Wait for TUI thread
    if let Err(e) = tui_handle.join() {
        log::error!("TuiRunner thread panicked: {:?}", e);
    }

    // pipe fds closed automatically by WakePipe drop

    Ok(())
}

fn main() -> Result<()> {
    // Raise fd limit for WebRTC. Each peer connection opens ~15 UDP sockets
    // for ICE gathering, and webrtc-rs close() takes up to 60s for SCTP
    // shutdown. macOS defaults to 256 fds which exhausts after ~4 rapid
    // reconnects. Every production WebRTC server raises this.
    raise_fd_limit();

    // Set up file logging so TUI doesn't interfere with log output
    // Use BOTSTER_LOG_FILE or BOTSTER_CONFIG_DIR/botster.log or fallback
    let log_path = if let Ok(path) = std::env::var("BOTSTER_LOG_FILE") {
        std::path::PathBuf::from(path)
    } else if let Ok(config_dir) = std::env::var("BOTSTER_CONFIG_DIR") {
        std::path::PathBuf::from(config_dir).join("botster.log")
    } else if botster::env::is_any_test() {
        // Test mode: use project tmp/ to avoid leaking outside the project
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.join("tmp/botster.log"))
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp/botster.log"))
    } else {
        std::path::PathBuf::from("/tmp/botster.log")
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
                // If a hub is already running for this directory AND has a
                // socket, attach to it (tmux-like behavior). If the socket
                // is missing (legacy hub started before socket support),
                // fall through to starting a new TUI.
                let can_attach = resolve_hub_id_for_cwd().is_some_and(|id| {
                    botster::hub::daemon::is_hub_running(&id)
                        && botster::hub::daemon::socket_path(&id)
                            .map(|p| p.exists())
                            .unwrap_or(false)
                });
                if can_attach {
                    println!("Hub already running — attaching...");
                    run_attach(None)?;
                } else {
                    run_with_tui()?;
                }
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
        Commands::Update { check } => {
            if check {
                commands::update::check()?;
            } else {
                commands::update::install()?;
            }
        }
        Commands::GetConnectionUrl { hub } => {
            use botster::relay::read_connection_url;
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
        Commands::Attach { hub: hub_arg } => {
            run_attach(hub_arg)?;
        }
    }

    Ok(())
}

