//! PTY spawning utilities.
//!
//! This module provides common functionality for spawning PTY processes,
//! extracting shared patterns from CLI and Server PTY creation.
//!
//! # Event-Driven Architecture
//!
//! Reader threads broadcast [`PtyEvent::Output`] via a broadcast channel.
//! This enables decoupled pub/sub where:
//! - TUI client feeds output to its local terminal parser
//! - Browser client encrypts and sends via ActionCable channel
//!
//! Each client subscribes to events independently and handles them
//! according to their transport requirements.

// Rust guideline compliant 2026-02

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};

/// Configuration for spawning a process in a PtySession.
///
/// This struct captures all the parameters needed to spawn a process,
/// unifying the CLI and server spawn paths into a single configurable
/// entry point via [`PtySession::spawn()`](super::pty::PtySession::spawn).
///
/// # Example
///
/// ```ignore
/// let config = PtySpawnConfig {
///     worktree_path: PathBuf::from("/path/to/worktree"),
///     command: "bash".to_string(),
///     env: HashMap::new(),
///     init_commands: vec!["source .botster/shared/sessions/agent/initialization".to_string()],
///     detect_notifications: true,
///     port: None,
///     context: String::new(),
/// };
/// pty_session.spawn(config)?;
/// ```
#[derive(Debug)]
pub struct PtySpawnConfig {
    /// Working directory for the process.
    pub worktree_path: PathBuf,
    /// Command to run (e.g., "bash").
    pub command: String,
    /// Environment variables to set.
    pub env: HashMap<String, String>,
    /// Commands to run after spawn (e.g., sourcing a session initialization script).
    pub init_commands: Vec<String>,
    /// Enable OSC notification detection on this session.
    ///
    /// When true, the reader thread will parse PTY output for OSC 9 and
    /// OSC 777 notification sequences and broadcast them as
    /// [`PtyEvent::Notification`](super::pty::PtyEvent::Notification) events.
    pub detect_notifications: bool,
    /// HTTP forwarding port (if this session runs a server).
    ///
    /// When set, stored on the PtySession via `set_port()` for browser
    /// clients to query when proxying preview requests.
    pub port: Option<u16>,
    /// Context string written to PTY before init commands.
    ///
    /// Typically used to send initial context to the agent process
    /// before running init scripts.
    pub context: String,
}

/// Open a new PTY pair with the given dimensions.
pub fn open_pty(rows: u16, cols: u16) -> Result<PtyPair> {
    let pty_system = native_pty_system();
    let size = PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    };
    pty_system.openpty(size).context("Failed to open PTY")
}

/// Build a command from a command string.
#[allow(
    clippy::implicit_hasher,
    reason = "internal API doesn't need hasher generalization"
)]
pub fn build_command(
    command_str: &str,
    cwd: &std::path::Path,
    env_vars: &std::collections::HashMap<String, String>,
) -> CommandBuilder {
    let parts: Vec<&str> = command_str.split_whitespace().collect();
    let mut cmd = CommandBuilder::new(parts[0]);
    for arg in &parts[1..] {
        cmd.arg(arg);
    }
    cmd.cwd(cwd);
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    // Prepend the running binary's directory to PATH so agent PTYs always
    // resolve `botster` to the same build that's running the hub — no need
    // to install globally or manage PATH expectations.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            let base_path = env_vars
                .get("PATH")
                .cloned()
                .or_else(|| std::env::var("PATH").ok())
                .unwrap_or_default();
            cmd.env("PATH", format!("{}:{}", bin_dir.display(), base_path));
        }
    }

    cmd
}

// process_pty_bytes and spawn_reader_thread removed — session process owns
// terminal parsing. All output processing happens in the session process;
// the hub is a router that broadcasts PtyEvent::Output from the session reader.

