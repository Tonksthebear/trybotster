//! Hub daemon infrastructure for PID file management and socket discovery.
//!
//! Provides utilities for detecting running hubs, managing PID files,
//! and resolving Unix socket paths for IPC.
//!
//! # File Layout
//!
//! ```text
//! {config_dir}/hubs/{hub_id}/
//!   hub.pid              # PID of the running hub process
//!
//! /tmp/botster-{uid}/
//!   {hub_id}.sock        # Unix domain socket for IPC
//! ```
//!
//! Sockets live in `/tmp` because macOS limits Unix socket paths to 104 bytes,
//! and `~/Library/Application Support/...` exceeds that.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Get the per-hub directory path.
///
/// Returns `{config_dir}/hubs/{hub_id}/`, creating it if needed.
pub fn hub_dir(hub_id: &str) -> Result<PathBuf> {
    let dir = crate::config::Config::config_dir()?.join("hubs").join(hub_id);
    if !dir.exists() {
        fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create hub directory: {}", dir.display()))?;
    }
    Ok(dir)
}

/// Get the PID file path for a hub.
pub fn pid_file_path(hub_id: &str) -> Result<PathBuf> {
    Ok(hub_dir(hub_id)?.join("hub.pid"))
}

/// Get the Unix socket path for a hub.
///
/// Uses `/tmp/botster-{uid}/` instead of the config dir because macOS
/// limits Unix socket paths to 104 bytes, and `~/Library/Application Support/...`
/// is too long.
pub fn socket_path(hub_id: &str) -> Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let dir = PathBuf::from(format!("/tmp/botster-{uid}"));
    if !dir.exists() {
        // Set restrictive umask before creating directory to avoid TOCTOU
        // race between mkdir and chmod on shared /tmp.
        let old_umask = unsafe { libc::umask(0o077) };
        let result = fs::create_dir_all(&dir);
        unsafe { libc::umask(old_umask); }
        result?;
    }
    Ok(dir.join(format!("{hub_id}.sock")))
}

/// Write the current process PID to the hub's PID file.
pub fn write_pid_file(hub_id: &str) -> Result<()> {
    let path = pid_file_path(hub_id)?;
    let pid = std::process::id();
    fs::write(&path, pid.to_string())
        .with_context(|| format!("Failed to write PID file: {}", path.display()))?;
    log::info!("Wrote PID file: {} (pid={})", path.display(), pid);
    Ok(())
}

/// Read the PID from a hub's PID file.
///
/// Returns `None` if the file doesn't exist or can't be parsed.
pub fn read_pid_file(hub_id: &str) -> Option<u32> {
    let path = pid_file_path(hub_id).ok()?;
    let contents = fs::read_to_string(&path).ok()?;
    contents.trim().parse().ok()
}

/// Check if a hub process is running by verifying the PID file.
///
/// Returns `true` if:
/// 1. The PID file exists
/// 2. The PID is parseable
/// 3. The process with that PID is alive (via `kill(pid, 0)`)
pub fn is_hub_running(hub_id: &str) -> bool {
    let Some(pid) = read_pid_file(hub_id) else {
        return false;
    };

    // Check if process is alive using kill(pid, 0)
    // This sends no signal but checks if the process exists
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Remove stale PID and socket files for a hub that is no longer running.
///
/// Only removes files if the recorded PID is not alive. If a hub is
/// already running, this is a no-op to avoid clobbering a live hub's files.
/// Safe to call even if files don't exist.
pub fn cleanup_stale_files(hub_id: &str) {
    // If the hub is still running, don't touch its files
    if is_hub_running(hub_id) {
        log::debug!("Hub {} is still running, skipping stale cleanup", &hub_id[..hub_id.len().min(8)]);
        return;
    }

    if let Ok(path) = pid_file_path(hub_id) {
        if path.exists() {
            let _ = fs::remove_file(&path);
            log::debug!("Removed stale PID file: {}", path.display());
        }
    }
    if let Ok(path) = socket_path(hub_id) {
        if path.exists() {
            let _ = fs::remove_file(&path);
            log::debug!("Removed stale socket file: {}", path.display());
        }
    }
}

/// Remove PID and socket files on shutdown.
///
/// Called from `Hub::shutdown()` to clean up daemon files.
pub fn cleanup_on_shutdown(hub_id: &str) {
    if let Ok(path) = pid_file_path(hub_id) {
        let _ = fs::remove_file(&path);
    }
    if let Ok(path) = socket_path(hub_id) {
        let _ = fs::remove_file(&path);
    }
    log::info!("Cleaned up daemon files for hub {}", &hub_id[..hub_id.len().min(8)]);
}

/// Remove orphaned socket files from `/tmp/botster-{uid}/`.
///
/// Scans the socket directory and removes any `.sock` files that don't
/// have a corresponding live process. This catches sockets left behind
/// by crashed processes, SIGKILL'd test processes, or any other case
/// where `cleanup_on_shutdown` didn't run.
///
/// Safe to call at startup — only removes sockets for dead processes.
pub fn cleanup_orphaned_sockets() {
    let uid = unsafe { libc::getuid() };
    let dir = PathBuf::from(format!("/tmp/botster-{uid}"));

    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension() else { continue };
        if ext != "sock" {
            continue;
        }

        // Extract hub_id from filename (strip .sock)
        let Some(hub_id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };

        // Skip broker sockets (named "broker-{hub_id}.sock").
        //
        // Broker sockets are owned by the broker subprocess, which has no
        // corresponding hub PID file. `is_hub_running("broker-{hub_id}")` would
        // always return false and cause the socket to be incorrectly deleted
        // while the broker is still running and holding live PTY FDs. The broker
        // cleans up its own socket on exit (see `broker::run()`).
        if hub_id.starts_with("broker-") {
            continue;
        }

        // If the hub has a live PID, keep its socket
        if is_hub_running(hub_id) {
            continue;
        }

        // No live process — remove the orphaned socket
        if fs::remove_file(&path).is_ok() {
            removed += 1;
            log::debug!("Removed orphaned socket: {}", path.display());
        }
    }

    if removed > 0 {
        log::info!("Cleaned up {removed} orphaned socket(s) from {}", dir.display());
    }
}

