//! PTY session management with event-driven broadcasting.
//!
//! This module provides pseudo-terminal (PTY) session handling with a pub/sub
//! architecture. PTY sessions broadcast events to connected clients, and each
//! client maintains its own terminal state (vt100 parser, etc.).
//!
//! # Architecture
//!
//! ```text
//! PtySession (owns I/O, broadcasts events)
//!  ├── master_pty: MasterPty (for resizing)
//!  ├── writer: Write (for input)
//!  ├── reader_thread: JoinHandle (PTY output reader)
//!  ├── child: Child (spawned process)
//!  ├── shadow_screen: Arc<Mutex<vt100::Parser>> (parsed terminal state)
//!  └── event_tx: broadcast::Sender<PtyEvent> (output + notification broadcast)
//! ```
//!
//! # Shadow Terminal (zmx pattern)
//!
//! Each PTY session maintains a `vt100::Parser` shadow screen that receives
//! the same bytes as live subscribers. On browser connect/reconnect, the shadow
//! screen produces a clean ANSI snapshot via `contents_formatted()` instead of
//! replaying raw byte history — eliminating escape sequence garbling and cursor
//! desync. Live streaming still uses raw PTY bytes for efficiency.
//!
//! # Event Broadcasting
//!
//! PTY sessions emit [`PtyEvent`]s to all subscribers via a broadcast channel:
//! - [`PtyEvent::Output`] - Raw terminal output bytes
//! - [`PtyEvent::Resized`] - PTY dimensions changed
//! - [`PtyEvent::ProcessExited`] - Process terminated
//!
//! # Client Tracking
//!
//! Client connection tracking and size ownership are managed by Lua.
//! Rust PTY sessions provide only the I/O primitives (resize, write,
//! subscribe, snapshot).
//!
//! # Thread Safety
//!
//! The shadow screen is wrapped in `Arc<Mutex<>>` to allow concurrent
//! access from the PTY reader thread and snapshot requests.

// Rust guideline compliant 2026-02

mod commands;
pub mod events;

pub use commands::PtyCommand;
pub use events::{PromptMark, PtyEvent};

pub use super::spawn::PtySpawnConfig;

