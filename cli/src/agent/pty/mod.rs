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
//!  ├── connected_clients: Vec<ConnectedClient> (client tracking)
//!  └── notification_tx: Sender<AgentNotification> (agent notifications)
//! ```
//!
//! # Event Broadcasting
//!
//! PTY sessions emit [`PtyEvent`]s to all subscribers via a broadcast channel:
//! - [`PtyEvent::Output`] - Raw terminal output bytes
//! - [`PtyEvent::Resized`] - PTY dimensions changed
//! - [`PtyEvent::ProcessExited`] - Process terminated
//! - [`PtyEvent::OwnerChanged`] - Size ownership transferred
//!
//! # Client Connection
//!
//! Clients connect with their terminal dimensions. The newest client becomes
//! the "size owner" whose dimensions are applied to the PTY:
//!
//! ```ignore
//! // Client connects and receives subscription + scrollback atomically
//! let (rx, scrollback) = pty.connect(ClientId::Tui, (80, 24));
//!
//! // Replay scrollback to reconstruct terminal state
//! parser.process(&scrollback);
//!
//! // Receive events
//! match rx.recv().await {
//!     Ok(PtyEvent::Output(data)) => handle_output(&data),
//!     Ok(PtyEvent::Resized { rows, cols }) => resize_display(rows, cols),
//!     // ...
//! }
//!
//! // Client disconnects
//! pty.disconnect(ClientId::Tui);
//! ```
//!
//! # Thread Safety
//!
//! The scrollback buffer is wrapped in `Arc<Mutex<>>` to allow concurrent
//! reads from the PTY reader thread and writes from the main thread.

// Rust guideline compliant 2026-01

pub mod cli;
mod commands;
pub mod connected_client;
pub mod events;
pub mod server;

pub use commands::PtyCommand;

pub use cli::{spawn_cli_pty, CliSpawnResult};
pub use connected_client::ConnectedClient;
pub use events::PtyEvent;
pub use server::spawn_server_pty;

use anyhow::Result;
use portable_pty::{Child, MasterPty, PtySize};
use std::{
    collections::VecDeque,
    io::Write,
    sync::{mpsc::Sender, Arc, Mutex},
    thread,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::client::ClientId;

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

    /// Connected clients with their terminal dimensions.
    pub(crate) connected_clients: Vec<ConnectedClient>,
}

impl std::fmt::Debug for SharedPtyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedPtyState")
            .field("has_master_pty", &self.master_pty.is_some())
            .field("has_writer", &self.writer.is_some())
            .field("dimensions", &self.dimensions)
            .field("connected_clients", &self.connected_clients.len())
            .finish()
    }
}

