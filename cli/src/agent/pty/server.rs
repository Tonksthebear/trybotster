//! Server PTY spawning for agents.
//!
//! This module provides the server PTY spawning functionality for running
//! dev servers in a separate PTY alongside the main CLI process.
//!
//! # Usage
//!
//! ```ignore
//! let server_pty = spawn_server_pty(
//!     worktree_path,
//!     ".botster_server",
//!     &env_vars,
//!     (24, 80),
//! )?;
//! ```

// Rust guideline compliant 2025-01

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use super::PtySession;
use crate::agent::spawn;

/// Spawn a server PTY to run a dev server.
///
/// This function:
/// 1. Creates a new PtySession
/// 2. Opens a PTY with the specified dimensions
/// 3. Spawns a bash shell in the PTY
/// 4. Starts the reader thread (no notification detection)
/// 5. Sources the init script to start the server
///
/// # Arguments
///
/// * `worktree_path` - Working directory for the server
/// * `init_script` - Script to source (e.g., ".botster_server")
/// * `env_vars` - Environment variables to set
/// * `dims` - Terminal dimensions (rows, cols)
///
/// # Returns
///
/// A configured `PtySession` with the server process running.
///
/// # Errors
///
/// Returns an error if PTY creation or shell spawn fails.
#[allow(clippy::implicit_hasher, reason = "internal API doesn't need hasher generalization")]
pub fn spawn_server_pty(
    worktree_path: &Path,
    init_script: &str,
    env_vars: &HashMap<String, String>,
    dims: (u16, u16),
) -> Result<PtySession> {
    let (rows, cols) = dims;
    log::info!("Spawning server PTY with init script: {init_script}");

    // Open PTY and spawn bash shell
    let pair = spawn::open_pty(rows, cols)?;
    let cmd = spawn::build_command("bash", worktree_path, env_vars);
    let child = pair.slave.spawn_command(cmd).context("Failed to spawn server shell")?;

    // Set up PTY session
    let mut server_pty = PtySession::new(rows, cols);
    server_pty.set_child(child);
    server_pty.writer = Some(pair.master.take_writer()?);

    // Start reader thread (no notification detection for server)
    let reader = pair.master.try_clone_reader()?;
    server_pty.reader_thread = Some(spawn::spawn_server_reader_thread(
        reader,
        Arc::clone(&server_pty.vt100_parser),
        Arc::clone(&server_pty.scrollback_buffer),
    ));

    server_pty.master_pty = Some(pair.master);

    // Send init script command
    thread::sleep(Duration::from_millis(100));
    if let Some(ref mut writer) = server_pty.writer {
        log::info!("Sending init command to server PTY: source {init_script}");
        writer.write_all(format!("source {init_script}\n").as_bytes())?;
        writer.flush()?;
    }

    log::info!("Server PTY spawned successfully");
    Ok(server_pty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_spawn_server_pty_basic() {
        let temp_dir = TempDir::new().unwrap();

        // Create a simple init script
        let script_path = temp_dir.path().join(".botster_server");
        std::fs::write(&script_path, "# test script\necho 'server started'").unwrap();

        let mut env = HashMap::new();
        env.insert("BOTSTER_TUNNEL_PORT".to_string(), "8080".to_string());

        let result = spawn_server_pty(
            temp_dir.path(),
            ".botster_server",
            &env,
            (24, 80),
        );

        assert!(result.is_ok());
        let pty = result.unwrap();
        assert!(pty.is_spawned());
    }
}