use anyhow::{Context, Result};
use portable_pty::{Child, MasterPty, PtySize};
use std::{
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::agent::spawn;

/// Default channel capacity for PTY command channels.
const PTY_COMMAND_CHANNEL_CAPACITY: usize = 64;

/// Number of scrollback lines tracked by the shadow terminal.
///
/// 5000 lines at 80 cols produces ~400KB of formatted ANSI output
/// on snapshot. This provides ample history for reconnecting browsers
/// while keeping memory bounded by line count rather than raw bytes.
pub(crate) const SHADOW_SCROLLBACK_LINES: usize = 5000;

/// Minimum row count for vt100 parser to avoid col_wrap overflow panic.
///
/// vt100 0.16.2 has a bug where a 1-row grid causes `col_wrap()` to
/// underflow (`prev_pos.row -= scrolled` where both are 0 and 1). This
/// affects both `set_size()` and `Parser::new()`. Clamping to 2 rows
/// avoids the impossible state while being functionally equivalent (a
/// 1-row terminal is unusable anyway).
pub(crate) const MIN_PARSER_ROWS: u16 = 2;

/// Default broadcast channel capacity.
///
/// This determines how many events can be buffered before slow receivers
/// start missing events. Set high enough to handle bursts of output.
const BROADCAST_CHANNEL_CAPACITY: usize = 1024;

/// PTY index constants for channel routing.
pub mod pty_index {
    /// CLI PTY index (main agent process).
    pub const CLI: usize = 0;
    /// Server PTY index (dev server process).
    pub const SERVER: usize = 1;
}

/// Shared mutable state for PTY command processing.
///
/// This struct holds state that needs concurrent access from both the
/// command processor task and the main `PtySession`. All fields are
/// wrapped in the outer `Mutex` of `PtySession::shared_state`.
///
/// Exposed as `pub(crate)` to allow direct sync I/O from `PtyHandle`.
pub struct SharedPtyState {
    /// Master PTY for resizing operations.
    pub(crate) master_pty: Option<Box<dyn MasterPty + Send>>,

    /// Writer for sending input to the PTY.
    pub(crate) writer: Option<Box<dyn Write + Send>>,

    /// Current PTY dimensions (rows, cols).
    pub(crate) dimensions: (u16, u16),
}

impl std::fmt::Debug for SharedPtyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedPtyState")
            .field("has_master_pty", &self.master_pty.is_some())
            .field("has_writer", &self.writer.is_some())
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

/// Encapsulates all state for a single PTY session.
///
/// Each PTY session manages:
/// - A pseudo-terminal for process I/O
/// - A shadow terminal (`vt100::Parser`) for clean ANSI snapshots on reconnect
/// - A broadcast channel for event distribution to clients
/// - An optional port for HTTP forwarding (used by server PTY for dev server preview)
///
/// # Shadow Terminal
///
/// The shadow screen receives the same raw bytes as live subscribers and
/// maintains parsed terminal state. On browser connect/reconnect,
/// `get_snapshot()` returns clean ANSI output via `contents_formatted()`
/// instead of replaying raw bytes — no garbling, correct cursor position.
///
/// # Event Broadcasting
///
/// Output and lifecycle events are broadcast to all subscribers via
/// [`PtyEvent`]. Clients subscribe via [`subscribe()`](Self::subscribe).
///
/// # Client Tracking
///
/// Client connection tracking and size ownership are managed by Lua.
/// Rust provides only the I/O primitives (resize, write, subscribe, snapshot).
///
/// # Command Processing
///
/// After spawning, call [`spawn_command_processor()`](Self::spawn_command_processor)
/// to start the background task that processes commands from `PtyHandle` clients.
/// The processor handles Input commands.
///
/// # Thread Safety
///
/// The shadow screen and shared state are wrapped in `Arc<Mutex<>>` to allow
/// concurrent access from the PTY reader thread, command processor task, and main
/// event loop.
pub struct PtySession {
    /// Shared mutable state accessed by the command processor task.
    ///
    /// Contains: master_pty, writer, dimensions.
    shared_state: Arc<Mutex<SharedPtyState>>,

    /// Reader thread handle.
    pub reader_thread: Option<thread::JoinHandle<()>>,

    /// Command processor task handle.
    command_processor_handle: Option<JoinHandle<()>>,

    /// Child process handle - stored so we can kill it on drop.
    child: Option<Box<dyn Child + Send>>,

    /// Shadow terminal for clean ANSI snapshots on reconnect.
    ///
    /// Receives the same PTY bytes as live subscribers. On connect,
    /// `contents_formatted()` produces clean ANSI output with correct
    /// cursor position — no escape sequence garbling from raw replay.
    pub shadow_screen: Arc<Mutex<vt100::Parser>>,

    /// Broadcast sender for PTY events.
    ///
    /// All output and lifecycle events are broadcast through this channel.
    /// Clients receive events by subscribing to this sender.
    event_tx: broadcast::Sender<PtyEvent>,

    /// Command sender for PTY operations.
    ///
    /// Clients send commands (input) through this
    /// channel. The receiver is consumed by the command processor task.
    command_tx: mpsc::Sender<PtyCommand>,

    /// Command receiver for PTY operations.
    ///
    /// Taken by [`spawn_command_processor()`](Self::spawn_command_processor)
    /// to be processed in a background task.
    command_rx: Option<mpsc::Receiver<PtyCommand>>,

    /// Whether notification detection is enabled for this session.
    ///
    /// When true, the reader thread broadcasts [`PtyEvent::Notification`]
    /// events for detected OSC 9 / OSC 777 sequences.
    detect_notifications: bool,

    /// Whether the inner PTY application has pushed kitty keyboard protocol.
    ///
    /// Set by the reader thread when it detects `CSI > flags u` (push) or
    /// `CSI < u` (pop) in the PTY output stream. Used by `get_snapshot()`
    /// to include the kitty push sequence so browser terminals enter kitty
    /// mode on connect/reconnect.
    kitty_enabled: Arc<AtomicBool>,

    /// Allocated port for HTTP forwarding.
    ///
    /// Used by sessions with `port_forward` enabled to expose the dev server
    /// port for preview proxying. The port is set via [`set_port()`](Self::set_port)
    /// and queried via [`port()`](Self::port).
    ///
    /// When spawning the PTY process, the caller passes this port via the
    /// `PORT` environment variable.
    port: Option<u16>,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self
            .shared_state
            .lock()
            .expect("shared_state lock poisoned");
        f.debug_struct("PtySession")
            .field("has_master_pty", &state.master_pty.is_some())
            .field("has_writer", &state.writer.is_some())
            .field("has_reader_thread", &self.reader_thread.is_some())
            .field("has_child", &self.child.is_some())
            .field("detect_notifications", &self.detect_notifications)
            .field(
                "has_command_processor",
                &self.command_processor_handle.is_some(),
            )
            .finish_non_exhaustive()
    }
}

impl PtySession {
    /// Creates a new PTY session with the specified dimensions.
    ///
    /// The broadcast channel is initialized with sufficient capacity for
    /// burst output. No clients are connected initially.
    #[must_use]
    pub fn new(rows: u16, cols: u16) -> Self {
        let (event_tx, _) = broadcast::channel(BROADCAST_CHANNEL_CAPACITY);
        let (command_tx, command_rx) = mpsc::channel(PTY_COMMAND_CHANNEL_CAPACITY);

        let shared_state = SharedPtyState {
            master_pty: None,
            writer: None,
            dimensions: (rows, cols),
        };

        Self {
            shared_state: Arc::new(Mutex::new(shared_state)),
            reader_thread: None,
            command_processor_handle: None,
            child: None,
            shadow_screen: Arc::new(Mutex::new(
                vt100::Parser::new(rows.max(MIN_PARSER_ROWS), cols, SHADOW_SCROLLBACK_LINES),
            )),
            event_tx,
            command_tx,
            command_rx: Some(command_rx),
            detect_notifications: false,
            kitty_enabled: Arc::new(AtomicBool::new(false)),
            port: None,
        }
    }

    /// Get the current PTY dimensions (rows, cols).
    #[must_use]
    pub fn dimensions(&self) -> (u16, u16) {
        self.shared_state
            .lock()
            .expect("shared_state lock poisoned")
            .dimensions
    }

    /// Set the master PTY handle.
    ///
    /// Called by spawn functions after creating the PTY.
    pub fn set_master_pty(&mut self, master_pty: Box<dyn MasterPty + Send>) {
        self.shared_state
            .lock()
            .expect("shared_state lock poisoned")
            .master_pty = Some(master_pty);
    }

