//! Channel abstraction for ActionCable communication.
//!
//! This module provides a unified `Channel` trait for WebSocket communication
//! via ActionCable, with optional Signal Protocol encryption and gzip compression.
//!
//! # Architecture
//!
//! ```text
//! Channel (trait)
//!     │
//!     ├── ActionCableChannel (encrypted)
//!     │   └── Uses SignalProtocolManager for E2E encryption
//!     │
//!     └── ActionCableChannel (unencrypted)
//!         └── Raw message relay
//! ```
//!
//! # Usage
//!
//! ```ignore
//! // Encrypted channel (terminal, preview)
//! let channel = ActionCableChannel::encrypted(signal_manager.clone());
//! channel.connect(ChannelConfig {
//!     channel_name: "TerminalRelayChannel".into(),
//!     hub_id: "hub-123".into(),
//!     agent_index: None,
//!     pty_index: Some(0), // 0=CLI, 1=Server
//!     encrypt: true,
//!     compression_threshold: Some(4096),
//! }).await?;
//!
//! // Send to all peers
//! channel.send(b"hello").await?;
//!
//! // Send to specific peer
//! channel.send_to(b"hello", &peer_id).await?;
//!
//! // Receive
//! let msg = channel.recv().await?;
//! ```
//!
//! # Compression
//!
//! When `compression_threshold` is set, payloads exceeding that size are
//! gzip-compressed. A marker byte prefix indicates compression:
//! - `0x00` - uncompressed
//! - `0x1f` - gzip compressed
//!
//! Rust guideline compliant 2025-01

pub mod action_cable;
pub mod compression;
pub mod reliable;
pub mod webrtc;

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Configuration for establishing a channel connection.
#[derive(Clone, Debug)]
pub struct ChannelConfig {
    /// ActionCable channel name (e.g., "TerminalRelayChannel", "PreviewChannel").
    pub channel_name: String,
    /// Hub identifier for routing.
    pub hub_id: String,
    /// Agent index within the hub (for agent-scoped channels like Preview).
    pub agent_index: Option<usize>,
    /// PTY index within the agent (0=CLI, 1=Server).
    pub pty_index: Option<usize>,
    /// Browser identity for browser-specific streams (HubChannel only).
    /// When set, subscribes to `hub:{hub_id}:browser:{identity}` instead of CLI stream.
    pub browser_identity: Option<String>,
    /// Whether to encrypt messages using Signal Protocol.
    pub encrypt: bool,
    /// Compression threshold in bytes. None disables compression.
    /// Payloads exceeding this size are gzip-compressed.
    pub compression_threshold: Option<usize>,
    /// Whether this is a CLI subscription (HubChannel per-browser streams).
    /// When true with browser_identity, subscribes to `hub:{id}:browser:{identity}:cli`
    /// instead of the browser stream `hub:{id}:browser:{identity}`.
    pub cli_subscription: bool,
}

/// Connection state for a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// Not connected.
    Disconnected,
    /// Attempting to connect.
    Connecting,
    /// Connected and ready.
    Connected,
    /// Reconnecting after disconnect.
    Reconnecting {
        /// Current reconnection attempt number.
        attempt: u32,
        /// Milliseconds until next retry.
        next_retry_ms: u64,
    },
    /// Permanent error state.
    Error(String),
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self::Disconnected
    }
}

/// A message received from a channel.
#[derive(Debug)]
pub struct IncomingMessage {
    /// Decrypted and decompressed payload.
    pub payload: Vec<u8>,
    /// Sender's peer identity (browser's Signal identity key).
    pub sender: PeerId,
}

/// Peer identifier (browser's Signal identity key).
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct PeerId(pub String);

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Truncate for display
        if self.0.len() > 16 {
            write!(f, "{}...", &self.0[..16])
        } else {
            write!(f, "{}", self.0)
        }
    }
}

