//! PTY session events for pub/sub broadcasting.
//!
//! This module defines events that PTY sessions broadcast to connected clients.
//! Events are sent via `tokio::sync::broadcast` channels, enabling true pub/sub
//! where the PTY emits without knowing about subscribers.
//!
//! # Event Types
//!
//! - [`PtyEvent::Output`] - Raw terminal output bytes
//! - [`PtyEvent::Resized`] - PTY dimensions changed
//! - [`PtyEvent::ProcessExited`] - Process in PTY terminated
//!
//! # Usage
//!
//! ```ignore
//! // PTY broadcasts
//! pty.emit(PtyEvent::Output(data));
//!
//! // Client subscribes and receives
//! let rx = pty.subscribe();
//! match rx.recv().await {
//!     Ok(PtyEvent::Output(data)) => client.on_output(&data),
//!     // ...
//! }
//! ```

// Rust guideline compliant 2026-02

/// Events broadcast by PTY sessions to connected clients.
///
/// These events enable decoupled communication between PTY sessions and clients.
/// PTY sessions emit events without knowing who is subscribed. Each client
/// receives events independently via their own broadcast receiver.
#[derive(Debug, Clone)]
pub enum PtyEvent {
    /// Raw output bytes from the PTY.
    ///
    /// Clients handle this according to their transport:
    /// - TUI: Feed to local vt100 parser
    /// - Browser: Encrypt and send via channel
    Output(Vec<u8>),

    /// PTY was resized to new dimensions.
    ///
    /// Broadcast when a resize is applied to the PTY.
    /// Clients may use this to sync their terminal display.
    Resized {
        /// New height in rows.
        rows: u16,
        /// New width in columns.
        cols: u16,
    },

    /// Process running in the PTY exited.
    ///
    /// The PTY session remains valid but has no running process.
    /// Clients should indicate the session ended.
    ProcessExited {
        /// Exit code if available (None if killed by signal).
        exit_code: Option<i32>,
    },
}

impl PtyEvent {
    /// Create an output event from bytes.
    #[must_use]
    pub fn output(data: impl Into<Vec<u8>>) -> Self {
        Self::Output(data.into())
    }

    /// Create a resized event.
    #[must_use]
    pub fn resized(rows: u16, cols: u16) -> Self {
        Self::Resized { rows, cols }
    }

    /// Create a process exited event.
    #[must_use]
    pub fn process_exited(exit_code: Option<i32>) -> Self {
        Self::ProcessExited { exit_code }
    }

    /// Check if this is an output event.
    #[must_use]
    pub fn is_output(&self) -> bool {
        matches!(self, Self::Output(_))
    }

    /// Check if this is a resize event.
    #[must_use]
    pub fn is_resized(&self) -> bool {
        matches!(self, Self::Resized { .. })
    }

    /// Check if this is a process exit event.
    #[must_use]
    pub fn is_process_exited(&self) -> bool {
        matches!(self, Self::ProcessExited { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pty_event_output_creation() {
        let event = PtyEvent::output(b"hello".to_vec());
        assert!(event.is_output());
        match event {
            PtyEvent::Output(data) => assert_eq!(data, b"hello"),
            _ => panic!("Expected Output variant"),
        }
    }

    #[test]
    fn test_pty_event_output_from_slice() {
        let event = PtyEvent::output(b"test".as_slice().to_vec());
        assert!(event.is_output());
    }

    #[test]
    fn test_pty_event_resized_creation() {
        let event = PtyEvent::resized(24, 80);
        assert!(event.is_resized());
        match event {
            PtyEvent::Resized { rows, cols } => {
                assert_eq!(rows, 24);
                assert_eq!(cols, 80);
            }
            _ => panic!("Expected Resized variant"),
        }
    }

    #[test]
    fn test_pty_event_process_exited_with_code() {
        let event = PtyEvent::process_exited(Some(0));
        assert!(event.is_process_exited());
        match event {
            PtyEvent::ProcessExited { exit_code } => {
                assert_eq!(exit_code, Some(0));
            }
            _ => panic!("Expected ProcessExited variant"),
        }
    }

    #[test]
    fn test_pty_event_process_exited_without_code() {
        let event = PtyEvent::process_exited(None);
        assert!(event.is_process_exited());
        match event {
            PtyEvent::ProcessExited { exit_code } => {
                assert!(exit_code.is_none());
            }
            _ => panic!("Expected ProcessExited variant"),
        }
    }

    #[test]
    fn test_pty_event_is_predicates_are_exclusive() {
        let output = PtyEvent::output(vec![]);
        assert!(output.is_output());
        assert!(!output.is_resized());
        assert!(!output.is_process_exited());

        let resized = PtyEvent::resized(24, 80);
        assert!(!resized.is_output());
        assert!(resized.is_resized());
        assert!(!resized.is_process_exited());
    }

    #[test]
    fn test_pty_event_clone() {
        let event = PtyEvent::output(b"test".to_vec());
        let cloned = event.clone();
        match cloned {
            PtyEvent::Output(data) => assert_eq!(data, b"test"),
            _ => panic!("Clone failed"),
        }
    }

    #[test]
    fn test_pty_event_debug() {
        let event = PtyEvent::resized(24, 80);
        let debug = format!("{:?}", event);
        assert!(debug.contains("Resized"));
        assert!(debug.contains("24"));
        assert!(debug.contains("80"));
    }
}
