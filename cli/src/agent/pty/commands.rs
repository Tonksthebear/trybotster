//! PTY command protocol.
//!
//! Commands sent from clients to PTY sessions via channels.

// Rust guideline compliant 2026-01

use tokio::sync::oneshot;

use crate::client::ClientId;

/// Command sent to a PTY session.
#[derive(Debug)]
pub enum PtyCommand {
    /// Send input data to the PTY.
    Input(Vec<u8>),

    /// Resize the PTY.
    Resize {
        /// Client requesting the resize.
        client_id: ClientId,
        /// New height in rows.
        rows: u16,
        /// New width in columns.
        cols: u16,
    },

    /// Client connected to this PTY.
    Connect {
        /// Client identifier.
        client_id: ClientId,
        /// Terminal dimensions (cols, rows).
        dims: (u16, u16),
        /// Response channel for scrollback data.
        response_tx: oneshot::Sender<Vec<u8>>,
    },

    /// Client disconnected from this PTY.
    Disconnect {
        /// Client identifier.
        client_id: ClientId,
    },
}