/// Discover all running hubs by scanning PID files.
///
/// Returns a list of `(hub_id, pid)` pairs for running hubs.
pub fn discover_running_hubs() -> Vec<(String, u32)> {
    let hubs_dir = match crate::config::Config::config_dir() {
        Ok(dir) => dir.join("hubs"),
        Err(_) => return Vec::new(),
    };

    let entries = match fs::read_dir(&hubs_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut running = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().map_or(false, |t| t.is_dir()) {
            continue;
        }
        let hub_id = entry.file_name().to_string_lossy().into_owned();

        if let Some(pid) = read_pid_file(&hub_id) {
            if unsafe { libc::kill(pid as libc::pid_t, 0) == 0 } {
                running.push((hub_id, pid));
            } else {
                // Process dead, clean up stale files
                cleanup_stale_files(&hub_id);
            }
        }
    }

    running
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_path_format() {
        let path = socket_path("abc123").unwrap();
        let path_str = path.to_string_lossy();
        assert!(path_str.starts_with("/tmp/botster-"), "Expected /tmp/botster-*, got: {path_str}");
        assert!(path_str.ends_with("/abc123.sock"), "Expected *.sock, got: {path_str}");
    }

    #[test]
    fn test_pid_file_path_format() {
        let path = pid_file_path("abc123").unwrap();
        assert!(path.to_string_lossy().contains("hubs/abc123/hub.pid"));
    }

    #[test]
    fn test_read_nonexistent_pid_file() {
        assert!(read_pid_file("nonexistent_hub_id_12345").is_none());
    }

    #[test]
    fn test_is_hub_running_nonexistent() {
        assert!(!is_hub_running("nonexistent_hub_id_12345"));
    }

    #[test]
    fn test_pid_file_write_read_cleanup_cycle() {
        let test_id = format!("_test_pid_{}", std::process::id());

        write_pid_file(&test_id).unwrap();
        assert_eq!(read_pid_file(&test_id), Some(std::process::id()));
        assert!(is_hub_running(&test_id));

        cleanup_on_shutdown(&test_id);
        assert!(read_pid_file(&test_id).is_none());
        assert!(!is_hub_running(&test_id));
    }

    #[test]
    fn test_discover_running_hubs_includes_self() {
        let test_id = format!("_test_discover_{}", std::process::id());
        write_pid_file(&test_id).unwrap();

        let running = discover_running_hubs();
        let found = running.iter().any(|(id, pid)| id == &test_id && *pid == std::process::id());
        assert!(found, "discover_running_hubs should find our test hub, got: {running:?}");

        cleanup_on_shutdown(&test_id);
    }

    /// Verifies that `cleanup_orphaned_sockets` does NOT delete broker sockets.
    ///
    /// Broker sockets are named `broker-{hub_id}.sock`. They have no matching
    /// hub PID file, so `is_hub_running("broker-{hub_id}")` always returns false.
    /// Without the `broker-` prefix guard introduced in the bug fix, the cleanup
    /// would incorrectly delete the live broker socket, breaking hub restart recovery.
    #[test]
    fn test_cleanup_orphaned_sockets_preserves_broker_socket() {
        let uid = unsafe { libc::getuid() };
        let dir = PathBuf::from(format!("/tmp/botster-{uid}"));
        fs::create_dir_all(&dir).unwrap();

        // Use process id for a unique broker socket name — no hub PID file will exist.
        let hub_id = format!("_test_hub_{}", std::process::id());
        let broker_sock = dir.join(format!("broker-{hub_id}.sock"));
        fs::write(&broker_sock, b"").unwrap();

        cleanup_orphaned_sockets();

        assert!(
            broker_sock.exists(),
            "cleanup_orphaned_sockets must not delete broker socket: {}",
            broker_sock.display()
        );

        let _ = fs::remove_file(&broker_sock);
    }

    /// Verifies that `cleanup_orphaned_sockets` removes stale hub sockets.
    ///
    /// A hub socket whose stem matches no live PID file is orphaned (e.g., left
    /// behind by a crashed process). The cleanup must remove it so a restarted
    /// hub can bind a fresh socket at the same path without EADDRINUSE.
    #[test]
    fn test_cleanup_orphaned_sockets_removes_stale_hub_socket() {
        let uid = unsafe { libc::getuid() };
        let dir = PathBuf::from(format!("/tmp/botster-{uid}"));
        fs::create_dir_all(&dir).unwrap();

        // Use a unique id with no corresponding PID file — simulates a crashed hub.
        let stale_id = format!("_test_stale_{}", std::process::id());
        let stale_sock = dir.join(format!("{stale_id}.sock"));
        fs::write(&stale_sock, b"").unwrap();

        // Precondition: no PID file exists for this id.
        assert!(read_pid_file(&stale_id).is_none(), "test precondition: no PID file");

        cleanup_orphaned_sockets();

        assert!(
            !stale_sock.exists(),
            "cleanup_orphaned_sockets should remove stale hub socket: {}",
            stale_sock.display()
        );
    }
}
