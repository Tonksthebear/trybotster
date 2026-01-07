//! CLI PTY spawning for agents.
//!
//! This module provides the CLI PTY spawning functionality, handling:
//! - PTY creation with specified dimensions
//! - Process spawning with environment variables
//! - Reader thread setup for output processing
//! - Notification channel configuration
//!
//! # Usage
//!
//! ```ignore
//! let result = spawn_cli_pty(
//!     &mut agent.cli_pty,
//!     &worktree_path,
//!     "bash",
//!     &env_vars,
//!     vec!["source .botster_init".to_string()],
//!     "Context for the agent",
//! )?;
//! ```

// Rust guideline compliant 2025-01

use std::collections::HashMap;
use std::path::Path;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use super::PtySession;
use crate::agent::notification::AgentNotification;
use crate::agent::spawn;

/// Result of spawning a CLI PTY.
#[derive(Debug)]
pub struct CliSpawnResult {
    /// Notification receiver for OSC sequences.
    pub notification_rx: mpsc::Receiver<AgentNotification>,
}

/// Spawn a CLI PTY process.
///
/// This function:
/// 1. Opens a PTY with the current parser dimensions
/// 2. Spawns the command in the PTY
/// 3. Sets up the notification channel
/// 4. Configures the PTY session
/// 5. Starts the reader thread
/// 6. Sends initial context and commands
///
/// # Arguments
///
/// * `pty` - The PtySession to configure with the spawned process
/// * `worktree_path` - Working directory for the process
/// * `command_str` - Command to execute (e.g., "bash")
/// * `env_vars` - Environment variables to set
/// * `init_commands` - Commands to run after spawn
/// * `context` - Initial context to send to the process
///
/// # Returns
///
/// A `CliSpawnResult` containing the notification receiver.
///
/// # Errors
///
/// Returns an error if PTY creation or command spawn fails.
#[allow(clippy::implicit_hasher, reason = "internal API doesn't need hasher generalization")]
pub fn spawn_cli_pty(
    pty: &mut PtySession,
    worktree_path: &Path,
    command_str: &str,
    env_vars: &HashMap<String, String>,
    init_commands: Vec<String>,
    context: &str,
) -> Result<CliSpawnResult> {
    let (rows, cols) = {
        let parser = pty.vt100_parser.lock().expect("parser lock poisoned");
        parser.screen().size()
    };

    // Open PTY and spawn command
    let pair = spawn::open_pty(rows, cols)?;
    let cmd = spawn::build_command(command_str, worktree_path, env_vars);
    let child = pair.slave.spawn_command(cmd).context("Failed to spawn command")?;

    // Set up notification channel
    let (notification_tx, notification_rx) = mpsc::channel::<AgentNotification>();

    // Configure pty with spawned PTY
    pty.set_child(child);
    pty.writer = Some(pair.master.take_writer()?);
    pty.notification_tx = Some(notification_tx.clone());

    // Start reader thread
    let reader = pair.master.try_clone_reader()?;
    pty.reader_thread = Some(spawn::spawn_cli_reader_thread(
        reader,
        Arc::clone(&pty.vt100_parser),
        Arc::clone(&pty.buffer),
        notification_tx,
    ));

    pty.master_pty = Some(pair.master);

    // Send initial context
    if !context.is_empty() {
        if let Some(ref mut writer) = pty.writer {
            use std::io::Write;
            let _ = writer.write_all(format!("{context}\n").as_bytes());
            let _ = writer.flush();
        }
    }

    // Send init commands
    if !init_commands.is_empty() {
        log::info!("Sending {} init command(s)", init_commands.len());
        thread::sleep(Duration::from_millis(100));
        for cmd in init_commands {
            log::debug!("Running init command: {cmd}");
            if let Some(ref mut writer) = pty.writer {
                use std::io::Write;
                let _ = writer.write_all(format!("{cmd}\n").as_bytes());
                let _ = writer.flush();
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    Ok(CliSpawnResult { notification_rx })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_spawn_cli_pty_basic() {
        let temp_dir = TempDir::new().unwrap();
        let mut pty = PtySession::new(24, 80);

        let result = spawn_cli_pty(
            &mut pty,
            temp_dir.path(),
            "echo hello",
            &HashMap::new(),
            vec![],
            "",
        );

        // Should succeed in spawning
        assert!(result.is_ok());
        assert!(pty.is_spawned());
    }
}
