//! PTY spawning utilities.
//!
//! This module provides common functionality for spawning PTY processes,
//! extracting shared patterns from CLI and Server PTY creation.

// Rust guideline compliant 2025-01

use std::collections::VecDeque;
use std::io::Read;
use std::sync::{mpsc::Sender, Arc, Mutex};
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtyPair, PtySize};
use vt100::Parser;

use super::notification::{detect_notifications, AgentNotification};
use super::pty::MAX_SCROLLBACK_BYTES;

/// Maximum number of raw output chunks to queue for browser streaming.
/// Each chunk is at most 4096 bytes, so 1000 chunks = ~4MB max.
/// If the browser isn't draining fast enough, oldest chunks are dropped.
const MAX_RAW_OUTPUT_QUEUE_SIZE: usize = 1000;

/// Configuration for spawning a PTY process.
#[derive(Debug)]
pub struct PtySpawnConfig<'a> {
    /// Terminal rows.
    pub rows: u16,
    /// Terminal columns.
    pub cols: u16,
    /// Working directory for the command.
    pub cwd: &'a std::path::Path,
    /// Environment variables to set.
    pub env_vars: &'a std::collections::HashMap<String, String>,
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
#[allow(clippy::implicit_hasher, reason = "internal API doesn't need hasher generalization")]
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
    cmd
}

/// Spawn a CLI PTY reader thread with notification detection.
///
/// Reads PTY output, processes it through the VT100 parser for terminal emulation,
/// adds raw bytes to the scrollback buffer, detects OSC notification sequences,
/// and queues raw output for browser streaming.
pub fn spawn_cli_reader_thread(
    reader: Box<dyn Read + Send>,
    parser: Arc<Mutex<Parser>>,
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
    raw_output_queue: Arc<Mutex<VecDeque<Vec<u8>>>>,
    notification_tx: Sender<AgentNotification>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = reader;
        log::info!("CLI PTY reader thread started");
        let mut buf = [0u8; 4096];
        let mut total_bytes_read: usize = 0;

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total_bytes_read += n;
                    if total_bytes_read % 10240 < n {
                        log::info!("CLI PTY reader: {total_bytes_read} total bytes read");
                    }

                    // Detect notifications in output
                    let notifications = detect_notifications(&buf[..n]);
                    for notif in notifications {
                        log::info!("Sending notification to channel: {:?}", notif);
                        let _ = notification_tx.send(notif);
                    }

                    // Process through parser (for TUI mode screen state)
                    {
                        let mut p = parser.lock().expect("parser lock poisoned");
                        p.process(&buf[..n]);
                    }

                    // Add raw bytes to scrollback buffer
                    {
                        let mut buffer = scrollback_buffer.lock().expect("scrollback_buffer lock poisoned");
                        buffer.extend(buf[..n].iter().copied());
                        // Trim from front if over limit
                        while buffer.len() > MAX_SCROLLBACK_BYTES {
                            buffer.pop_front();
                        }
                    }

                    // Queue raw output for browser streaming (bounded to prevent memory leaks)
                    {
                        let mut queue = raw_output_queue.lock().expect("raw_output_queue lock poisoned");
                        queue.push_back(buf[..n].to_vec());
                        // Drop oldest chunks if queue exceeds limit
                        while queue.len() > MAX_RAW_OUTPUT_QUEUE_SIZE {
                            queue.pop_front();
                        }
                    }
                }
                Err(e) => {
                    log::error!("CLI PTY read error: {e}");
                    break;
                }
            }
        }
        log::info!("CLI PTY reader thread exiting");
    })
}

/// Spawn a Server PTY reader thread (no notification detection).
pub fn spawn_server_reader_thread(
    reader: Box<dyn Read + Send>,
    pty_parser: Arc<Mutex<Parser>>,
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = reader;
        log::info!("Server PTY reader thread started");
        let mut buf = [0u8; 4096];
        let mut total_bytes_read: usize = 0;

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total_bytes_read += n;
                    if total_bytes_read % 10240 < n {
                        log::info!("Server PTY reader: {total_bytes_read} total bytes read");
                    }

                    // Process through parser (for TUI mode screen state)
                    {
                        let mut parser = pty_parser.lock().expect("parser lock poisoned");
                        parser.process(&buf[..n]);
                    }

                    // Add raw bytes to scrollback buffer
                    {
                        let mut buffer = scrollback_buffer.lock().expect("scrollback_buffer lock poisoned");
                        buffer.extend(buf[..n].iter().copied());
                        // Trim from front if over limit
                        while buffer.len() > MAX_SCROLLBACK_BYTES {
                            buffer.pop_front();
                        }
                    }
                }
                Err(e) => {
                    log::error!("Server PTY read error: {e}");
                    break;
                }
            }
        }
        log::info!("Server PTY reader thread exiting");
    })
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
}
