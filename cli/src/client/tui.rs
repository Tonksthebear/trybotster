//! TUI client implementation for the local terminal interface.
//!
//! `TuiClient` handles I/O routing for the local terminal, analogous to how
//! `BrowserClient` routes I/O to the web browser. The actual terminal emulation
//! (vt100 parsing) happens in `TuiRunner`, just as the web browser handles
//! its own terminal rendering.
//!
//! # Architecture
//!
//! ```text
//! TuiClient (I/O routing)          BrowserClient (I/O routing)
//!   ├── hub_handle                   ├── hub_handle
//!   ├── dims                         ├── dims
//!   ├── output_sink                  └── terminal_channels
//!   └── output_task                        │
//!          │                               ▼
//!          ▼                         Web Browser (rendering)
//! TuiRunner (rendering)               └── xterm.js
//!   └── vt100_parser
//! ```
//!
//! # Event Flow
//!
//! 1. PTY emits `PtyEvent::Output` → forwarder task receives via broadcast
//!    → sends `TuiOutput::Output` through channel → TuiRunner receives and
//!    feeds to its vt100_parser
//! 2. User types → TuiRunner routes input via hub_handle
//! 3. Agent selected → `connect_to_pty()` fetches scrollback, sends via channel,
//!    spawns forwarder task
//!
//! # Why No Parser Here
//!
//! TuiClient is like BrowserClient - it routes bytes, it doesn't parse them.
//! TuiRunner owns the vt100 parser, just as the web browser owns xterm.js.

// Rust guideline compliant 2026-01

use tokio::sync::broadcast;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use crate::agent::pty::PtyEvent;
use crate::hub::hub_handle::HubHandle;

use super::{Client, ClientId};

/// Output messages sent from TuiClient to TuiRunner.
///
/// Mirrors `TerminalMessage` used by BrowserClient. TuiRunner receives these
/// through the channel and processes them (feeding to vt100 parser, handling
/// process exit, etc.).
#[derive(Debug, Clone)]
pub enum TuiOutput {
    /// Historical output from before connection.
    ///
    /// Sent once when connecting to a PTY, contains the scrollback buffer.
    Scrollback(Vec<u8>),

    /// Ongoing PTY output.
    ///
    /// Sent whenever the PTY produces new output.
    Output(Vec<u8>),

    /// PTY process exited.
    ///
    /// Sent when the PTY process terminates. TuiRunner should handle this
    /// appropriately (e.g., show exit status, disable input).
    ProcessExited {
        /// Exit code from the PTY process, if available.
        exit_code: Option<i32>,
    },
}

/// TUI client - I/O routing for the local terminal.
///
/// Routes PTY events to TuiRunner via a channel, analogous to how BrowserClient
/// routes to the browser via WebSocket. Does NOT parse terminal output - that's
/// TuiRunner's job.
///
/// # What's Here (I/O Routing)
///
/// - Hub access via `hub_handle`
/// - Client identity (`id`)
/// - Terminal dimensions (`dims`)
/// - Output sink channel (`output_sink`)
/// - Output forwarder task (`output_task`)
///
/// # What's NOT Here (Rendering - in TuiRunner)
///
/// - vt100 parser (TuiRunner owns this)
/// - Scroll position (TuiRunner manages)
/// - Selection state (TuiRunner manages)
pub struct TuiClient {
    /// Thread-safe access to Hub state and operations.
    hub_handle: HubHandle,

    /// Unique identifier (always `ClientId::Tui`).
    id: ClientId,

    /// Terminal dimensions (cols, rows).
    dims: (u16, u16),

    /// Channel sender for PTY output to TuiRunner.
    ///
    /// When connected to a PTY, output events are forwarded through this channel.
    /// TuiRunner owns the receiver end.
    output_sink: UnboundedSender<TuiOutput>,

    /// Handle to the output forwarder task.
    ///
    /// Spawned by `connect_to_pty()`, aborted by `disconnect_from_pty()`.
    /// Unlike BrowserClient's `terminal_channels` (which supports multiple
    /// simultaneous connections), TUI only connects to one PTY at a time.
    output_task: Option<JoinHandle<()>>,

    /// Currently connected PTY indices, if any.
    ///
    /// Stores (agent_index, pty_index) when connected to a PTY.
    /// Used by `set_dims()` to propagate resize to the connected PTY.
    connected_pty: Option<(usize, usize)>,
}

impl TuiClient {
    /// Create a new TUI client with default dimensions.
    ///
    /// # Arguments
    ///
    /// * `hub_handle` - Handle for Hub communication and agent queries.
    /// * `output_sink` - Channel sender for PTY output to TuiRunner.
    #[must_use]
    pub fn new(hub_handle: HubHandle, output_sink: UnboundedSender<TuiOutput>) -> Self {
        Self::with_dims(hub_handle, output_sink, 80, 24)
    }