/// Scan PTY output for OSC 7 current working directory sequences.
///
/// Returns the last CWD path found in the buffer, or `None`.
/// Format: `ESC ] 7 ; file://hostname/path BEL`
///
/// Extracts just the path component, percent-decoded.
pub fn scan_cwd(data: &[u8]) -> Option<String> {
    let mut last_cwd = None;
    let mut i = 0;

    while i + 1 < data.len() {
        if data[i] == 0x1b && data[i + 1] == b']' {
            let osc_start = i + 2;
            // Check for OSC 7; prefix
            let is_cwd = osc_start + 2 <= data.len()
                && data[osc_start] == b'7'
                && osc_start + 1 < data.len()
                && data[osc_start + 1] == b';';

            if is_cwd {
                let uri_start = osc_start + 2;
                let mut end = None;
                for j in uri_start..data.len() {
                    if data[j] == 0x07 {
                        end = Some((j, j + 1));
                        break;
                    } else if j + 1 < data.len() && data[j] == 0x1b && data[j + 1] == b'\\' {
                        end = Some((j, j + 2));
                        break;
                    }
                }
                if let Some((content_end, skip_to)) = end {
                    let uri = String::from_utf8_lossy(&data[uri_start..content_end]);
                    // Extract path from file://hostname/path URI
                    let path = if let Some(rest) = uri.strip_prefix("file://") {
                        // Skip hostname (everything up to the next /)
                        if let Some(slash_pos) = rest.find('/') {
                            percent_decode(&rest[slash_pos..])
                        } else {
                            percent_decode(rest)
                        }
                    } else {
                        // Not a proper URI, use as-is
                        uri.to_string()
                    };
                    if !path.is_empty() {
                        last_cwd = Some(path);
                    }
                    i = skip_to;
                    continue;
                }
            }
        }
        i += 1;
    }

    last_cwd
}

/// Simple percent-decoding for file URIs.
///
/// Decodes `%XX` sequences to their byte values. Used for OSC 7 paths
/// which may contain encoded spaces and special characters.
fn percent_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                result.push(byte as char);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