/// Encapsulates all state for a single PTY session.
///
/// Each PTY session manages:
/// - A pseudo-terminal for process I/O
/// - A raw byte scrollback buffer for history replay
/// - A broadcast channel for event distribution to clients
/// - A list of connected clients with their dimensions
/// - An optional port for HTTP forwarding (used by server PTY for dev server preview)
///
/// # Event Broadcasting
///
/// Output and lifecycle events are broadcast to all connected clients via
/// [`PtyEvent`]. Clients subscribe via [`subscribe()`](Self::subscribe) or
/// [`connect()`](Self::connect).
///
/// # Terminal Emulation
///
/// PtySession does NOT own a vt100 parser. It emits raw bytes via broadcast.
/// Clients (TuiClient, TuiRunner) own their own parsers and feed bytes in
/// their `on_output()` handlers. This keeps PtySession as pure I/O.
///
/// # Size Ownership
///
/// The newest connected client is the "size owner" - their terminal dimensions
/// are applied to the PTY. When they disconnect, ownership passes to the next
/// most recent client.
///
/// # Command Processing
///
/// After spawning, call [`spawn_command_processor()`](Self::spawn_command_processor)
/// to start the background task that processes commands from `PtyHandle` clients.
/// The processor handles Input, Resize, Connect, and Disconnect commands.
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
    /// Contains: master_pty, writer, dimensions, connected_clients.
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
    /// Clients send commands (input, resize, connect, disconnect) through this
    /// channel. The receiver is consumed by the command processor task.
    command_tx: mpsc::Sender<PtyCommand>,

    /// Command receiver for PTY operations.
    ///
    /// Taken by [`spawn_command_processor()`](Self::spawn_command_processor)
    /// to be processed in a background task.
    command_rx: Option<mpsc::Receiver<PtyCommand>>,

    /// Channel for sending detected notifications.
    pub notification_tx: Option<Sender<AgentNotification>>,

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
            .field("connected_clients", &state.connected_clients.len())
            .field("has_notification_tx", &self.notification_tx.is_some())
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
            connected_clients: Vec::new(),
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

    /// Spawn the command processor task.
    ///
    /// This starts a background tokio task that processes commands from the
    /// `command_rx` channel. The task handles:
    /// - `PtyCommand::Input` - Writes data to the PTY
    /// - `PtyCommand::Resize` - Resizes the PTY (if client is size owner)
    /// - `PtyCommand::Connect` - Registers a new client and returns scrollback
    /// - `PtyCommand::Disconnect` - Removes a client
    ///
    /// The task runs until the command channel is closed (all senders dropped)
    /// or the PTY session is dropped.
    ///
    /// # Panics
    ///
    /// Panics if called more than once (command receiver already taken).
    pub fn spawn_command_processor(&mut self) {
        let rx = self
            .command_rx
            .take()
            .expect("Command receiver already taken - spawn_command_processor called twice?");

        let shared_state = self.shared_state_clone();
        let event_tx = self.event_sender();
        let scrollback_buffer = Arc::clone(&self.scrollback_buffer);

        let handle = tokio::spawn(async move {
            run_command_processor(rx, shared_state, event_tx, scrollback_buffer).await;
        });

        self.command_processor_handle = Some(handle);
        log::debug!("PTY command processor spawned");
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
            PtyCommand::Resize {
                client_id,
                rows,
                cols,
            } => {
                self.client_resized(client_id, (cols, rows));
            }
            PtyCommand::Connect {
                client_id,
                dims,
                response_tx,
            } => {
                // Caller subscribes separately via PtyHandle
                let (_subscription, scrollback) = self.connect(client_id, dims);
                let _ = response_tx.send(scrollback);
            }
            PtyCommand::Disconnect { client_id } => {
                self.disconnect(client_id);
            }
        }
    }

    // =========================================================================
    // Event Broadcasting
    // =========================================================================

    /// Subscribe to PTY events without registering as a client.
    ///
    /// Returns a broadcast receiver that will receive all future events.
    /// Use this for passive listeners that don't need size ownership.
    ///
    /// For clients that need size tracking, use [`connect()`](Self::connect) instead.
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
    // Client Connection
    // =========================================================================

    /// Connect a client and return a subscription with scrollback snapshot.
    ///
    /// The client is registered with their terminal dimensions and receives
    /// a broadcast receiver for PTY events. If this client becomes the size
    /// owner (newest client), the PTY is resized to their dimensions.
    ///
    /// # Atomicity
    ///
    /// The scrollback snapshot and subscription are obtained atomically.
    /// This ensures the client can replay the scrollback and then receive
    /// all subsequent events without missing any data.
    ///
    /// # Arguments
    ///
    /// * `client_id` - Unique identifier for this client
    /// * `dims` - Terminal dimensions as (cols, rows)
    ///
    /// # Returns
    ///
    /// A tuple of (broadcast receiver, scrollback snapshot). The scrollback
    /// contains all raw PTY output up to the moment of subscription.
    pub fn connect(
        &self,
        client_id: ClientId,
        dims: (u16, u16),
    ) -> (broadcast::Receiver<PtyEvent>, Vec<u8>) {
        {
            let mut state = self
                .shared_state
                .lock()
                .expect("shared_state lock poisoned");

            // Remove any existing connection for this client (reconnect case)
            state.connected_clients.retain(|c| c.id != client_id);

            // Add the new client
            let client = ConnectedClient::new(client_id.clone(), dims);
            state.connected_clients.push(client);

            // Sort by connection time - newest last
            state.connected_clients.sort_by_key(|c| c.connected_at);
        }

        // New client is now the size owner - resize PTY to their dimensions
        let (cols, rows) = dims;
        self.resize(rows, cols);

        // Broadcast owner changed event
        self.broadcast(PtyEvent::owner_changed(Some(client_id)));

        // Atomic: subscribe THEN snapshot (order matters!)
        // Subscribe first so we don't miss any events that arrive between
        // snapshot and subscribe. The client may see brief duplicates at the
        // boundary (data in both scrollback and first few events), but gaps
        // (missed data) are worse than duplicates.
        let subscription = self.subscribe();
        let scrollback = self.get_scrollback_snapshot();

        (subscription, scrollback)
    }

    /// Disconnect a client.
    ///
    /// Removes the client from the connected list. If this was the size owner,
    /// ownership passes to the next most recent client (if any) and the PTY
    /// is resized to their dimensions.
    pub fn disconnect(&self, client_id: ClientId) {
        let (was_owner, new_owner_info) = {
            let mut state = self
                .shared_state
                .lock()
                .expect("shared_state lock poisoned");

            let was_owner = state
                .connected_clients
                .last()
                .map(|o| o.id == client_id)
                .unwrap_or(false);

            state.connected_clients.retain(|c| c.id != client_id);

            let new_owner_info = if was_owner {
                state
                    .connected_clients
                    .last()
                    .map(|o| (o.id.clone(), o.dims))
            } else {
                None
            };

            (was_owner, new_owner_info)
        };

        // If the disconnected client was the owner, transfer ownership
        if was_owner {
            if let Some((new_owner_id, (cols, rows))) = new_owner_info {
                // Resize to new owner's dimensions
                self.resize(rows, cols);
                self.broadcast(PtyEvent::owner_changed(Some(new_owner_id)));
            } else {
                // No clients left
                self.broadcast(PtyEvent::owner_changed(None));
            }
        }
    }

    /// Update a client's terminal dimensions.
    ///
    /// If the client is the size owner, the PTY is resized and a resize
    /// event is broadcast. Otherwise, only the client's stored dimensions
    /// are updated.
    ///
    /// # Arguments
    ///
    /// * `client_id` - The client whose dimensions changed
    /// * `dims` - New dimensions as (cols, rows)
    pub fn client_resized(&self, client_id: ClientId, dims: (u16, u16)) {
        let is_owner = {
            let mut state = self
                .shared_state
                .lock()
                .expect("shared_state lock poisoned");

            let is_owner = state
                .connected_clients
                .last()
                .map(|o| o.id == client_id)
                .unwrap_or(false);

            // Update the client's stored dimensions
            if let Some(client) = state
                .connected_clients
                .iter_mut()
                .find(|c| c.id == client_id)
            {
                client.dims = dims;
            }

            is_owner
        };

        // If this is the owner, resize the PTY
        if is_owner {
            let (cols, rows) = dims;
            self.resize(rows, cols);
        }
    }

    /// Get a clone of the current size owner (newest connected client).
    ///
    /// Returns `None` if no clients are connected.
    #[must_use]
    pub fn size_owner(&self) -> Option<ConnectedClient> {
        // Newest client is last (sorted by connected_at)
        self.shared_state
            .lock()
            .expect("shared_state lock poisoned")
            .connected_clients
            .last()
            .cloned()
    }

    /// Get a clone of all connected clients.
    #[must_use]
    pub fn connected_clients(&self) -> Vec<ConnectedClient> {
        self.shared_state
            .lock()
            .expect("shared_state lock poisoned")
            .connected_clients
            .clone()
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
    event_tx: broadcast::Sender<PtyEvent>,
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
) {
    log::debug!("Command processor started");

    while let Some(cmd) = rx.recv().await {
        process_single_command(cmd, &shared_state, &event_tx, &scrollback_buffer);
    }

    log::debug!("Command processor exiting - channel closed");
}

