//! Terminal notification detection for agent PTY output.
//!
//! This module handles parsing of OSC (Operating System Command) escape sequences
//! that terminals use for notifications. Agents can use these to signal events
//! like task completion.
//!
//! # Supported Notification Types
//!
//! - **OSC 9**: Simple notification with message (`ESC ] 9 ; message BEL`)
//! - **OSC 777**: Rich notification with title and body (`ESC ] 777 ; notify ; title ; body BEL`)
//!
//! # Example
//!
//! ```
//! use botster_hub::agent::notification::{detect_notifications, AgentNotification};
//!
//! let data = b"\x1b]9;Build complete\x07";
//! let notifications = detect_notifications(data);
//! assert_eq!(notifications.len(), 1);
//! ```

// Rust guideline compliant 2025-01

/// Notification types detected from PTY output.
///
/// Terminal applications can send notifications via OSC escape sequences.
/// These are parsed from the raw PTY output stream.
#[derive(Clone, Debug)]
pub enum AgentNotification {
    /// OSC 9 notification with optional message.
    ///
    /// Format: `ESC ] 9 ; message BEL` or `ESC ] 9 ; message ESC \`
    Osc9(Option<String>),

    /// OSC 777 notification (rxvt-unicode style) with title and body.
    ///
    /// Format: `ESC ] 777 ; notify ; title ; body BEL`
    Osc777 {
        /// Notification title.
        title: String,
        /// Notification body text.
        body: String,
    },
}

/// Agent execution status.
///
/// Tracks the lifecycle state of an agent from initialization through
/// completion or failure.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AgentStatus {
    /// Agent is starting up.
    Initializing,
    /// Agent is actively running.
    Running,
    /// Agent completed successfully.
    Finished,
    /// Agent failed with an error message.
    Failed(String),
    /// Agent was manually terminated.
    Killed,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Initializing => write!(f, "initializing"),
            AgentStatus::Running => write!(f, "running"),
            AgentStatus::Finished => write!(f, "finished"),
            AgentStatus::Failed(e) => write!(f, "failed: {}", e),
            AgentStatus::Killed => write!(f, "killed"),
        }
    }
}