/// Scan PTY output for OSC 133/633 shell integration markers.
///
/// Returns semantic prompt actions found in the buffer.
///
/// This scanner normalizes raw OSC markers into the same action model used by
/// Ghostty's semantic prompt callback. Extra VS Code payloads like command text
/// or exit codes are ignored.
///
/// Supports both OSC 133 (FinalTerm/iTerm2) and OSC 633 (VS Code).
pub fn scan_prompt_marks(data: &[u8]) -> Vec<super::pty::PromptMark> {
    use super::pty::PromptMark;
    let mut marks = Vec::new();
    let mut i = 0;

    while i + 1 < data.len() {
        if data[i] == 0x1b && data[i + 1] == b']' {
            let osc_start = i + 2;

            // Check for OSC 133; or OSC 633; prefix
            let (prefix_len, is_vscode) = if osc_start + 4 <= data.len()
                && &data[osc_start..osc_start + 4] == b"133;"
            {
                (4, false)
            } else if osc_start + 4 <= data.len() && &data[osc_start..osc_start + 4] == b"633;" {
                (4, true)
            } else {
                i += 1;
                continue;
            };

            let mark_start = osc_start + prefix_len;

            // Find terminator
            let mut end = None;
            for j in mark_start..data.len() {
                if data[j] == 0x07 {
                    end = Some((j, j + 1));
                    break;
                } else if j + 1 < data.len() && data[j] == 0x1b && data[j + 1] == b'\\' {
                    end = Some((j, j + 2));
                    break;
                }
            }

            if let Some((content_end, skip_to)) = end {
                let content = &data[mark_start..content_end];

                let mark = if !content.is_empty() {
                    match content[0] {
                        b'A' => Some(PromptMark::FreshLineNewPrompt),
                        b'B' => Some(PromptMark::EndPromptStartInput),
                        b'C' => Some(PromptMark::EndInputStartOutput),
                        b'D' => Some(PromptMark::EndCommand),
                        b'P' => Some(PromptMark::PromptStart),
                        b'I' => Some(PromptMark::EndPromptStartInput),
                        b'L' => Some(PromptMark::FreshLine),
                        b'N' => Some(PromptMark::NewCommand),
                        b'E' if is_vscode && content.len() > 2 && content[1] == b';' => None,
                        _ => None,
                    }
                } else {
                    None
                };

                if let Some(m) = mark {
                    marks.push(m);
                }
                i = skip_to;
                continue;
            }
        }
        i += 1;
    }

    marks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_pty() {
        // This test may fail in CI environments without PTY support
        let result = open_pty(24, 80);
        // Just check it doesn't panic - actual success depends on environment
        let _ = result;
    }

    #[test]
    fn test_build_command() {
        use std::collections::HashMap;
        use std::path::PathBuf;

        let env = HashMap::new();
        let cwd = PathBuf::from("/tmp");
        let cmd = build_command("echo hello world", &cwd, &env);

        // CommandBuilder doesn't expose its internals, so we just verify it was created
        let _ = cmd;
    }

    // Reader thread tests removed — process_pty_bytes and spawn_reader_thread
    // were removed when terminal parsing moved to the session process.
    // The session process owns all VT parsing; the hub is a router.

    // =========================================================================
    // CWD Scanner Tests (OSC 7)
    // =========================================================================

    #[test]
    fn test_scan_cwd_basic() {
        let data = b"\x1b]7;file://localhost/home/user/projects\x07";
        assert_eq!(scan_cwd(data), Some("/home/user/projects".to_string()));
    }

    #[test]
    fn test_scan_cwd_st_terminator() {
        let data = b"\x1b]7;file://host/tmp/dir\x1b\\";
        assert_eq!(scan_cwd(data), Some("/tmp/dir".to_string()));
    }

    #[test]
    fn test_scan_cwd_percent_encoded() {
        let data = b"\x1b]7;file://localhost/home/user/my%20project\x07";
        assert_eq!(scan_cwd(data), Some("/home/user/my project".to_string()));
    }

    #[test]
    fn test_scan_cwd_empty_hostname() {
        // Some shells emit file:///path (empty hostname)
        let data = b"\x1b]7;file:///Users/jason/code\x07";
        assert_eq!(scan_cwd(data), Some("/Users/jason/code".to_string()));
    }

    #[test]
    fn test_scan_cwd_none() {
        assert_eq!(scan_cwd(b"plain text"), None);
        assert_eq!(scan_cwd(b"\x1b]0;title\x07"), None);
    }

    // =========================================================================
    // Prompt Mark Scanner Tests (OSC 133/633)
    // =========================================================================

    #[test]
    fn test_scan_prompt_marks_133_a() {
        use super::super::pty::PromptMark;
        let data = b"\x1b]133;A\x07";
        let marks = scan_prompt_marks(data);
        assert_eq!(marks, vec![PromptMark::FreshLineNewPrompt]);
    }

    #[test]
    fn test_scan_prompt_marks_633_a() {
        use super::super::pty::PromptMark;
        let data = b"\x1b]633;A\x07";
        let marks = scan_prompt_marks(data);
        assert_eq!(marks, vec![PromptMark::FreshLineNewPrompt]);
    }

    #[test]
    fn test_scan_prompt_marks_all_types() {
        use super::super::pty::PromptMark;
        let mut data = Vec::new();
        data.extend(b"\x1b]133;A\x07");
        data.extend(b"\x1b]133;B\x07");
        data.extend(b"\x1b]133;C\x07");
        data.extend(b"\x1b]133;D;0\x07");
        let marks = scan_prompt_marks(&data);
        assert_eq!(
            marks,
            vec![
                PromptMark::FreshLineNewPrompt,
                PromptMark::EndPromptStartInput,
                PromptMark::EndInputStartOutput,
                PromptMark::EndCommand,
            ]
        );
    }

    #[test]
    fn test_scan_prompt_marks_633_e_command_text() {
        let data = b"\x1b]633;E;ls -la\x07";
        let marks = scan_prompt_marks(data);
        assert!(marks.is_empty());
    }

    #[test]
    fn test_scan_prompt_marks_exit_code() {
        use super::super::pty::PromptMark;
        let data = b"\x1b]133;D;1\x07";
        let marks = scan_prompt_marks(data);
        assert_eq!(marks, vec![PromptMark::EndCommand]);
    }

    #[test]
    fn test_scan_prompt_marks_no_exit_code() {
        use super::super::pty::PromptMark;
        let data = b"\x1b]133;D\x07";
        let marks = scan_prompt_marks(data);
        assert_eq!(marks, vec![PromptMark::EndCommand]);
    }

    #[test]
    fn test_scan_prompt_marks_none() {
        assert!(scan_prompt_marks(b"plain text").is_empty());
        assert!(scan_prompt_marks(b"\x1b]0;title\x07").is_empty());
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("/path/to/file"), "/path/to/file");
        assert_eq!(percent_decode("/path%20with%20spaces"), "/path with spaces");
        assert_eq!(percent_decode("%2Froot"), "/root");
        assert_eq!(percent_decode("no%encoding"), "no%encoding"); // invalid hex
    }
}