    /// Set the PTY writer.
    ///
    /// Called by spawn functions after creating the PTY.
    pub fn set_writer(&mut self, writer: Box<dyn Write + Send>) {
        self.shared_state
            .lock()
            .expect("shared_state lock poisoned")
            .writer = Some(writer);
    }

    /// Set the HTTP forwarding port for this PTY.
    ///
    /// Called after spawning to record the allocated port. Used for sessions
    /// with `port_forward` enabled.
    ///
    /// Note: The port is also passed via the `PORT` env var when spawning
    /// the PTY process.
    pub fn set_port(&mut self, port: u16) {
        self.port = Some(port);
    }

    /// Get the HTTP forwarding port for this PTY.
    ///
    /// Returns the port allocated for HTTP preview proxying, or `None` if
    /// no port has been assigned.
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    // =========================================================================
    // Unified Spawn
    // =========================================================================

    /// Spawn a process in this PTY session.
    ///
    /// This is the single entry point for spawning processes. The behavior
    /// is configured via [`PtySpawnConfig`]:
    ///
    /// - Set `detect_notifications: true` for CLI sessions that need OSC
    ///   notification detection.
    /// - Set `detect_notifications: false` for server sessions that only
    ///   need output broadcasting.
    ///
    /// After calling this method, the PtySession is fully configured with
    /// a running process, reader thread, and command processor.
    ///
    /// # Arguments
    ///
    /// * `config` - Spawn configuration including command, env, init commands, etc.
    ///
    /// # Errors
    ///
    /// Returns an error if PTY creation, command spawn, or writer setup fails.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut pty = PtySession::new(24, 80);
    /// pty.spawn(PtySpawnConfig {
    ///     worktree_path: PathBuf::from("/path/to/worktree"),
    ///     command: "bash".to_string(),
    ///     env: HashMap::new(),
    ///     init_commands: vec!["source .botster/shared/sessions/agent/initialization".to_string()],
    ///     detect_notifications: true,
    ///     port: None,
    ///     context: String::new(),
    /// })?;
    /// ```
    pub fn spawn(&mut self, config: PtySpawnConfig) -> Result<()> {
        // Set port if provided
        if let Some(port) = config.port {
            self.set_port(port);
        }

        // Open PTY pair with current dimensions
        let (rows, cols) = self.dimensions();
        let pair = spawn::open_pty(rows, cols)?;

        // Build and spawn command
        let cmd = spawn::build_command(&config.command, &config.worktree_path, &config.env);
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("Failed to spawn command")?;

        // Track notification detection flag
        self.detect_notifications = config.detect_notifications;

        // Configure PTY with spawned resources
        self.set_child(child);
        self.set_writer(pair.master.take_writer()?);

        // Start unified reader thread
        let reader = pair.master.try_clone_reader()?;
        self.reader_thread = Some(spawn::spawn_reader_thread(
            reader,
            Arc::clone(&self.shadow_screen),
            self.event_sender(),
            config.detect_notifications,
            Arc::clone(&self.kitty_enabled),
        ));

        self.set_master_pty(pair.master);

        // Start command processor task
        self.spawn_command_processor();

        // Write context and init commands
        if !config.context.is_empty() {
            let _ = self.write_input_str(&format!("{}\n", config.context));
        }

        if !config.init_commands.is_empty() {
            log::info!("Sending {} init command(s)", config.init_commands.len());
            std::thread::sleep(std::time::Duration::from_millis(100));
            for cmd_str in &config.init_commands {
                log::debug!("Running init command: {cmd_str}");
                let _ = self.write_input_str(&format!("{cmd_str}\n"));
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }

        Ok(())
    }

    /// Check if notification detection is enabled for this session.
    #[must_use]
    pub fn has_notifications(&self) -> bool {
        self.detect_notifications
    }

    /// Get a clone of the shared state Arc for the command processor.
    ///
    /// Used internally by `spawn_command_processor()`.
    fn shared_state_clone(&self) -> Arc<Mutex<SharedPtyState>> {
        Arc::clone(&self.shared_state)
    }

    /// Get the event and command channel senders for this PTY, along with the port.
    ///
    /// Returns a tuple of (event_tx, command_tx, port) that can be used to create
    /// a `PtyHandle` for client access.
    #[must_use]
    pub fn get_channels(&self) -> (broadcast::Sender<PtyEvent>, mpsc::Sender<PtyCommand>, Option<u16>) {
        (self.event_tx.clone(), self.command_tx.clone(), self.port)
    }

    /// Get direct access handles for sync I/O operations.
    ///
    /// Returns clones of the internal Arc references for direct, synchronous
    /// PTY access without going through the async command channel. This enables
    /// immediate input/output with no tokio scheduling delay.
    ///
    /// # Returns
    ///
    /// Tuple of (shared_state, shadow_screen, event_tx, kitty_enabled) for direct access.
    #[must_use]
    pub fn get_direct_access(
        &self,
    ) -> (
        Arc<Mutex<SharedPtyState>>,
        Arc<Mutex<vt100::Parser>>,
        broadcast::Sender<PtyEvent>,
        Arc<AtomicBool>,
    ) {
        (
            Arc::clone(&self.shared_state),
            Arc::clone(&self.shadow_screen),
            self.event_tx.clone(),
            Arc::clone(&self.kitty_enabled),
        )
    }

    /// Take the command receiver from this PTY session.
    ///
    /// This should be called once during setup to obtain the receiver for
    /// processing commands in the event loop. Returns None if already taken.
    pub fn take_command_receiver(&mut self) -> Option<mpsc::Receiver<PtyCommand>> {
        self.command_rx.take()
    }

    /// Spawn the command processor task.
    ///
    /// This starts a background tokio task that processes commands from the
    /// `command_rx` channel. The task handles:
    /// - `PtyCommand::Input` - Writes data to the PTY
    ///
    /// The task runs until the command channel is closed (all senders dropped)
    /// or the PTY session is dropped.
    ///
    /// # Runtime Context
    ///
    /// If called outside a Tokio runtime context, this method logs a warning
    /// and returns without spawning. The caller should then use
    /// [`process_commands`] for synchronous command processing.
    ///
    /// # Panics
    ///
    /// Panics if called more than once (command receiver already taken).
    pub fn spawn_command_processor(&mut self) {
        // Check for Tokio runtime before taking the rx.
        // If no runtime available, leave rx in place for sync processing.
        let runtime_handle = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => {
                log::warn!(
                    "PTY command processor not spawned - no Tokio runtime context. \
                     Using synchronous command processing via process_commands()."
                );
                return;
            }
        };

        let rx = self
            .command_rx
            .take()
            .expect("Command receiver already taken - spawn_command_processor called twice?");

        let shared_state = self.shared_state_clone();

        let handle = runtime_handle.spawn(async move {
            run_command_processor(rx, shared_state).await;
        });

        self.command_processor_handle = Some(handle);
        log::debug!("PTY command processor spawned (async)");
    }

