//! CLI PTY spawning for agents.
//!
//! This module provides the CLI PTY spawning functionality, handling:
//! - PTY creation with specified dimensions
//! - Process spawning with environment variables
//! - Reader thread setup for output broadcasting
//! - Notification channel configuration
//!
//! # Event-Driven Output
//!
//! The reader thread broadcasts [`PtyEvent::Output`] via the PTY session's
//! event channel. Clients subscribe to receive output events:
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
//!
//! // Subscribe to output events
//! let rx = agent.cli_pty.subscribe();
//! ```

// Rust guideline compliant 2026-01

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
/// 1. Opens a PTY with dimensions from the PtySession
/// 2. Spawns the command in the PTY
/// 3. Sets up the notification channel
/// 4. Configures the PTY session
/// 5. Starts the reader thread (broadcasts events, does NOT parse)
/// 6. Starts the command processor task
/// 7. Sends initial context and commands
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
#[allow(
    clippy::implicit_hasher,
    reason = "internal API doesn't need hasher generalization"
)]
pub fn spawn_cli_pty(
    pty: &mut PtySession,
    worktree_path: &Path,
    command_str: &str,
    env_vars: &HashMap<String, String>,
    init_commands: Vec<String>,
    context: &str,
) -> Result<CliSpawnResult> {
    // Get dimensions from the PtySession
    let (rows, cols) = pty.dimensions();

    // Open PTY and spawn command
    let pair = spawn::open_pty(rows, cols)?;
    let cmd = spawn::build_command(command_str, worktree_path, env_vars);
    let child = pair
        .slave
        .spawn_command(cmd)
        .context("Failed to spawn command")?;

    // Set up notification channel
    let (notification_tx, notification_rx) = mpsc::channel::<AgentNotification>();

    // Configure pty with spawned PTY
    pty.set_child(child);
    pty.set_writer(pair.master.take_writer()?);
    pty.notification_tx = Some(notification_tx.clone());

    // Start reader thread - broadcasts events (clients parse in their own parsers)
    let reader = pair.master.try_clone_reader()?;
    pty.reader_thread = Some(spawn::spawn_cli_reader_thread(
        reader,
        Arc::clone(&pty.scrollback_buffer),
        pty.event_sender(),
        notification_tx,
    ));

    pty.set_master_pty(pair.master);

    // Start command processor task - handles Input, Resize, Connect, Disconnect
    pty.spawn_command_processor();

    // Send initial context
    if !context.is_empty() {
        let _ = pty.write_input_str(&format!("{context}\n"));
    }

    // Send init commands
    if !init_commands.is_empty() {
        log::info!("Sending {} init command(s)", init_commands.len());
        thread::sleep(Duration::from_millis(100));
        for cmd in init_commands {
            log::debug!("Running init command: {cmd}");
            let _ = pty.write_input_str(&format!("{cmd}\n"));
            thread::sleep(Duration::from_millis(50));
        }
    }

    Ok(CliSpawnResult { notification_rx })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_spawn_cli_pty_basic() {
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