/// Detect terminal notifications in raw PTY output.
///
/// Parses OSC (Operating System Command) escape sequences from the byte stream.
/// Supports both BEL (0x07) and ST (ESC \) terminators.
///
/// # Arguments
///
/// * `data` - Raw bytes from PTY output
///
/// # Returns
///
/// Vector of detected notifications. Empty if no notifications found.
///
/// # Filtering
///
/// OSC 9 messages that look like escape sequences (only digits and semicolons)
/// are filtered out to avoid false positives.
pub fn detect_notifications(data: &[u8]) -> Vec<AgentNotification> {
    let mut notifications = Vec::new();

    // Parse OSC sequences (ESC ] ... BEL or ESC ] ... ESC \)
    let mut i = 0;
    while i < data.len() {
        // Check for OSC sequence start: ESC ]
        if i + 1 < data.len() && data[i] == 0x1b && data[i + 1] == b']' {
            // Find the end of the OSC sequence (BEL or ST)
            let osc_start = i + 2;
            let mut osc_end = None;

            for j in osc_start..data.len() {
                if data[j] == 0x07 {
                    // Ends with BEL
                    osc_end = Some(j);
                    break;
                } else if j + 1 < data.len() && data[j] == 0x1b && data[j + 1] == b'\\' {
                    // Ends with ST (ESC \)
                    osc_end = Some(j);
                    break;
                }
            }

            if let Some(end) = osc_end {
                let osc_content = &data[osc_start..end];

                // Parse OSC 9: notification
                // Filter out messages that look like escape sequences (only digits/semicolons)
                if osc_content.starts_with(b"9;") {
                    let message = String::from_utf8_lossy(&osc_content[2..]).to_string();
                    // Only add if message is meaningful (not just numbers/semicolons)
                    let is_escape_sequence =
                        message.chars().all(|c| c.is_ascii_digit() || c == ';');
                    if !message.is_empty() && !is_escape_sequence {
                        notifications.push(AgentNotification::Osc9(Some(message)));
                    }
                }
                // Parse OSC 777: notify;title;body
                else if osc_content.starts_with(b"777;notify;") {
                    let content = String::from_utf8_lossy(&osc_content[11..]).to_string();
                    let parts: Vec<&str> = content.splitn(2, ';').collect();
                    let title = parts.first().unwrap_or(&"").to_string();
                    let body = parts.get(1).unwrap_or(&"").to_string();
                    // Only add if there's meaningful content
                    if !title.is_empty() || !body.is_empty() {
                        notifications.push(AgentNotification::Osc777 { title, body });
                    }
                }

                // Skip past the OSC sequence
                i = end + 1;
                continue;
            }
        }

        i += 1;
    }

    notifications
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_standalone_bell_ignored() {
        // Standalone BEL character is ignored (not useful for agent notifications)
        let data = b"some output\x07more output";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 0, "Standalone BEL should be ignored");
    }

    #[test]
    fn test_detect_osc9_with_bel_terminator() {
        // OSC 9 with BEL terminator: ESC ] 9 ; message BEL
        let data = b"\x1b]9;Test notification\x07";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 1);
        match &notifications[0] {
            AgentNotification::Osc9(Some(msg)) => assert_eq!(msg, "Test notification"),
            _ => panic!("Expected Osc9 notification"),
        }
    }

    #[test]
    fn test_detect_osc9_with_st_terminator() {
        // OSC 9 with ST terminator: ESC ] 9 ; message ESC \
        // Example: \033]9;message\033\\
        let data = b"\x1b]9;Agent notification\x1b\\";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 1);
        match &notifications[0] {
            AgentNotification::Osc9(Some(msg)) => assert_eq!(msg, "Agent notification"),
            _ => panic!("Expected Osc9 notification with ST terminator"),
        }
    }

    #[test]
    fn test_detect_osc777_notification() {
        // OSC 777: ESC ] 777 ; notify ; title ; body BEL
        let data = b"\x1b]777;notify;Build Complete;All tests passed\x07";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 1);
        match &notifications[0] {
            AgentNotification::Osc777 { title, body } => {
                assert_eq!(title, "Build Complete");
                assert_eq!(body, "All tests passed");
            }
            _ => panic!("Expected Osc777 notification"),
        }
    }

    #[test]
    fn test_no_false_positive_bel_in_osc() {
        // BEL inside OSC should not trigger standalone Bell notification
        let data = b"\x1b]9;message\x07";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 1);
        // Should be Osc9, not Bell
        assert!(matches!(notifications[0], AgentNotification::Osc9(_)));
    }

    #[test]
    fn test_osc9_filters_escape_sequence_messages() {
        // OSC 9 with escape-sequence-like content (just numbers/semicolons) should be filtered
        let data = b"\x1b]9;4;0;\x07";
        let notifications = detect_notifications(data);
        assert_eq!(
            notifications.len(),
            0,
            "Should filter escape-sequence-like messages"
        );

        // But real messages should still work
        let data = b"\x1b]9;Real notification message\x07";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 1);
        match &notifications[0] {
            AgentNotification::Osc9(Some(msg)) => assert_eq!(msg, "Real notification message"),
            _ => panic!("Expected Osc9 notification"),
        }
    }

    #[test]
    fn test_multiple_notifications() {
        // Multiple notifications in one buffer (without Bell since it's disabled)
        let data = b"\x07\x1b]9;first\x07\x07\x1b]9;second\x1b\\";
        let notifications = detect_notifications(data);
        // Should detect: Osc9("first"), Osc9("second") - no standalone Bell
        assert_eq!(notifications.len(), 2);
    }

    #[test]
    fn test_no_notifications_in_regular_output() {
        // Regular output without OSC sequences
        let data = b"Building project...\nCompilation complete.";
        let notifications = detect_notifications(data);
        assert_eq!(notifications.len(), 0);
    }

    #[test]
    fn test_agent_status_display() {
        assert_eq!(format!("{}", AgentStatus::Initializing), "initializing");
        assert_eq!(format!("{}", AgentStatus::Running), "running");
        assert_eq!(format!("{}", AgentStatus::Finished), "finished");
        assert_eq!(
            format!("{}", AgentStatus::Failed("error".to_string())),
            "failed: error"
        );
        assert_eq!(format!("{}", AgentStatus::Killed), "killed");
    }
}