    /// Process pending commands from clients (synchronous version).
    ///
    /// Call this in the event loop to handle commands sent via `PtyHandle`.
    /// Returns the number of commands processed.
    ///
    /// NOTE: Prefer using `spawn_command_processor()` for async command
    /// processing. This method is provided for backwards compatibility.
    pub fn process_commands(&mut self) -> usize {
        // Collect commands first to avoid borrow conflict with handle_command
        let mut commands = Vec::new();

        if let Some(ref mut rx) = self.command_rx {
            // Drain up to 100 commands per tick
            // Magic value: balances responsiveness with not blocking too long
            for _ in 0..100 {
                match rx.try_recv() {
                    Ok(cmd) => commands.push(cmd),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        log::warn!("PTY command channel disconnected");
                        break;
                    }
                }
            }
        }

        // Now process collected commands
        let count = commands.len();
        for cmd in commands {
            self.handle_command(cmd);
        }
        count
    }

    /// Handle a single PTY command.
    fn handle_command(&self, cmd: PtyCommand) {
        match cmd {
            PtyCommand::Input(data) => {
                if let Err(e) = self.write_input(&data) {
                    log::error!("Failed to write PTY input: {}", e);
                }
            }
        }
    }

    // =========================================================================
    // Event Broadcasting
    // =========================================================================

    /// Subscribe to PTY events.
    ///
    /// Returns a broadcast receiver that will receive all future events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<PtyEvent> {
        self.event_tx.subscribe()
    }

    /// Broadcast an event to all subscribers.
    ///
    /// This is the primary method for emitting events. The reader thread
    /// uses this to broadcast output, and lifecycle methods use it for
    /// resize and ownership events.
    ///
    /// Returns the number of receivers that received the event.
    pub fn broadcast(&self, event: PtyEvent) -> usize {
        // Ignore send errors - they occur when there are no receivers,
        // which is valid (no clients connected yet)
        self.event_tx.send(event).unwrap_or(0)
    }

    /// Get a clone of the broadcast sender.
    ///
    /// Useful for passing to background tasks (like the reader thread)
    /// that need to emit events.
    #[must_use]
    pub fn event_sender(&self) -> broadcast::Sender<PtyEvent> {
        self.event_tx.clone()
    }

    // =========================================================================
    // PTY I/O
    // =========================================================================

    /// Check if a process has been spawned in this PTY session.
    #[must_use]
    pub fn is_spawned(&self) -> bool {
        self.shared_state
            .lock()
            .expect("shared_state lock poisoned")
            .master_pty
            .is_some()
    }

    /// Store the child process handle (called after spawn).
    pub fn set_child(&mut self, child: Box<dyn Child + Send>) {
        self.child = Some(child);
    }

    /// Kill the child process if running.
    ///
    /// This is automatically called on drop, but can be called manually
    /// for explicit cleanup.
    pub fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            log::info!("Killing PTY child process");
            if let Err(e) = child.kill() {
                log::warn!("Failed to kill PTY child: {e}");
            }
            // Wait for process to exit to prevent zombies
            let _ = child.wait();
        }
    }

    /// Resize the PTY to new dimensions.
    ///
    /// Resizes the shadow screen *before* the PTY so that when the inner
    /// application redraws for the new size, the reader thread's
    /// `parser.process()` already targets the correct dimensions.
    /// Resizing in the opposite order (PTY first) creates a race where
    /// redraw output is parsed against stale dimensions, corrupting
    /// cursor tracking — especially visible on Linux.
    pub fn resize(&self, rows: u16, cols: u16) {
        // 1. Shadow screen first — ready for new-size output.
        // Clamp rows to MIN_PARSER_ROWS to avoid vt100 0.16.2 col_wrap
        // panic: a 1-row grid causes `prev_pos.row -= scrolled` to
        // underflow in Grid::col_wrap (grid.rs:683).
        let old_dims = {
            let mut parser = self
                .shadow_screen
                .lock()
                .expect("shadow_screen lock poisoned");
            let screen = parser.screen_mut();
            let old = screen.size();
            screen.set_size(rows.max(MIN_PARSER_ROWS), cols);
            old
        };

        // 2. PTY resize — triggers application redraw.
        {
            let mut state = self
                .shared_state
                .lock()
                .expect("shared_state lock poisoned");

            state.dimensions = (rows, cols);

            if let Some(master_pty) = &state.master_pty {
                if let Err(e) = master_pty.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                }) {
                    log::warn!("Failed to resize PTY: {e}");
                    // Revert shadow screen to match the actual PTY dimensions.
                    let mut parser = self
                        .shadow_screen
                        .lock()
                        .expect("shadow_screen lock poisoned");
                    parser.screen_mut().set_size(old_dims.0.max(MIN_PARSER_ROWS), old_dims.1);
                    state.dimensions = (old_dims.0, old_dims.1);
                    return;
                }
            }
        }

        // Broadcast resize event
        self.broadcast(PtyEvent::resized(rows, cols));
    }

    /// Write input bytes to the PTY.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails or no writer is available.
    pub fn write_input(&self, input: &[u8]) -> Result<()> {
        let mut state = self
            .shared_state
            .lock()
            .expect("shared_state lock poisoned");
        if let Some(writer) = &mut state.writer {
            writer.write_all(input)?;
            writer.flush()?;
        }
        Ok(())
    }

    /// Write a string to the PTY.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_input_str(&self, input: &str) -> Result<()> {
        self.write_input(input.as_bytes())
    }

    // =========================================================================
    // Shadow Terminal Snapshot
    // =========================================================================

    /// Get a clean ANSI snapshot of the current terminal state.
    ///
    /// Locks the shadow screen and delegates to [`snapshot_with_scrollback`].
    /// Appends the kitty keyboard push sequence when the inner PTY has
    /// activated kitty mode, so connecting terminals enter kitty mode.
    #[must_use]
    pub fn get_snapshot(&self) -> Vec<u8> {
        let mut parser = self
            .shadow_screen
            .lock()
            .expect("shadow_screen lock poisoned");
        let mut snapshot = snapshot_with_scrollback(parser.screen_mut());

        // Restore kitty keyboard protocol state in the snapshot.
        // The vt100 shadow screen doesn't track kitty mode, so we append
        // the push sequence from our own tracking of the raw PTY output.
        if self.kitty_enabled.load(Ordering::Relaxed) {
            // CSI > 1 u = push kitty keyboard with DISAMBIGUATE_ESCAPE_CODES flag.
            snapshot.extend(b"\x1b[>1u");
        }

        snapshot
    }

    /// Whether the inner PTY has kitty keyboard protocol active.
    #[must_use]
    pub fn kitty_enabled(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.kitty_enabled)
    }

}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Abort the command processor task if running
        if let Some(handle) = self.command_processor_handle.take() {
            handle.abort();
        }
        self.kill_child();
    }
}