/// Process a single PTY command.
///
/// Handles Input, Resize, Connect, and Disconnect commands using the shared
/// state. This is called from the async command processor task.
fn process_single_command(
    cmd: PtyCommand,
    shared_state: &Arc<Mutex<SharedPtyState>>,
    event_tx: &broadcast::Sender<PtyEvent>,
    scrollback_buffer: &Arc<Mutex<VecDeque<u8>>>,
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
        PtyCommand::Resize {
            client_id,
            rows,
            cols,
        } => {
            process_resize_command(&client_id, rows, cols, shared_state, event_tx);
        }
        PtyCommand::Connect {
            client_id,
            dims,
            response_tx,
        } => {
            let scrollback =
                process_connect_command(&client_id, dims, shared_state, event_tx, scrollback_buffer);
            let _ = response_tx.send(scrollback);
        }
        PtyCommand::Disconnect { client_id } => {
            process_disconnect_command(&client_id, shared_state, event_tx);
        }
    }
}

/// Process a Resize command.
///
/// Exposed as `pub(crate)` for direct sync resize from `PtyHandle`.
pub(crate) fn process_resize_command(
    client_id: &ClientId,
    rows: u16,
    cols: u16,
    shared_state: &Arc<Mutex<SharedPtyState>>,
    event_tx: &broadcast::Sender<PtyEvent>,
) {
    let should_resize = {
        let mut state = shared_state.lock().expect("shared_state lock poisoned");

        // Check if client is the size owner
        let is_owner = state
            .connected_clients
            .last()
            .map(|o| &o.id == client_id)
            .unwrap_or(false);

        // Update the client's stored dimensions
        if let Some(client) = state
            .connected_clients
            .iter_mut()
            .find(|c| &c.id == client_id)
        {
            client.dims = (cols, rows);
        }

        is_owner
    };

    // If this is the owner, resize the PTY
    if should_resize {
        do_resize(rows, cols, shared_state, event_tx);
    }
}

/// Process a Connect command.
///
/// Returns the scrollback buffer snapshot for the connecting client.
///
/// Exposed as `pub(crate)` for direct sync connect from `PtyHandle`.
pub(crate) fn process_connect_command(
    client_id: &ClientId,
    dims: (u16, u16),
    shared_state: &Arc<Mutex<SharedPtyState>>,
    event_tx: &broadcast::Sender<PtyEvent>,
    scrollback_buffer: &Arc<Mutex<VecDeque<u8>>>,
) -> Vec<u8> {
    {
        let mut state = shared_state.lock().expect("shared_state lock poisoned");

        // Remove any existing connection for this client (reconnect case)
        state.connected_clients.retain(|c| c.id != *client_id);

        // Add the new client
        let client = ConnectedClient::new(client_id.clone(), dims);
        state.connected_clients.push(client);

        // Sort by connection time - newest last
        state.connected_clients.sort_by_key(|c| c.connected_at);
    }

    // New client is now the size owner - resize PTY to their dimensions
    let (cols, rows) = dims;
    do_resize(rows, cols, shared_state, event_tx);

    // Broadcast owner changed event
    let _ = event_tx.send(PtyEvent::owner_changed(Some(client_id.clone())));

    // Return scrollback snapshot
    scrollback_buffer
        .lock()
        .expect("scrollback_buffer lock poisoned")
        .iter()
        .copied()
        .collect()
}

