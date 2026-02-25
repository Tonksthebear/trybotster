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
//! - [`PtyEvent::Notification`] - OSC notification detected (OSC 9, OSC 777)
//! - [`PtyEvent::TitleChanged`] - Window title set via OSC 0/2
//! - [`PtyEvent::CwdChanged`] - Working directory reported via OSC 7
//! - [`PtyEvent::PromptMark`] - Shell integration prompt mark (OSC 133/633)
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

use super::super::notification::AgentNotification;

/// Shell integration prompt marks detected from OSC 133/633 sequences.
///
/// These sequences are emitted by shells with prompt integration (bash, zsh, fish)
/// and VS Code's terminal shell integration. They mark command boundaries in the
/// terminal output stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptMark {
    /// Prompt is about to be displayed (OSC 133;A / 633;A).
    PromptStart,
    /// User has entered a command, prompt ended (OSC 133;B / 633;B).
    CommandStart,
    /// Command has been executed, output begins (OSC 133;C / 633;C).
    /// The optional string carries the command text from OSC 633;E.
    CommandExecuted(Option<String>),
    /// Command finished with an exit code (OSC 133;D / 633;D).
    CommandFinished(Option<i32>),
}

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

    /// OSC notification detected in PTY output.
    ///
    /// Broadcast when the reader thread detects OSC 9 or OSC 777
    /// notification sequences. Subscribers (e.g., notification watcher
    /// tasks) can filter for this event to fire Lua hooks.
    Notification(AgentNotification),

    /// Window title changed via OSC 0 or OSC 2.
    ///
    /// Programs set the terminal tab title with `ESC ] 0 ; title BEL`
    /// (set title + icon) or `ESC ] 2 ; title BEL` (set title only).
    TitleChanged(String),

    /// Current working directory reported via OSC 7.
    ///
    /// Shells report CWD changes with `ESC ] 7 ; file://hostname/path BEL`.
    /// The string contains the decoded path (not the full URI).
    CwdChanged(String),

    /// Shell integration prompt mark detected (OSC 133/633).
    ///
    /// Marks command boundaries in the terminal output stream.
    /// Used for tracking command lifecycle in agent sessions.
    PromptMark(PromptMark),

    /// Kitty keyboard protocol state changed.
    ///
    /// Emitted when the reader thread detects `CSI > flags u` (push, true)
    /// or `CSI < u` (pop, false) in the PTY output stream.
    KittyChanged(bool),

    /// PTY enabled focus reporting via `CSI ? 1004 h`.
    ///
    /// The TUI should respond with the current terminal focus state
    /// (`CSI I` or `CSI O`) so the inner application knows immediately.
    FocusRequested,

    /// Cursor visibility changed via DECTCEM (`CSI ? 25 h` / `CSI ? 25 l`).
    ///
    /// `true` = cursor shown (free-text input expected),
    /// `false` = cursor hidden (generation, selection UI, no input expected).
    /// Emitted only on transitions, not on every occurrence.
    CursorVisibilityChanged(bool),
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

    /// Create a notification event.
    #[must_use]
    pub fn notification(notif: AgentNotification) -> Self {
        Self::Notification(notif)
    }

    /// Create a title changed event.
    #[must_use]
    pub fn title_changed(title: impl Into<String>) -> Self {
        Self::TitleChanged(title.into())
    }

    /// Create a CWD changed event.
    #[must_use]
    pub fn cwd_changed(cwd: impl Into<String>) -> Self {
        Self::CwdChanged(cwd.into())
    }

    /// Create a prompt mark event.
    #[must_use]
    pub fn prompt_mark(mark: PromptMark) -> Self {
        Self::PromptMark(mark)
    }

    /// Create a kitty keyboard protocol state change event.
    #[must_use]
    pub fn kitty_changed(enabled: bool) -> Self {
        Self::KittyChanged(enabled)
    }

    /// Create a focus reporting requested event.
    #[must_use]
    pub fn focus_requested() -> Self {
        Self::FocusRequested
    }

    /// Create a cursor visibility changed event.
    #[must_use]
    pub fn cursor_visibility_changed(visible: bool) -> Self {
        Self::CursorVisibilityChanged(visible)
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

    /// Check if this is a notification event.
    #[must_use]
    pub fn is_notification(&self) -> bool {
        matches!(self, Self::Notification(_))
    }

    /// Check if this is a title changed event.
    #[must_use]
    pub fn is_title_changed(&self) -> bool {
        matches!(self, Self::TitleChanged(_))
    }

    /// Check if this is a CWD changed event.
    #[must_use]
    pub fn is_cwd_changed(&self) -> bool {
        matches!(self, Self::CwdChanged(_))
    }

    /// Check if this is a prompt mark event.
    #[must_use]
    pub fn is_prompt_mark(&self) -> bool {
        matches!(self, Self::PromptMark(_))
    }

    /// Check if this is a cursor visibility changed event.
    #[must_use]
    pub fn is_cursor_visibility_changed(&self) -> bool {
        matches!(self, Self::CursorVisibilityChanged(_))
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
        use crate::agent::notification::AgentNotification;

        let output = PtyEvent::output(vec![]);
        assert!(output.is_output());
        assert!(!output.is_resized());
        assert!(!output.is_process_exited());
        assert!(!output.is_notification());
        assert!(!output.is_title_changed());
        assert!(!output.is_cwd_changed());
        assert!(!output.is_prompt_mark());
        assert!(!output.is_cursor_visibility_changed());

        let resized = PtyEvent::resized(24, 80);
        assert!(!resized.is_output());
        assert!(resized.is_resized());
        assert!(!resized.is_process_exited());
        assert!(!resized.is_notification());
        assert!(!resized.is_cursor_visibility_changed());

        let notification = PtyEvent::notification(AgentNotification::Osc9(Some("test".to_string())));
        assert!(!notification.is_output());
        assert!(!notification.is_resized());
        assert!(!notification.is_process_exited());
        assert!(notification.is_notification());
        assert!(!notification.is_cursor_visibility_changed());

        let title = PtyEvent::title_changed("My Title");
        assert!(title.is_title_changed());
        assert!(!title.is_output());
        assert!(!title.is_notification());
        assert!(!title.is_cursor_visibility_changed());

        let cwd = PtyEvent::cwd_changed("/home/user");
        assert!(cwd.is_cwd_changed());
        assert!(!cwd.is_output());
        assert!(!cwd.is_cursor_visibility_changed());

        let mark = PtyEvent::prompt_mark(PromptMark::PromptStart);
        assert!(mark.is_prompt_mark());
        assert!(!mark.is_output());
        assert!(!mark.is_cursor_visibility_changed());

        let cursor = PtyEvent::cursor_visibility_changed(true);
        assert!(cursor.is_cursor_visibility_changed());
        assert!(!cursor.is_output());
        assert!(!cursor.is_resized());
        assert!(!cursor.is_process_exited());
        assert!(!cursor.is_notification());
        assert!(!cursor.is_title_changed());
        assert!(!cursor.is_cwd_changed());
        assert!(!cursor.is_prompt_mark());
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