// =============================================================================
// Command Processor Task
// =============================================================================

/// Run the command processor loop.
///
/// This function processes commands from `PtyHandle` clients in a background
/// task. It runs until the command channel is closed (all senders dropped).
///
/// The processor is self-contained within the PTY module - Hub does not need
/// to poll for commands.
async fn run_command_processor(
    mut rx: mpsc::Receiver<PtyCommand>,
    shared_state: Arc<Mutex<SharedPtyState>>,
) {
    log::debug!("Command processor started");

    while let Some(cmd) = rx.recv().await {
        process_single_command(cmd, &shared_state);
    }

    log::debug!("Command processor exiting - channel closed");
}

/// Process a single PTY command.
///
/// Handles Input commands using the shared state.
/// This is called from the async command processor task.
fn process_single_command(
    cmd: PtyCommand,
    shared_state: &Arc<Mutex<SharedPtyState>>,
) {
    match cmd {
        PtyCommand::Input(data) => {
            let mut state = shared_state.lock().expect("shared_state lock poisoned");
            if let Some(writer) = &mut state.writer {
                if let Err(e) = writer.write_all(&data) {
                    log::error!("Failed to write PTY input: {}", e);
                    return;
                }
                if let Err(e) = writer.flush() {
                    log::error!("Failed to flush PTY writer: {}", e);
                }
            }
        }
    }
}

/// Perform PTY resize operation.
///
/// Resizes shadow screen before PTY to avoid the race where the reader
/// thread processes new-size output against stale shadow screen dimensions.
/// See `PtySession::resize` for detailed rationale.
///
/// Exposed as `pub(crate)` for direct sync resize from `PtyHandle`.
pub(crate) fn do_resize(
    rows: u16,
    cols: u16,
    shared_state: &Arc<Mutex<SharedPtyState>>,
    shadow_screen: &Arc<Mutex<vt100::Parser>>,
    event_tx: &broadcast::Sender<PtyEvent>,
) {
    // 1. Shadow screen first — ready for new-size output.
    let old_dims = {
        let mut parser = shadow_screen
            .lock()
            .expect("shadow_screen lock poisoned");
        let screen = parser.screen_mut();
        let old = screen.size();
        screen.set_size(rows.max(MIN_PARSER_ROWS), cols);
        old
    };

    // 2. PTY resize — triggers application redraw.
    {
        let mut state = shared_state.lock().expect("shared_state lock poisoned");

        state.dimensions = (rows, cols);

        if let Some(master_pty) = &state.master_pty {
            if let Err(e) = master_pty.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            }) {
                log::warn!("Failed to resize PTY: {e}");
                // Revert shadow screen to match the actual PTY dimensions.
                let mut parser = shadow_screen
                    .lock()
                    .expect("shadow_screen lock poisoned");
                parser.screen_mut().set_size(old_dims.0.max(MIN_PARSER_ROWS), old_dims.1);
                state.dimensions = (old_dims.0, old_dims.1);
                return;
            }
        }
    }

    // Broadcast resize event
    let _ = event_tx.send(PtyEvent::resized(rows, cols));
}

