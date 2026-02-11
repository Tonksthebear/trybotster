//! PTY command protocol.
//!
//! Commands sent from clients to PTY sessions via channels.

// Rust guideline compliant 2026-02

/// Command sent to a PTY session.
#[derive(Debug)]
pub enum PtyCommand {
    /// Send input data to the PTY.
    Input(Vec<u8>),
}