/// Process a Disconnect command.
///
/// Exposed as `pub(crate)` for direct sync disconnect from `PtyHandle`.
pub(crate) fn process_disconnect_command(
    client_id: &ClientId,
    shared_state: &Arc<Mutex<SharedPtyState>>,
    event_tx: &broadcast::Sender<PtyEvent>,
) {
    let (was_owner, new_owner_info) = {
        let mut state = shared_state.lock().expect("shared_state lock poisoned");

        let was_owner = state
            .connected_clients
            .last()
            .map(|o| &o.id == client_id)
            .unwrap_or(false);

        state.connected_clients.retain(|c| c.id != *client_id);

        let new_owner_info = if was_owner {
            state
                .connected_clients
                .last()
                .map(|o| (o.id.clone(), o.dims))
        } else {
            None
        };

        (was_owner, new_owner_info)
    };

    // If the disconnected client was the owner, transfer ownership
    if was_owner {
        if let Some((new_owner_id, (cols, rows))) = new_owner_info {
            // Resize to new owner's dimensions
            do_resize(rows, cols, shared_state, event_tx);
            let _ = event_tx.send(PtyEvent::owner_changed(Some(new_owner_id)));
        } else {
            // No clients left
            let _ = event_tx.send(PtyEvent::owner_changed(None));
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
        assert!(session.connected_clients().is_empty());
        assert!(session.size_owner().is_none());
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

        // Multiple subscriptions should work
        assert!(session.connected_clients().is_empty()); // subscribe doesn't add client
    }

    #[test]
    fn test_pty_session_connect_single_client() {
        let session = PtySession::new(24, 80);

        let (_rx, _scrollback) = session.connect(ClientId::Tui, (80, 24));

        assert_eq!(session.connected_clients().len(), 1);
        assert!(session.size_owner().is_some());
        assert_eq!(session.size_owner().unwrap().id, ClientId::Tui);
    }

    #[test]
    fn test_pty_session_connect_multiple_clients() {
        let session = PtySession::new(24, 80);

        let (_rx1, _scrollback1) = session.connect(ClientId::Tui, (80, 24));
        let (_rx2, _scrollback2) = session.connect(ClientId::browser("abc123"), (120, 40));

        assert_eq!(session.connected_clients().len(), 2);
        // Newest client (browser) should be the owner
        assert_eq!(
            session.size_owner().unwrap().id,
            ClientId::browser("abc123")
        );
    }

    #[test]
    fn test_pty_session_disconnect() {
        let session = PtySession::new(24, 80);

        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));
        let (_rx2, _) = session.connect(ClientId::browser("abc123"), (120, 40));

        // Disconnect the owner
        session.disconnect(ClientId::browser("abc123"));

        assert_eq!(session.connected_clients().len(), 1);
        // TUI should now be the owner
        assert_eq!(session.size_owner().unwrap().id, ClientId::Tui);
    }

    #[test]
    fn test_pty_session_disconnect_last_client() {
        let session = PtySession::new(24, 80);

        let (_rx, _) = session.connect(ClientId::Tui, (80, 24));
        session.disconnect(ClientId::Tui);

        assert!(session.connected_clients().is_empty());
        assert!(session.size_owner().is_none());
    }

    #[test]
    fn test_pty_session_client_resized_owner() {
        let session = PtySession::new(24, 80);

        let (_rx, _) = session.connect(ClientId::Tui, (80, 24));
        session.client_resized(ClientId::Tui, (100, 30));

        let owner = session.size_owner().unwrap();
        assert_eq!(owner.dims, (100, 30));
    }

    #[test]
    fn test_pty_session_client_resized_non_owner() {
        let session = PtySession::new(24, 80);

        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));
        let (_rx2, _) = session.connect(ClientId::browser("abc123"), (120, 40));

        // Resize TUI (not the owner)
        session.client_resized(ClientId::Tui, (100, 30));

        // TUI's dims should be updated
        let clients = session.connected_clients();
        let tui_client = clients.iter().find(|c| c.id == ClientId::Tui).unwrap();
        assert_eq!(tui_client.dims, (100, 30));

        // Owner should still be browser with original dims
        let owner = session.size_owner().unwrap();
        assert_eq!(owner.id, ClientId::browser("abc123"));
        assert_eq!(owner.dims, (120, 40));
    }

    #[test]
    fn test_pty_session_reconnect_same_client() {
        let session = PtySession::new(24, 80);

        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));
        let (_rx2, _) = session.connect(ClientId::Tui, (100, 30)); // Reconnect with new dims

        // Should only have one client entry
        assert_eq!(session.connected_clients().len(), 1);
        assert_eq!(session.size_owner().unwrap().dims, (100, 30));
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
    fn test_pty_session_connect_returns_scrollback() {
        // Verify that connect() returns the scrollback snapshot atomically
        let session = PtySession::new(24, 80);

        // Add some data to scrollback before connecting
        session.add_to_scrollback(b"existing output\n");
        session.add_to_scrollback(b"more history\n");

        // Connect should return both subscription and scrollback
        let (_rx, scrollback) = session.connect(ClientId::Tui, (80, 24));

        // Scrollback should contain all data added before connect
        assert_eq!(scrollback, b"existing output\nmore history\n");
    }

    #[test]
    fn test_pty_session_scrollback_limit() {
        let session = PtySession::new(24, 80);

        // Add more bytes than MAX_SCROLLBACK_BYTES
        let chunk = vec![b'x'; 1024]; // 1KB chunks
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

        // Broadcast an event
        let count = session.broadcast(PtyEvent::output(b"hello".to_vec()));
        assert_eq!(count, 1);

        // Receiver should get the event
        let event = rx.try_recv().unwrap();
        match event {
            PtyEvent::Output(data) => assert_eq!(data, b"hello"),
            _ => panic!("Expected Output event"),
        }
    }

    #[test]
    fn test_pty_session_broadcast_no_receivers() {
        let session = PtySession::new(24, 80);

        // Broadcasting with no receivers should not panic
        let count = session.broadcast(PtyEvent::output(b"hello".to_vec()));
        assert_eq!(count, 0);
    }

    #[test]
    fn test_pty_session_event_sender() {
        let session = PtySession::new(24, 80);

        let tx = session.event_sender();
        let mut rx = session.subscribe();

        // Send via the cloned sender
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
        assert!(debug.contains("connected_clients"));
    }

    // =========================================================================
    // Multi-Client Lifecycle Tests
    // =========================================================================

    #[test]
    fn test_pty_session_multiple_clients_connect_verifies_dimensions() {
        // Test: Connect client A, then client B
        // Verify: B is size owner (newest)
        // Verify: PTY resized to B's dimensions
        let session = PtySession::new(24, 80);

        // Initial dimensions
        assert_eq!(session.dimensions(), (24, 80));

        // Connect client A with 80x24 (cols, rows)
        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));

        // After A connects: A is owner, PTY resized to A's dims
        assert_eq!(session.size_owner().unwrap().id, ClientId::Tui);
        assert_eq!(session.dimensions(), (24, 80)); // (rows, cols)

        // Connect client B with 120x40
        let (_rx2, _) = session.connect(ClientId::browser("browser1"), (120, 40));

        // After B connects: B is owner (newest), PTY resized to B's dims
        assert_eq!(
            session.size_owner().unwrap().id,
            ClientId::browser("browser1")
        );
        assert_eq!(session.dimensions(), (40, 120)); // (rows, cols)

        // Connect client C with 100x30
        let (_rx3, _) = session.connect(ClientId::browser("browser2"), (100, 30));

        // After C connects: C is owner (newest), PTY resized to C's dims
        assert_eq!(
            session.size_owner().unwrap().id,
            ClientId::browser("browser2")
        );
        assert_eq!(session.dimensions(), (30, 100)); // (rows, cols)
        assert_eq!(session.connected_clients().len(), 3);
    }

    #[test]
    fn test_pty_session_owner_disconnect_fallback_with_dimensions() {
        // Test: Connect A, connect B (B is owner), disconnect B
        // Verify: A becomes owner
        // Verify: PTY resized to A's dimensions
        let session = PtySession::new(24, 80);

        // Connect A with 80x24
        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));
        assert_eq!(session.dimensions(), (24, 80));

        // Connect B with 120x40 - B becomes owner
        let (_rx2, _) = session.connect(ClientId::browser("browser1"), (120, 40));
        assert_eq!(session.dimensions(), (40, 120));
        assert_eq!(
            session.size_owner().unwrap().id,
            ClientId::browser("browser1")
        );

        // Disconnect B (the owner)
        session.disconnect(ClientId::browser("browser1"));

        // A should now be owner with PTY resized to A's dimensions
        assert_eq!(session.connected_clients().len(), 1);
        assert_eq!(session.size_owner().unwrap().id, ClientId::Tui);
        assert_eq!(session.dimensions(), (24, 80)); // Resized back to A's dims
    }

    #[test]
    fn test_pty_session_owner_disconnect_fallback_chain() {
        // Test: Connect A, B, C (C is owner)
        // Disconnect C -> B becomes owner with B's dims
        // Disconnect B -> A becomes owner with A's dims
        let session = PtySession::new(24, 80);

        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));
        let (_rx2, _) = session.connect(ClientId::browser("b1"), (100, 30));
        let (_rx3, _) = session.connect(ClientId::browser("b2"), (120, 40));

        assert_eq!(session.dimensions(), (40, 120)); // C's dims
        assert_eq!(session.size_owner().unwrap().id, ClientId::browser("b2"));

        // Disconnect C
        session.disconnect(ClientId::browser("b2"));
        assert_eq!(session.dimensions(), (30, 100)); // B's dims
        assert_eq!(session.size_owner().unwrap().id, ClientId::browser("b1"));

        // Disconnect B
        session.disconnect(ClientId::browser("b1"));
        assert_eq!(session.dimensions(), (24, 80)); // A's dims
        assert_eq!(session.size_owner().unwrap().id, ClientId::Tui);
    }

    #[test]
    fn test_pty_session_broadcasts_owner_changed_on_connect() {
        // Test: Connect client, verify OwnerChanged event broadcast
        let session = PtySession::new(24, 80);

        // Subscribe BEFORE connecting
        let mut rx = session.subscribe();

        // Connect client
        let (_client_rx, _) = session.connect(ClientId::Tui, (80, 24));

        // connect() broadcasts Resized first, then OwnerChanged
        let resize_event = rx.try_recv().expect("Should receive Resized event");
        assert!(resize_event.is_resized(), "First event should be Resized");

        // Then OwnerChanged event with the new owner
        let owner_event = rx.try_recv().expect("Should receive OwnerChanged event");
        match owner_event {
            PtyEvent::OwnerChanged { new_owner } => {
                assert_eq!(new_owner, Some(ClientId::Tui));
            }
            other => panic!("Expected OwnerChanged, got {:?}", other),
        }
    }

    #[test]
    fn test_pty_session_broadcasts_owner_changed_on_disconnect() {
        // Test: Connect two clients, disconnect owner, verify OwnerChanged broadcast
        let session = PtySession::new(24, 80);

        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));
        let (_rx2, _) = session.connect(ClientId::browser("b1"), (120, 40));

        // Subscribe after connecting both
        let mut rx = session.subscribe();

        // Disconnect the owner (browser)
        session.disconnect(ClientId::browser("b1"));

        // Should receive Resized event (from fallback resize)
        let resize_event = rx.try_recv().expect("Should receive Resized event");
        assert!(resize_event.is_resized());

        // Should receive OwnerChanged event with new owner
        let owner_event = rx.try_recv().expect("Should receive OwnerChanged event");
        match owner_event {
            PtyEvent::OwnerChanged { new_owner } => {
                assert_eq!(new_owner, Some(ClientId::Tui));
            }
            other => panic!("Expected OwnerChanged, got {:?}", other),
        }
    }

    #[test]
    fn test_pty_session_broadcasts_owner_changed_none_when_last_disconnects() {
        // Test: Connect single client, disconnect, verify OwnerChanged(None)
        let session = PtySession::new(24, 80);

        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));

        // Subscribe after connecting
        let mut rx = session.subscribe();

        // Disconnect the only client
        session.disconnect(ClientId::Tui);

        // Should receive OwnerChanged(None)
        let event = rx.try_recv().expect("Should receive OwnerChanged event");
        match event {
            PtyEvent::OwnerChanged { new_owner } => {
                assert_eq!(new_owner, None);
            }
            other => panic!("Expected OwnerChanged(None), got {:?}", other),
        }
    }

    #[test]
    fn test_pty_session_client_resize_only_affects_owner() {
        // Test: Connect A, connect B (B is owner)
        // A resizes - should NOT affect PTY
        // B resizes - SHOULD affect PTY
        let session = PtySession::new(24, 80);

        // Connect A with 80x24
        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));
        assert_eq!(session.dimensions(), (24, 80));

        // Connect B with 120x40 - B becomes owner
        let (_rx2, _) = session.connect(ClientId::browser("b1"), (120, 40));
        assert_eq!(session.dimensions(), (40, 120));

        // A resizes to 90x25 - should NOT affect PTY (A is not owner)
        session.client_resized(ClientId::Tui, (90, 25));

        // PTY should still have B's dimensions
        assert_eq!(session.dimensions(), (40, 120));

        // But A's stored dims should be updated
        let clients = session.connected_clients();
        let a_client = clients.iter().find(|c| c.id == ClientId::Tui).unwrap();
        assert_eq!(a_client.dims, (90, 25));

        // B resizes to 150x50 - SHOULD affect PTY (B is owner)
        session.client_resized(ClientId::browser("b1"), (150, 50));

        // PTY should now have B's new dimensions
        assert_eq!(session.dimensions(), (50, 150));
    }

    #[test]
    fn test_pty_session_resize_broadcasts_event() {
        // Verify that client resize as owner broadcasts a Resized event
        let session = PtySession::new(24, 80);

        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));

        // Subscribe after initial connect
        let mut rx = session.subscribe();

        // Owner resizes
        session.client_resized(ClientId::Tui, (100, 30));

        // Should receive Resized event
        let event = rx.try_recv().expect("Should receive Resized event");
        match event {
            PtyEvent::Resized { rows, cols } => {
                assert_eq!(rows, 30);
                assert_eq!(cols, 100);
            }
            other => panic!("Expected Resized, got {:?}", other),
        }
    }

    #[test]
    fn test_pty_session_non_owner_resize_no_broadcast() {
        // Verify that non-owner resize does NOT broadcast a Resized event
        let session = PtySession::new(24, 80);

        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));
        let (_rx2, _) = session.connect(ClientId::browser("b1"), (120, 40));

        // Subscribe after both connected
        let mut rx = session.subscribe();

        // Non-owner (TUI) resizes
        session.client_resized(ClientId::Tui, (90, 25));

        // Should NOT receive any event (non-owner resize is silent)
        let result = rx.try_recv();
        assert!(
            result.is_err(),
            "Non-owner resize should not broadcast, got {:?}",
            result
        );
    }

    #[test]
    fn test_pty_session_disconnect_non_owner_no_ownership_change() {
        // Verify disconnecting a non-owner doesn't trigger ownership change
        let session = PtySession::new(24, 80);

        let (_rx1, _) = session.connect(ClientId::Tui, (80, 24));
        let (_rx2, _) = session.connect(ClientId::browser("b1"), (120, 40));

        // B is owner, PTY has B's dims
        assert_eq!(session.size_owner().unwrap().id, ClientId::browser("b1"));
        assert_eq!(session.dimensions(), (40, 120));

        // Subscribe before disconnecting
        let mut rx = session.subscribe();

        // Disconnect A (not the owner)
        session.disconnect(ClientId::Tui);

        // B should still be owner, dims unchanged
        assert_eq!(session.size_owner().unwrap().id, ClientId::browser("b1"));
        assert_eq!(session.dimensions(), (40, 120));

        // Should NOT receive any events (no ownership change)
        let result = rx.try_recv();
        assert!(
            result.is_err(),
            "Disconnecting non-owner should not broadcast, got {:?}",
            result
        );
    }

    // =========================================================================
    // Hot Path Tests - PTY Output Broadcasting
    // =========================================================================
    // These tests verify the critical output hot path:
    // PTY process writes -> Reader thread reads -> broadcast::send() ->
    //   -> Multiple subscribers receive Output events

    #[test]
    fn test_hot_path_broadcast_to_multiple_subscribers() {
        // CRITICAL: Tests that Output events reach ALL subscribers simultaneously.
        // This is the core of the hot path fan-out - simulating TUI + Browser clients.
        let session = PtySession::new(24, 80);

        // Create multiple subscribers (simulating TUI + multiple Browser clients)
        let mut rx1 = session.subscribe();
        let mut rx2 = session.subscribe();
        let mut rx3 = session.subscribe();

        // Broadcast output event (simulating reader thread behavior)
        let count = session.broadcast(PtyEvent::output(b"hello world".to_vec()));
        assert_eq!(count, 3, "All 3 subscribers should receive the event");

        // Verify all receivers got the same data
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
        // Tests that multiple Output events arrive in order (FIFO guarantee).
        // Critical for terminal rendering correctness.
        let session = PtySession::new(24, 80);
        let mut rx = session.subscribe();

        // Broadcast multiple events in sequence (simulating rapid PTY output)
        session.broadcast(PtyEvent::output(b"first".to_vec()));
        session.broadcast(PtyEvent::output(b"second".to_vec()));
        session.broadcast(PtyEvent::output(b"third".to_vec()));

        // Verify ordering is preserved
        let expected = [
            b"first".as_slice(),
            b"second".as_slice(),
            b"third".as_slice(),
        ];
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
        // Tests that dropping one subscriber doesn't affect others.
        // Critical for robustness when browser clients disconnect.
        let session = PtySession::new(24, 80);

        let mut rx1 = session.subscribe();
        let rx2 = session.subscribe(); // Will be dropped
        let mut rx3 = session.subscribe();

        // Drop rx2 before broadcasting (simulates browser disconnect)
        drop(rx2);

        // Broadcast should still succeed for remaining subscribers
        let count = session.broadcast(PtyEvent::output(b"test".to_vec()));
        assert_eq!(count, 2, "Should deliver to 2 remaining subscribers");

        // Verify remaining receivers got the event
        assert!(rx1.try_recv().is_ok());
        assert!(rx3.try_recv().is_ok());
    }

    #[test]
    fn test_hot_path_event_sender_for_reader_thread() {
        // Tests the pattern used by spawn_cli_reader_thread:
        // Get a cloned sender and use it from a separate context.
        // This is exactly how the reader thread broadcasts output.
        let session = PtySession::new(24, 80);
        let tx = session.event_sender();
        let mut rx = session.subscribe();

        // Simulate reader thread sending output
        let output_data = b"PTY output from reader thread";
        let _ = tx.send(PtyEvent::output(output_data.to_vec()));

        // Subscriber receives the event
        let event = rx
            .try_recv()
            .expect("Should receive event from cloned sender");
        match event {
            PtyEvent::Output(data) => assert_eq!(data, output_data),
            _ => panic!("Expected Output event"),
        }
    }

    #[test]
    fn test_hot_path_high_volume_broadcast() {
        // Tests broadcasting many events quickly (simulates high output rate).
        // The hot path must handle burst traffic without dropping events.
        let session = PtySession::new(24, 80);
        let mut rx = session.subscribe();

        // Broadcast 100 events rapidly (simulating fast command output)
        for i in 0..100 {
            let data = format!("chunk-{}", i);
            session.broadcast(PtyEvent::output(data.into_bytes()));
        }

        // Verify all 100 events arrived in order
        for i in 0..100 {
            let expected = format!("chunk-{}", i);
            let event = rx
                .try_recv()
                .unwrap_or_else(|_| panic!("Event {} should exist", i));
            match event {
                PtyEvent::Output(data) => {
                    assert_eq!(
                        String::from_utf8_lossy(&data),
                        expected,
                        "Event {} wrong",
                        i
                    );
                }
                _ => panic!("Event {} should be Output", i),
            }
        }
    }

    #[test]
    fn test_hot_path_connect_returns_working_subscription() {
        // Tests that connect() returns a subscription that receives events.
        // This is how TuiClient and BrowserClient get their event streams.
        let session = PtySession::new(24, 80);

        // Connect returns a subscription + scrollback (via connect, not subscribe)
        let (mut rx, _scrollback) = session.connect(ClientId::Tui, (80, 24));

        // Send an output event after connection
        session.broadcast(PtyEvent::output(b"after connect".to_vec()));

        // Drain any OwnerChanged events first
        loop {
            match rx.try_recv() {
                Ok(PtyEvent::Output(data)) => {
                    assert_eq!(data, b"after connect");
                    return; // Success
                }
                Ok(_) => continue, // Skip non-Output events
                Err(_) => break,
            }
        }
        panic!("Connected subscription should receive Output events");
    }

    // =========================================================================
    // Command Processor Tests
    // =========================================================================
    // These tests verify the async command processor task behavior.

    /// Helper to create a test scrollback buffer.
    fn test_scrollback() -> Arc<Mutex<VecDeque<u8>>> {
        Arc::new(Mutex::new(VecDeque::new()))
    }

    #[tokio::test]
    async fn test_command_processor_input_command() {
        // Test that Input commands are processed correctly
        let (event_tx, _) = broadcast::channel(16);
        let (cmd_tx, cmd_rx) = mpsc::channel::<PtyCommand>(16);
        let scrollback = test_scrollback();

        let shared_state = Arc::new(Mutex::new(SharedPtyState {
            master_pty: None,
            writer: None, // No writer - input will be silently ignored
            dimensions: (24, 80),
            connected_clients: Vec::new(),
        }));

        // Process a single command
        process_single_command(
            PtyCommand::Input(b"test input".to_vec()),
            &shared_state,
            &event_tx,
            &scrollback,
        );

        // Command should be processed without panic (no writer available)
        // This verifies the command processor handles missing writer gracefully
        drop(cmd_tx);
        drop(cmd_rx);
    }

    #[tokio::test]
    async fn test_command_processor_connect_command() {
        // Test that Connect commands register clients correctly
        let (event_tx, mut event_rx) = broadcast::channel(16);
        let scrollback = test_scrollback();

        // Add some data to scrollback
        scrollback
            .lock()
            .unwrap()
            .extend(b"existing scrollback".iter());

        let shared_state = Arc::new(Mutex::new(SharedPtyState {
            master_pty: None,
            writer: None,
            dimensions: (24, 80),
            connected_clients: Vec::new(),
        }));

        // Create response channel
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();

        // Process Connect command
        process_single_command(
            PtyCommand::Connect {
                client_id: ClientId::Tui,
                dims: (100, 40),
                response_tx,
            },
            &shared_state,
            &event_tx,
            &scrollback,
        );

        // Verify client was added
        let state = shared_state.lock().unwrap();
        assert_eq!(state.connected_clients.len(), 1);
        assert_eq!(state.connected_clients[0].id, ClientId::Tui);
        assert_eq!(state.connected_clients[0].dims, (100, 40));
        drop(state);

        // Verify scrollback was sent via response channel
        let received_scrollback = response_rx.await.unwrap();
        assert_eq!(received_scrollback, b"existing scrollback");

        // Verify events were broadcast (Resized + OwnerChanged)
        let resize_event = event_rx.try_recv().expect("Should receive Resized event");
        assert!(matches!(resize_event, PtyEvent::Resized { .. }));

        let owner_event = event_rx
            .try_recv()
            .expect("Should receive OwnerChanged event");
        match owner_event {
            PtyEvent::OwnerChanged { new_owner } => {
                assert_eq!(new_owner, Some(ClientId::Tui));
            }
            _ => panic!("Expected OwnerChanged event"),
        }
    }

    #[tokio::test]
    async fn test_command_processor_disconnect_command() {
        // Test that Disconnect commands remove clients correctly
        let (event_tx, mut event_rx) = broadcast::channel(16);
        let scrollback = test_scrollback();

        let shared_state = Arc::new(Mutex::new(SharedPtyState {
            master_pty: None,
            writer: None,
            dimensions: (24, 80),
            connected_clients: vec![ConnectedClient::new(ClientId::Tui, (80, 24))],
        }));

        // Process Disconnect command
        process_single_command(
            PtyCommand::Disconnect {
                client_id: ClientId::Tui,
            },
            &shared_state,
            &event_tx,
            &scrollback,
        );

        // Verify client was removed
        let state = shared_state.lock().unwrap();
        assert!(state.connected_clients.is_empty());
        drop(state);

        // Verify OwnerChanged(None) was broadcast
        let event = event_rx
            .try_recv()
            .expect("Should receive OwnerChanged event");
        match event {
            PtyEvent::OwnerChanged { new_owner } => {
                assert_eq!(new_owner, None);
            }
            _ => panic!("Expected OwnerChanged(None) event"),
        }
    }

    #[tokio::test]
    async fn test_command_processor_resize_command_owner() {
        // Test that Resize from owner updates dimensions
        let (event_tx, mut event_rx) = broadcast::channel(16);
        let scrollback = test_scrollback();

        let shared_state = Arc::new(Mutex::new(SharedPtyState {
            master_pty: None,
            writer: None,
            dimensions: (24, 80),
            connected_clients: vec![ConnectedClient::new(ClientId::Tui, (80, 24))],
        }));

        // Process Resize command from owner
        process_single_command(
            PtyCommand::Resize {
                client_id: ClientId::Tui,
                rows: 50,
                cols: 120,
            },
            &shared_state,
            &event_tx,
            &scrollback,
        );

        // Verify dimensions were updated
        let state = shared_state.lock().unwrap();
        assert_eq!(state.dimensions, (50, 120));
        assert_eq!(state.connected_clients[0].dims, (120, 50)); // (cols, rows)
        drop(state);

        // Verify Resized event was broadcast
        let event = event_rx.try_recv().expect("Should receive Resized event");
        match event {
            PtyEvent::Resized { rows, cols } => {
                assert_eq!(rows, 50);
                assert_eq!(cols, 120);
            }
            _ => panic!("Expected Resized event"),
        }
    }

    #[tokio::test]
    async fn test_command_processor_resize_command_non_owner() {
        // Test that Resize from non-owner only updates client dims, not PTY
        let (event_tx, mut event_rx) = broadcast::channel(16);
        let scrollback = test_scrollback();

        let shared_state = Arc::new(Mutex::new(SharedPtyState {
            master_pty: None,
            writer: None,
            dimensions: (40, 120), // Browser is owner
            connected_clients: vec![
                ConnectedClient::new(ClientId::Tui, (80, 24)),
                ConnectedClient::new(ClientId::browser("b1"), (120, 40)),
            ],
        }));

        // Process Resize command from non-owner (TUI)
        process_single_command(
            PtyCommand::Resize {
                client_id: ClientId::Tui,
                rows: 30,
                cols: 100,
            },
            &shared_state,
            &event_tx,
            &scrollback,
        );

        // Verify PTY dimensions unchanged
        let state = shared_state.lock().unwrap();
        assert_eq!(state.dimensions, (40, 120)); // Still browser's dims

        // Verify TUI's stored dims were updated
        let tui = state
            .connected_clients
            .iter()
            .find(|c| c.id == ClientId::Tui)
            .unwrap();
        assert_eq!(tui.dims, (100, 30)); // Updated to new dims
        drop(state);

        // Verify NO Resized event was broadcast (non-owner resize is silent)
        assert!(
            event_rx.try_recv().is_err(),
            "Non-owner resize should not broadcast"
        );
    }

    #[test]
    fn test_spawn_command_processor_takes_receiver() {
        // Test that spawn_command_processor takes the command receiver
        let mut session = PtySession::new(24, 80);

        // Before spawning, command_rx should exist
        assert!(session.command_rx.is_some());
        assert!(session.command_processor_handle.is_none());

        // Spawn the processor (requires tokio runtime)
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            session.spawn_command_processor();
        });

        // After spawning, command_rx should be taken
        assert!(session.command_rx.is_none());
        assert!(session.command_processor_handle.is_some());
    }

    #[test]
    #[should_panic(expected = "Command receiver already taken")]
    fn test_spawn_command_processor_panics_if_called_twice() {
        // Test that calling spawn_command_processor twice panics
        let mut session = PtySession::new(24, 80);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            session.spawn_command_processor();
            session.spawn_command_processor(); // Should panic
        });
    }
}