/// Build a clean ANSI snapshot from a vt100 screen.
///
/// Returns the visible screen contents as formatted ANSI escape sequences
/// with the cursor positioned correctly. Used for browser connect/reconnect
/// instead of replaying raw byte history.
///
/// The snapshot includes:
/// - Terminal state reset (clear screen, reset attributes)
/// - Visible screen contents with correct colors/attributes
/// - Cursor positioned where the PTY left it
#[must_use]
pub fn snapshot_from_screen(screen: &vt100::Screen) -> Vec<u8> {
    let mut output = Vec::new();
    // Reset terminal state and clear screen
    output.extend(b"\x1b[H\x1b[2J\x1b[0m");
    // Emit visible screen with ANSI attributes (colors, bold, etc.)
    output.extend(screen.contents_formatted());
    // Position cursor where the PTY left it
    let (row, col) = screen.cursor_position();
    output.extend(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    output
}

/// Produces a snapshot that includes scrollback history plus the visible screen.
///
/// Scrollback rows are emitted as ANSI-formatted text (oldest first) so they
/// flow through ghostty's VT core and naturally land in its scrollback buffer.
/// After all history is sent, the screen is cleared and the visible state is
/// rendered with precise cursor positioning.
///
/// Falls back to [`snapshot_from_screen`] when no scrollback exists.
#[must_use]
pub fn snapshot_with_scrollback(screen: &mut vt100::Screen) -> Vec<u8> {
    let saved_offset = screen.scrollback();

    // Discover how many scrollback lines the shadow terminal has accumulated.
    screen.set_scrollback(usize::MAX);
    let total_scrollback = screen.scrollback();
    screen.set_scrollback(saved_offset);

    if total_scrollback == 0 {
        return snapshot_from_screen(screen);
    }

    let rows = screen.size().0 as usize;
    let cols = screen.size().1;
    // Pre-allocate: ~120 bytes per scrollback line is a reasonable estimate.
    let mut output = Vec::with_capacity(total_scrollback * 120);

    // Phase 1: Emit scrollback history (oldest → newest).
    //
    // At scrollback offset N, `visible_rows()` shows the last N rows of the
    // scrollback buffer followed by screen rows. We step through in
    // screen-height chunks from the top of history, emitting only the
    // scrollback portion of each view.
    output.extend(b"\x1b[0m");

    let mut remaining = total_scrollback;
    while remaining > 0 {
        screen.set_scrollback(remaining);
        let scrollback_rows_in_view = remaining.min(rows);

        for (i, row_bytes) in screen.rows_formatted(0, cols).enumerate() {
            if i >= scrollback_rows_in_view {
                break;
            }
            output.extend(&row_bytes);
            output.extend(b"\r\n");
        }

        remaining = remaining.saturating_sub(rows);
    }

    screen.set_scrollback(saved_offset);

    // Phase 2: Visible screen with full ANSI formatting and cursor position.
    //
    // After Phase 1, the last `rows`-worth of scrollback lines are sitting on
    // the receiving parser's visible screen. \x1b[2J would erase them without
    // pushing to scrollback, losing those lines. Instead, scroll them off the
    // top so they land in the scrollback buffer, then draw the live screen.
    output.extend(format!("\x1b[{};1H", rows).as_bytes());
    for _ in 0..rows {
        output.push(b'\n');
    }
    output.extend(b"\x1b[H\x1b[0m");
    output.extend(screen.contents_formatted());
    let (row, col) = screen.cursor_position();
    output.extend(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pty_session_creation() {
        let session = PtySession::new(24, 80);

        assert!(!session.is_spawned());
        assert_eq!(session.dimensions(), (24, 80));
        assert!(session.port().is_none());
    }

    #[test]
    fn test_pty_session_port() {
        let mut session = PtySession::new(24, 80);

        // Initially no port
        assert!(session.port().is_none());

        // Set port
        session.set_port(8080);
        assert_eq!(session.port(), Some(8080));

        // get_channels returns the port
        let (_, _, port) = session.get_channels();
        assert_eq!(port, Some(8080));
    }

    #[test]
    fn test_pty_session_subscribe() {
        let session = PtySession::new(24, 80);

        let _rx1 = session.subscribe();
        let _rx2 = session.subscribe();

        // Multiple subscriptions should work without error
    }

    #[test]
    fn test_pty_session_snapshot() {
        let session = PtySession::new(24, 80);

        // Feed some output to the shadow screen
        session
            .shadow_screen
            .lock()
            .unwrap()
            .process(b"hello world");

        let snapshot = session.get_snapshot();
        // Snapshot should contain the text and ANSI reset/cursor sequences
        let snapshot_str = String::from_utf8_lossy(&snapshot);
        assert!(snapshot_str.contains("hello world"));
        // Should start with reset sequence
        assert!(snapshot_str.starts_with("\x1b[H\x1b[2J\x1b[0m"));
    }

    #[test]
    fn test_pty_session_snapshot_with_colors() {
        let session = PtySession::new(24, 80);

        // Feed colored output (green text)
        session
            .shadow_screen
            .lock()
            .unwrap()
            .process(b"\x1b[32mgreen text\x1b[0m");

        let snapshot = session.get_snapshot();
        let snapshot_str = String::from_utf8_lossy(&snapshot);
        assert!(snapshot_str.contains("green text"));
    }

    // =========================================================================
    // Kitty Keyboard Protocol in Snapshots
    // =========================================================================

    #[test]
    fn test_snapshot_includes_kitty_push_when_enabled() {
        let session = PtySession::new(24, 80);

        session
            .shadow_screen
            .lock()
            .unwrap()
            .process(b"hello");

        // Simulate reader thread detecting kitty push
        session.kitty_enabled.store(true, Ordering::Relaxed);

        let snapshot = session.get_snapshot();
        let snapshot_str = String::from_utf8_lossy(&snapshot);

        assert!(snapshot_str.contains("hello"), "snapshot should contain screen content");
        assert!(
            snapshot.windows(5).any(|w| w == b"\x1b[>1u"),
            "snapshot should end with kitty push sequence (CSI > 1 u)"
        );
    }

    #[test]
    fn test_snapshot_excludes_kitty_when_disabled() {
        let session = PtySession::new(24, 80);

        session
            .shadow_screen
            .lock()
            .unwrap()
            .process(b"hello");

        // kitty_enabled defaults to false
        let snapshot = session.get_snapshot();

        assert!(
            !snapshot.windows(5).any(|w| w == b"\x1b[>1u"),
            "snapshot should NOT contain kitty push when kitty is disabled"
        );
    }

    #[test]
    fn test_snapshot_excludes_kitty_after_pop() {
        let session = PtySession::new(24, 80);

        // Push then pop
        session.kitty_enabled.store(true, Ordering::Relaxed);
        session.kitty_enabled.store(false, Ordering::Relaxed);

        let snapshot = session.get_snapshot();

        assert!(
            !snapshot.windows(5).any(|w| w == b"\x1b[>1u"),
            "snapshot should NOT contain kitty push after pop"
        );
    }

    #[test]
    fn test_pty_session_broadcast() {
        let session = PtySession::new(24, 80);

        let mut rx = session.subscribe();

        let count = session.broadcast(PtyEvent::output(b"hello".to_vec()));
        assert_eq!(count, 1);

        let event = rx.try_recv().unwrap();
        match event {
            PtyEvent::Output(data) => assert_eq!(data, b"hello"),
            _ => panic!("Expected Output event"),
        }
    }

    #[test]
    fn test_pty_session_broadcast_no_receivers() {
        let session = PtySession::new(24, 80);

        let count = session.broadcast(PtyEvent::output(b"hello".to_vec()));
        assert_eq!(count, 0);
    }

    #[test]
    fn test_pty_session_event_sender() {
        let session = PtySession::new(24, 80);

        let tx = session.event_sender();
        let mut rx = session.subscribe();

        let _ = tx.send(PtyEvent::resized(30, 100));

        let event = rx.try_recv().unwrap();
        match event {
            PtyEvent::Resized { rows, cols } => {
                assert_eq!(rows, 30);
                assert_eq!(cols, 100);
            }
            _ => panic!("Expected Resized event"),
        }
    }

    #[test]
    fn test_pty_session_debug() {
        let session = PtySession::new(24, 80);
        let debug = format!("{:?}", session);

        assert!(debug.contains("PtySession"));
        assert!(debug.contains("has_master_pty"));
    }

    #[test]
    fn test_pty_session_resize() {
        let session = PtySession::new(24, 80);
        let mut rx = session.subscribe();

        session.resize(30, 120);

        assert_eq!(session.dimensions(), (30, 120));

        let event = rx.try_recv().unwrap();
        match event {
            PtyEvent::Resized { rows, cols } => {
                assert_eq!(rows, 30);
                assert_eq!(cols, 120);
            }
            _ => panic!("Expected Resized event"),
        }
    }

    // =========================================================================
    // Hot Path Tests - PTY Output Broadcasting
    // =========================================================================

    #[test]
    fn test_hot_path_broadcast_to_multiple_subscribers() {
        let session = PtySession::new(24, 80);

        let mut rx1 = session.subscribe();
        let mut rx2 = session.subscribe();
        let mut rx3 = session.subscribe();

        let count = session.broadcast(PtyEvent::output(b"hello world".to_vec()));
        assert_eq!(count, 3);

        for (i, rx) in [&mut rx1, &mut rx2, &mut rx3].iter_mut().enumerate() {
            let event = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("Receiver {} should have event", i));
            match event {
                PtyEvent::Output(data) => {
                    assert_eq!(data, b"hello world", "Receiver {} got wrong data", i);
                }
                _ => panic!("Receiver {} expected Output event", i),
            }
        }
    }

    #[test]
    fn test_hot_path_output_event_ordering() {
        let session = PtySession::new(24, 80);
        let mut rx = session.subscribe();

        session.broadcast(PtyEvent::output(b"first".to_vec()));
        session.broadcast(PtyEvent::output(b"second".to_vec()));
        session.broadcast(PtyEvent::output(b"third".to_vec()));

        let expected = [b"first".as_slice(), b"second".as_slice(), b"third".as_slice()];
        for (i, expected_data) in expected.iter().enumerate() {
            let event = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("Event {} should exist", i));
            match event {
                PtyEvent::Output(data) => {
                    assert_eq!(&data[..], *expected_data, "Event {} has wrong data", i);
                }
                _ => panic!("Event {} should be Output", i),
            }
        }
    }

    #[test]
    fn test_hot_path_dropped_subscriber_doesnt_block_others() {
        let session = PtySession::new(24, 80);

        let mut rx1 = session.subscribe();
        let rx2 = session.subscribe();
        let mut rx3 = session.subscribe();

        drop(rx2);

        let count = session.broadcast(PtyEvent::output(b"test".to_vec()));
        assert_eq!(count, 2);

        assert!(rx1.try_recv().is_ok());
        assert!(rx3.try_recv().is_ok());
    }

    #[test]
    fn test_hot_path_event_sender_for_reader_thread() {
        let session = PtySession::new(24, 80);
        let tx = session.event_sender();
        let mut rx = session.subscribe();

        let output_data = b"PTY output from reader thread";
        let _ = tx.send(PtyEvent::output(output_data.to_vec()));

        let event = rx.try_recv().expect("Should receive event from cloned sender");
        match event {
            PtyEvent::Output(data) => assert_eq!(data, output_data),
            _ => panic!("Expected Output event"),
        }
    }

    #[test]
    fn test_hot_path_high_volume_broadcast() {
        let session = PtySession::new(24, 80);
        let mut rx = session.subscribe();

        for i in 0..100 {
            let data = format!("chunk-{}", i);
            session.broadcast(PtyEvent::output(data.into_bytes()));
        }

        for i in 0..100 {
            let expected = format!("chunk-{}", i);
            let event = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("Event {} should exist", i));
            match event {
                PtyEvent::Output(data) => {
                    assert_eq!(String::from_utf8_lossy(&data), expected, "Event {} wrong", i);
                }
                _ => panic!("Event {} should be Output", i),
            }
        }
    }

    // =========================================================================
    // Command Processor Tests
    // =========================================================================

    #[tokio::test]
    async fn test_command_processor_input_command() {
        let shared_state = Arc::new(Mutex::new(SharedPtyState {
            master_pty: None,
            writer: None,
            dimensions: (24, 80),
        }));

        process_single_command(
            PtyCommand::Input(b"test input".to_vec()),
            &shared_state,
        );
    }

    #[test]
    fn test_spawn_command_processor_takes_receiver() {
        let mut session = PtySession::new(24, 80);

        assert!(session.command_rx.is_some());
        assert!(session.command_processor_handle.is_none());

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            session.spawn_command_processor();
        });

        assert!(session.command_rx.is_none());
        assert!(session.command_processor_handle.is_some());
    }

    #[test]
    #[should_panic(expected = "Command receiver already taken")]
    fn test_spawn_command_processor_panics_if_called_twice() {
        let mut session = PtySession::new(24, 80);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            session.spawn_command_processor();
            session.spawn_command_processor(); // Should panic
        });
    }

    // =========================================================================
    // PtySession::spawn Tests
    // =========================================================================

    #[tokio::test]
    async fn test_spawn_basic() {
        use std::collections::HashMap;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let mut session = PtySession::new(24, 80);

        let config = PtySpawnConfig {
            worktree_path: temp_dir.path().to_path_buf(),
            command: "echo hello".to_string(),
            env: HashMap::new(),
            init_commands: vec![],
            detect_notifications: false,
            port: None,
            context: String::new(),
        };

        let result = session.spawn(config);
        assert!(result.is_ok());
        assert!(session.is_spawned());
        assert!(!session.has_notifications());
    }

    #[tokio::test]
    async fn test_spawn_with_notifications() {
        use std::collections::HashMap;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let mut session = PtySession::new(24, 80);

        let config = PtySpawnConfig {
            worktree_path: temp_dir.path().to_path_buf(),
            command: "echo hello".to_string(),
            env: HashMap::new(),
            init_commands: vec![],
            detect_notifications: true,
            port: None,
            context: String::new(),
        };

        let result = session.spawn(config);
        assert!(result.is_ok());
        assert!(session.is_spawned());
        assert!(session.has_notifications());
    }

    #[tokio::test]
    async fn test_spawn_with_port() {
        use std::collections::HashMap;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let mut session = PtySession::new(24, 80);

        let config = PtySpawnConfig {
            worktree_path: temp_dir.path().to_path_buf(),
            command: "echo hello".to_string(),
            env: HashMap::new(),
            init_commands: vec![],
            detect_notifications: false,
            port: Some(8080),
            context: String::new(),
        };

        let result = session.spawn(config);
        assert!(result.is_ok());
        assert_eq!(session.port(), Some(8080));
    }

    #[test]
    fn test_spawn_command_processor_outside_tokio_runtime() {
        let mut session = PtySession::new(24, 80);
        session.spawn_command_processor();
        assert!(session.command_rx.is_some());
        let processed = session.process_commands();
        assert_eq!(processed, 0);
    }
}
