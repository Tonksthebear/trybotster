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
//!  ├── scrollback_buffer: Arc<Mutex<VecDeque<u8>>> (raw byte history)
//!  ├── event_tx: broadcast::Sender<PtyEvent> (output broadcast)
//!  ├── notification_tx: Sender<AgentNotification> (send notifications)
//!  └── notification_rx: Receiver<AgentNotification> (receive notifications)
//! ```
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
//! subscribe, scrollback).
//!
//! # Thread Safety
//!
//! The scrollback buffer is wrapped in `Arc<Mutex<>>` to allow concurrent
//! reads from the PTY reader thread and writes from the main thread.

// Rust guideline compliant 2026-02

mod commands;
pub mod events;

pub use commands::PtyCommand;
pub use events::PtyEvent;

pub use super::spawn::PtySpawnConfig;

use anyhow::{Context, Result};
use portable_pty::{Child, MasterPty, PtySize};
use std::{
    collections::VecDeque,
    io::Write,
    sync::{mpsc::Sender, Arc, Mutex},
    thread,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::agent::spawn;

use super::notification::AgentNotification;

/// Default channel capacity for PTY command channels.
const PTY_COMMAND_CHANNEL_CAPACITY: usize = 64;

/// Maximum bytes to keep in scrollback buffer.
///
/// 4MB balances memory usage with sufficient history for debugging.
/// Based on typical agent session output rates, this provides
/// several hours of scrollback.
pub const MAX_SCROLLBACK_BYTES: usize = 4 * 1024 * 1024;

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
pub(crate) struct SharedPtyState {
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
/// - A raw byte scrollback buffer for history replay
/// - A broadcast channel for event distribution to clients
/// - An optional port for HTTP forwarding (used by server PTY for dev server preview)
///
/// # Event Broadcasting
///
/// Output and lifecycle events are broadcast to all subscribers via
/// [`PtyEvent`]. Clients subscribe via [`subscribe()`](Self::subscribe).
///
/// # Terminal Emulation
///
/// PtySession does NOT own a vt100 parser. It emits raw bytes via broadcast.
/// Consumers (TuiRunner, browser via Lua) own their own parsers.
/// This keeps PtySession as pure I/O.
///
/// # Client Tracking
///
/// Client connection tracking and size ownership are managed by Lua.
/// Rust provides only the I/O primitives (resize, write, subscribe, scrollback).
///
/// # Command Processing
///
/// After spawning, call [`spawn_command_processor()`](Self::spawn_command_processor)
/// to start the background task that processes commands from `PtyHandle` clients.
/// The processor handles Input, Connect, and Disconnect commands.
///
/// # Thread Safety
///
/// The scrollback buffer and shared state are wrapped in `Arc<Mutex<>>` to allow
/// concurrent access from the PTY reader thread, command processor task, and main
/// event loop.
///
/// # Port Field
///
/// The `port` field stores the HTTP forwarding port for preview functionality.
/// This is primarily used by server PTYs (pty_index=1) running dev servers.
/// When set, the port value is also passed via `BOTSTER_TUNNEL_PORT` env var
/// to the PTY process at spawn time.
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

    /// Raw byte scrollback buffer for history replay.
    ///
    /// Stores raw PTY output so xterm.js can interpret escape sequences correctly.
    /// Clients can request a snapshot for session replay.
    pub scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,

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

    /// Channel for sending detected notifications.
    pub notification_tx: Option<Sender<AgentNotification>>,

    /// Channel for receiving detected notifications.
    ///
    /// Populated by [`spawn()`](Self::spawn) when
    /// [`PtySpawnConfig::detect_notifications`] is true.
    /// Drained via [`poll_notifications()`](Self::poll_notifications).
    ///
    /// Note: `mpsc::Receiver` is not `Clone`, which is intentional -- only
    /// one consumer should drain notifications for a given PTY session.
    notification_rx: Option<std::sync::mpsc::Receiver<AgentNotification>>,

    /// Allocated port for HTTP forwarding.
    ///
    /// Used by server PTYs (pty_index=1) to expose the dev server port for
    /// preview proxying. The port is set via [`set_port()`](Self::set_port)
    /// and queried via [`port()`](Self::port).
    ///
    /// When spawning the PTY process, the caller should also pass this port
    /// via the `BOTSTER_TUNNEL_PORT` environment variable.
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
            .field("has_notification_tx", &self.notification_tx.is_some())
            .field("has_notification_rx", &self.notification_rx.is_some())
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
            scrollback_buffer: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_SCROLLBACK_BYTES))),
            event_tx,
            command_tx,
            command_rx: Some(command_rx),
            notification_tx: None,
            notification_rx: None,
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
    /// Called after spawning to record the allocated port. This is primarily
    /// used for server PTYs (pty_index=1) running dev servers.
    ///
    /// Note: The port should also be passed via `BOTSTER_TUNNEL_PORT` env var
    /// when spawning the PTY process.
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
    ///     init_commands: vec!["source .botster_init".to_string()],
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

        // Set up notification channel if enabled
        let notification_tx = if config.detect_notifications {
            let (tx, rx) = std::sync::mpsc::channel();
            self.notification_rx = Some(rx);
            Some(tx)
        } else {
            None
        };

        // Store the notification_tx on self for external access if needed
        self.notification_tx = notification_tx.clone();

        // Configure PTY with spawned resources
        self.set_child(child);
        self.set_writer(pair.master.take_writer()?);

        // Start unified reader thread
        let reader = pair.master.try_clone_reader()?;
        self.reader_thread = Some(spawn::spawn_reader_thread(
            reader,
            Arc::clone(&self.scrollback_buffer),
            self.event_sender(),
            notification_tx,
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

    /// Poll for pending notifications (non-blocking).
    ///
    /// Drains all available notifications from the internal receiver channel.
    /// Returns an empty `Vec` if notifications are not enabled for this session
    /// (i.e., `detect_notifications` was `false` in the spawn config).
    ///
    /// This is the PtySession-level equivalent of `Agent::poll_notifications()`.
    /// As the Agent struct is dissolved, callers should use this method directly
    /// on the PtySession instead.
    #[must_use]
    pub fn poll_notifications(&self) -> Vec<AgentNotification> {
        let mut notifications = Vec::new();
        if let Some(ref rx) = self.notification_rx {
            while let Ok(notif) = rx.try_recv() {
                notifications.push(notif);
            }
        }
        notifications
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
    /// Tuple of (shared_state, scrollback_buffer, event_tx) for direct access.
    #[must_use]
    pub fn get_direct_access(
        &self,
    ) -> (
        Arc<Mutex<SharedPtyState>>,
        Arc<Mutex<VecDeque<u8>>>,
        broadcast::Sender<PtyEvent>,
    ) {
        (
            Arc::clone(&self.shared_state),
            Arc::clone(&self.scrollback_buffer),
            self.event_tx.clone(),
        )
    }

    /// Take the command receiver from this PTY session.
    ///
    /// This should be called once during setup to obtain the receiver for
    /// processing commands in the event loop. Returns None if already taken.
    pub fn take_command_receiver(&mut self) -> Option<mpsc::Receiver<PtyCommand>> {
        self.command_rx.take()
    }

    /// Take the notification receiver from this PTY session.
    ///
    /// This transfers ownership of the notification channel receiver to the
    /// caller. Used by `PtySessionHandle` to own the notification receiver
    /// directly, avoiding the need to go through the `PtySession` for polling.
    ///
    /// Returns `None` if notifications are not enabled or already taken.
    pub fn take_notification_rx(
        &mut self,
    ) -> Option<std::sync::mpsc::Receiver<AgentNotification>> {
        self.notification_rx.take()
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
    /// Updates the underlying PTY size and broadcasts a resize event.
    /// Clients should update their own parsers when they receive the resize event.
    pub fn resize(&self, rows: u16, cols: u16) {
        {
            let mut state = self
                .shared_state
                .lock()
                .expect("shared_state lock poisoned");

            // Track dimensions locally
            state.dimensions = (rows, cols);

            // Resize the PTY
            if let Some(master_pty) = &state.master_pty {
                if let Err(e) = master_pty.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                }) {
                    log::warn!("Failed to resize PTY: {e}");
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
    // Scrollback
    // =========================================================================

    /// Add raw bytes to the scrollback buffer.
    ///
    /// Bytes exceeding `MAX_SCROLLBACK_BYTES` are dropped from the front.
    pub fn add_to_scrollback(&self, data: &[u8]) {
        let mut buffer = self
            .scrollback_buffer
            .lock()
            .expect("scrollback_buffer lock poisoned");

        // Add new bytes
        buffer.extend(data.iter().copied());

        // Trim from front if over limit
        while buffer.len() > MAX_SCROLLBACK_BYTES {
            buffer.pop_front();
        }
    }

    /// Get a snapshot of the scrollback buffer as raw bytes.
    ///
    /// Returns the complete scrollback history for session replay.
    #[must_use]
    pub fn get_scrollback_snapshot(&self) -> Vec<u8> {
        self.scrollback_buffer
            .lock()
            .expect("scrollback_buffer lock poisoned")
            .iter()
            .copied()
            .collect()
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
/// Updates dimensions, resizes the PTY, and broadcasts the resize event.
///
/// Exposed as `pub(crate)` for direct sync resize from `PtyHandle`.
pub(crate) fn do_resize(
    rows: u16,
    cols: u16,
    shared_state: &Arc<Mutex<SharedPtyState>>,
    event_tx: &broadcast::Sender<PtyEvent>,
) {
    {
        let mut state = shared_state.lock().expect("shared_state lock poisoned");

        // Track dimensions
        state.dimensions = (rows, cols);

        // Resize the PTY
        if let Some(master_pty) = &state.master_pty {
            if let Err(e) = master_pty.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            }) {
                log::warn!("Failed to resize PTY: {e}");
                return;
            }
        }
    }

    // Broadcast resize event
    let _ = event_tx.send(PtyEvent::resized(rows, cols));
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
    fn test_pty_session_scrollback() {
        let session = PtySession::new(24, 80);

        session.add_to_scrollback(b"test line 1\n");
        session.add_to_scrollback(b"test line 2\n");

        let snapshot = session.get_scrollback_snapshot();
        assert_eq!(snapshot, b"test line 1\ntest line 2\n");
    }

    #[test]
    fn test_pty_session_scrollback_limit() {
        let session = PtySession::new(24, 80);

        let chunk = vec![b'x'; 1024];
        let num_chunks = MAX_SCROLLBACK_BYTES / 1024 + 100;
        for _ in 0..num_chunks {
            session.add_to_scrollback(&chunk);
        }

        let snapshot = session.get_scrollback_snapshot();
        assert!(snapshot.len() <= MAX_SCROLLBACK_BYTES);
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
    // PtySession::poll_notifications Tests
    // =========================================================================

    #[test]
    fn test_poll_notifications_empty_when_no_rx() {
        let session = PtySession::new(24, 80);
        assert!(session.notification_rx.is_none());
        assert!(session.poll_notifications().is_empty());
    }

    #[test]
    fn test_poll_notifications_drains_channel() {
        let mut session = PtySession::new(24, 80);
        let (tx, rx) = std::sync::mpsc::channel();
        session.notification_rx = Some(rx);

        tx.send(AgentNotification::Osc9(Some("first".to_string())))
            .unwrap();
        tx.send(AgentNotification::Osc9(Some("second".to_string())))
            .unwrap();

        let notifications = session.poll_notifications();
        assert_eq!(notifications.len(), 2);
        assert!(session.poll_notifications().is_empty());
    }

    #[test]
    fn test_poll_notifications_handles_dropped_sender() {
        let mut session = PtySession::new(24, 80);
        let (tx, rx) = std::sync::mpsc::channel::<AgentNotification>();
        session.notification_rx = Some(rx);
        drop(tx);
        assert!(session.poll_notifications().is_empty());
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
        assert!(session.notification_rx.is_none());
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
        assert!(session.notification_rx.is_some());
        assert!(session.notification_tx.is_some());
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
    fn test_notification_rx_not_clone() {
        let session = PtySession::new(24, 80);
        let _: &Option<std::sync::mpsc::Receiver<AgentNotification>> = &session.notification_rx;
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