    /// Create a new TUI client with specific dimensions.
    ///
    /// # Arguments
    ///
    /// * `hub_handle` - Handle for Hub communication and agent queries.
    /// * `output_sink` - Channel sender for PTY output to TuiRunner.
    /// * `cols` - Terminal width in columns.
    /// * `rows` - Terminal height in rows.
    #[must_use]
    pub fn with_dims(
        hub_handle: HubHandle,
        output_sink: UnboundedSender<TuiOutput>,
        cols: u16,
        rows: u16,
    ) -> Self {
        Self {
            hub_handle,
            id: ClientId::Tui,
            dims: (cols, rows),
            output_sink,
            output_task: None,
            connected_pty: None,
        }
    }

    /// Get the hub handle for Hub communication.
    #[must_use]
    pub fn hub_handle(&self) -> &HubHandle {
        &self.hub_handle
    }

    /// Update terminal dimensions.
    ///
    /// Called when the terminal is resized. Does NOT propagate to connected PTY -
    /// use `set_dims()` for that (which is a trait method).
    pub fn update_dims(&mut self, cols: u16, rows: u16) {
        self.dims = (cols, rows);
    }
}

impl std::fmt::Debug for TuiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiClient")
            .field("id", &self.id)
            .field("dims", &self.dims)
            .field("connected_pty", &self.connected_pty)
            .finish_non_exhaustive()
    }
}

impl Client for TuiClient {
    fn id(&self) -> &ClientId {
        &self.id
    }

    fn dims(&self) -> (u16, u16) {
        self.dims
    }

    fn hub_handle(&self) -> &HubHandle {
        &self.hub_handle
    }

    fn set_dims(&mut self, cols: u16, rows: u16) {
        self.dims = (cols, rows);

        // Propagate resize to connected PTY if any
        if let Some((agent_idx, pty_idx)) = self.connected_pty {
            if let Err(e) = self.resize_pty(agent_idx, pty_idx, rows, cols) {
                log::debug!("Failed to resize PTY on dims change: {}", e);
            }
        }
    }

    /// Connect to a PTY and start forwarding output.
    ///
    /// Mirrors BrowserClient's `connect_to_pty()`:
    /// 1. Aborts previous output task if any
    /// 2. Gets agent/PTY handles
    /// 3. Calls `pty.connect_blocking()` to get scrollback
    /// 4. Sends scrollback through output channel
    /// 5. Subscribes to PTY events
    /// 6. Spawns forwarder task to route events to TuiRunner
    ///
    /// # Returns
    ///
    /// - `Ok(())` on success
    /// - `Err(message)` if agent/PTY not found or connection fails
    fn connect_to_pty(&mut self, agent_index: usize, pty_index: usize) -> Result<(), String> {
        // Abort previous output task if any (like BrowserClient removes old channel).
        if let Some(task) = self.output_task.take() {
            task.abort();
        }
        self.connected_pty = None;

        // Get agent handle from Hub.
        let agent_handle = self
            .hub_handle
            .get_agent(agent_index)
            .ok_or_else(|| format!("Agent at index {} not found", agent_index))?;

        // Get PTY handle from agent.
        let pty_handle = agent_handle
            .get_pty(pty_index)
            .ok_or_else(|| format!("PTY at index {} not found for agent", pty_index))?
            .clone();

        // Connect to PTY and get scrollback BEFORE spawning forwarder.
        // This ensures TuiRunner receives historical output first.
        let scrollback = pty_handle.connect_blocking(self.id.clone(), self.dims)?;

        // Send scrollback to TuiRunner if available.
        if !scrollback.is_empty() {
            // If receiver is dropped, this will fail - that's fine, we'll discover
            // it when the forwarder task tries to send.
            let _ = self.output_sink.send(TuiOutput::Scrollback(scrollback));
        }

        // Subscribe to PTY events for output forwarding.
        let pty_rx = pty_handle.subscribe();

        // Get tokio runtime from hub_handle.
        let runtime = self
            .hub_handle
            .tokio_runtime()
            .ok_or_else(|| "No tokio runtime available".to_string())?;

        // Spawn output forwarder: PTY -> TuiRunner.
        let sink = self.output_sink.clone();
        self.output_task = Some(runtime.spawn(spawn_tui_output_forwarder(pty_rx, sink)));

        // Track connected PTY indices for resize propagation.
        self.connected_pty = Some((agent_index, pty_index));

        log::info!(
            "TUI connected to PTY ({}, {})",
            agent_index,
            pty_index
        );

        Ok(())
    }