impl From<String> for PeerId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for PeerId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for PeerId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Errors that can occur during channel operations.
#[derive(Debug)]
pub enum ChannelError {
    /// Failed to establish connection.
    ConnectionFailed(String),
    /// Failed to send message.
    SendFailed(String),
    /// Encryption operation failed.
    EncryptionError(String),
    /// Decryption operation failed.
    DecryptionError(String),
    /// Compression operation failed.
    CompressionError(String),
    /// Channel was closed.
    Closed,
    /// No session exists for the peer.
    NoSession(PeerId),
    /// Operation timed out.
    Timeout,
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionFailed(msg) => write!(f, "Connection failed: {msg}"),
            Self::SendFailed(msg) => write!(f, "Send failed: {msg}"),
            Self::EncryptionError(msg) => write!(f, "Encryption error: {msg}"),
            Self::DecryptionError(msg) => write!(f, "Decryption error: {msg}"),
            Self::CompressionError(msg) => write!(f, "Compression error: {msg}"),
            Self::Closed => write!(f, "Channel closed"),
            Self::NoSession(peer) => write!(f, "No session for peer: {peer}"),
            Self::Timeout => write!(f, "Operation timed out"),
        }
    }
}

impl std::error::Error for ChannelError {}

/// A bidirectional communication channel with optional encryption and compression.
///
/// Implementors handle the underlying transport (WebSocket), optional Signal
/// Protocol encryption, and optional gzip compression transparently.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Connect to the ActionCable channel.
    ///
    /// # Errors
    ///
    /// Returns `ChannelError::ConnectionFailed` if the WebSocket connection
    /// or ActionCable subscription fails.
    async fn connect(&mut self, config: ChannelConfig) -> Result<(), ChannelError>;

    /// Disconnect from the channel.
    ///
    /// Closes the WebSocket connection and cleans up resources.
    async fn disconnect(&mut self);

    /// Get the current connection state.
    fn state(&self) -> ConnectionState;

    /// Send a message to all connected peers.
    ///
    /// For encrypted channels, the message is encrypted separately for each peer.
    /// Compression is applied if the payload exceeds the configured threshold.
    ///
    /// # Errors
    ///
    /// Returns `ChannelError::SendFailed` if the send fails, or
    /// `ChannelError::EncryptionError` if encryption fails.
    async fn send(&self, msg: &[u8]) -> Result<(), ChannelError>;

    /// Send a message to a specific peer.
    ///
    /// # Errors
    ///
    /// Returns `ChannelError::NoSession` if no session exists for the peer,
    /// `ChannelError::SendFailed` if the send fails, or
    /// `ChannelError::EncryptionError` if encryption fails.
    async fn send_to(&self, msg: &[u8], peer: &PeerId) -> Result<(), ChannelError>;

    /// Receive the next message from the channel.
    ///
    /// This method blocks until a message is available or the channel is closed.
    /// Messages are automatically decrypted and decompressed.
    ///
    /// # Errors
    ///
    /// Returns `ChannelError::Closed` if the channel is closed,
    /// `ChannelError::DecryptionError` if decryption fails, or
    /// `ChannelError::CompressionError` if decompression fails.
    async fn recv(&mut self) -> Result<IncomingMessage, ChannelError>;

    /// Get the list of connected peers.
    ///
    /// For encrypted channels, this returns peers with active Signal sessions.
    fn peers(&self) -> Vec<PeerId>;

    /// Check if a peer has an active session.
    fn has_peer(&self, peer: &PeerId) -> bool;
}

/// Shared connection state that can be observed from outside the channel.
#[derive(Debug, Default)]
pub struct SharedConnectionState {
    state: RwLock<ConnectionState>,
}

impl SharedConnectionState {
    /// Create new shared state.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Get the current state.
    pub async fn get(&self) -> ConnectionState {
        self.state.read().await.clone()
    }

    /// Set the state.
    pub async fn set(&self, new_state: ConnectionState) {
        *self.state.write().await = new_state;
    }

    /// Check if connected.
    pub async fn is_connected(&self) -> bool {
        matches!(*self.state.read().await, ConnectionState::Connected)
    }
}

// Re-exports
pub use action_cable::{
    ActionCableChannel, ActionCableChannelBuilder, ChannelReceiverHandle, ChannelSenderHandle,
};
pub use compression::{maybe_compress, maybe_decompress, should_compress_response};
pub use reliable::{ReliableMessage, ReliableReceiver, ReliableSender, ReliableSession};
pub use webrtc::{WebRtcChannel, WebRtcChannelBuilder, WebRtcConfig};