    fn disconnect_from_pty(&mut self, agent_index: usize, pty_index: usize) {
        // Abort output forwarder task if running.
        if let Some(task) = self.output_task.take() {
            task.abort();
        }
        self.connected_pty = None;

        // Notify PTY of disconnection.
        if let Some(agent) = self.hub_handle.get_agent(agent_index) {
            if let Some(pty) = agent.get_pty(pty_index) {
                let _ = pty.disconnect_blocking(self.id.clone());
            }
        }

        log::info!(
            "TUI disconnected from PTY ({}, {})",
            agent_index,
            pty_index
        );
    }

    // NOTE: get_agents, get_agent, send_input, resize_pty, agent_count
    // all use DEFAULT IMPLEMENTATIONS from the trait - not implemented here
}

/// Background task that forwards PTY output to TuiRunner via channel.
///
/// Mirrors `spawn_pty_output_forwarder` from browser.rs. Subscribes to PTY
/// events and sends `TuiOutput` messages through the channel.
/// Exits when the PTY closes or channel receiver is dropped.
async fn spawn_tui_output_forwarder(
    mut pty_rx: broadcast::Receiver<PtyEvent>,
    sink: UnboundedSender<TuiOutput>,
) {
    log::debug!("Started TUI output forwarder task");

    loop {
        match pty_rx.recv().await {
            Ok(PtyEvent::Output(data)) => {
                if sink.send(TuiOutput::Output(data)).is_err() {
                    // Receiver dropped, stop forwarding.
                    log::debug!("TUI output sink closed, stopping forwarder");
                    break;
                }
            }
            Ok(PtyEvent::ProcessExited { exit_code }) => {
                log::info!("TUI PTY process exited (code={:?})", exit_code);
                // Send exit notification, then continue - may have final output.
                let _ = sink.send(TuiOutput::ProcessExited { exit_code });
            }
            Ok(_other_event) => {
                // Ignore other events (Resized, OwnerChanged).
                // TuiRunner handles these through other mechanisms.
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                log::warn!("TUI output forwarder lagged by {} events", n);
                // Continue - we'll catch up with future events.
            }
            Err(broadcast::error::RecvError::Closed) => {
                log::debug!("PTY channel closed, stopping TUI forwarder");
                break;
            }
        }
    }

    log::debug!("Stopped TUI output forwarder task");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Helper to create a TuiClient with a mock HubHandle for testing.
    /// Returns both the client and the receiver for TuiOutput messages.
    fn test_client() -> (TuiClient, mpsc::UnboundedReceiver<TuiOutput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = TuiClient::new(HubHandle::mock(), tx);
        (client, rx)
    }

    /// Helper to create a TuiClient with specific dimensions.
    /// Returns both the client and the receiver for TuiOutput messages.
    fn test_client_with_dims(cols: u16, rows: u16) -> (TuiClient, mpsc::UnboundedReceiver<TuiOutput>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = TuiClient::with_dims(HubHandle::mock(), tx, cols, rows);
        (client, rx)
    }

    #[test]
    fn test_construction_default() {
        let (client, _rx) = test_client();
        assert_eq!(client.id(), &ClientId::Tui);
        assert_eq!(client.dims(), (80, 24));
    }

    #[test]
    fn test_construction_with_dims() {
        let (client, _rx) = test_client_with_dims(120, 40);
        assert_eq!(client.dims(), (120, 40));
    }

    #[test]
    fn test_update_dims() {
        let (mut client, _rx) = test_client();
        client.update_dims(100, 50);
        assert_eq!(client.dims(), (100, 50));
    }

    #[test]
    fn test_client_id_is_tui() {
        let (client, _rx) = test_client();
        assert!(client.id().is_tui());
    }

    #[test]
    fn test_hub_handle_accessible() {
        let (client, _rx) = test_client();
        // Should be able to access hub_handle
        let _handle = client.hub_handle();
    }

    #[test]
    fn test_debug_format() {
        let (client, _rx) = test_client();
        let debug_str = format!("{:?}", client);
        assert!(debug_str.contains("TuiClient"));
        assert!(debug_str.contains("id"));
        assert!(debug_str.contains("dims"));
    }

    #[test]
    fn test_connected_pty_initially_none() {
        let (client, _rx) = test_client();
        assert!(client.connected_pty.is_none());
    }

    #[test]
    fn test_output_task_initially_none() {
        let (client, _rx) = test_client();
        assert!(client.output_task.is_none());
    }

    #[test]
    fn test_tui_output_debug() {
        // Verify TuiOutput variants can be debugged
        let scrollback = TuiOutput::Scrollback(vec![1, 2, 3]);
        let output = TuiOutput::Output(vec![4, 5, 6]);
        let exited = TuiOutput::ProcessExited { exit_code: Some(0) };

        assert!(format!("{:?}", scrollback).contains("Scrollback"));
        assert!(format!("{:?}", output).contains("Output"));
        assert!(format!("{:?}", exited).contains("ProcessExited"));
    }
}
