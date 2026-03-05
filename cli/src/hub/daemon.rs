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
use serde::{Deserialize, Serialize};

/// Hub runtime manifest persisted under `{config_dir}/hubs/{hub_id}/manifest.json`.
///
/// This artifact lets child sessions (for example `botster mcp-serve`) resolve
/// a live hub socket by server-assigned ID instead of trusting inherited env.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HubManifest {
    /// Stable local hub hash ID (matches socket filename stem).
    pub hub_id: String,
    /// Optional server-assigned hub ID (`hub.server_id()` in Lua).
    pub server_id: Option<String>,
    /// Absolute socket path for the hub.
    pub socket_path: String,
    /// PID of the hub process that wrote this manifest.
    pub pid: u32,
    /// Last write timestamp (unix seconds).
    pub updated_at: u64,
}

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

/// Get the manifest file path for a hub.
pub fn manifest_path(hub_id: &str) -> Result<PathBuf> {
    Ok(hub_dir(hub_id)?.join("manifest.json"))
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

/// Write or update the hub runtime manifest.
pub fn write_manifest(hub_id: &str, server_id: Option<&str>) -> Result<()> {
    let socket = socket_path(hub_id)?;
    let path = manifest_path(hub_id)?;
    let manifest = HubManifest {
        hub_id: hub_id.to_string(),
        server_id: server_id
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string),
        socket_path: socket.to_string_lossy().into_owned(),
        pid: std::process::id(),
        updated_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let content =
        serde_json::to_string_pretty(&manifest).context("Failed to serialize hub manifest")?;
    fs::write(&path, content)
        .with_context(|| format!("Failed to write hub manifest: {}", path.display()))?;
    Ok(())
}

/// Read a hub runtime manifest.
///
/// Returns `None` if the manifest is missing or invalid JSON.
pub fn read_manifest(hub_id: &str) -> Option<HubManifest> {
    let path = manifest_path(hub_id).ok()?;
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Return true if a manifest appears live (PID alive + socket exists).
fn manifest_is_live(manifest: &HubManifest) -> bool {
    let pid_alive = unsafe { libc::kill(manifest.pid as libc::pid_t, 0) == 0 };
    let socket_alive = PathBuf::from(&manifest.socket_path).exists();
    pid_alive && socket_alive
}

/// Resolve a hub socket by server-assigned hub ID using persisted manifests.
///
/// Returns `None` when no manifest matches the server ID.
pub fn resolve_socket_for_server_id(server_id: &str) -> Option<PathBuf> {
    let hubs_dir = crate::config::Config::config_dir().ok()?.join("hubs");
    let entries = fs::read_dir(&hubs_dir).ok()?;

    for entry in entries.flatten() {
        if !entry.file_type().map_or(false, |t| t.is_dir()) {
            continue;
        }
        let hub_id = entry.file_name().to_string_lossy().into_owned();
        let Some(manifest) = read_manifest(&hub_id) else {
            continue;
        };
        if manifest.server_id.as_deref() == Some(server_id) {
            if !manifest_is_live(&manifest) {
                cleanup_stale_files(&manifest.hub_id);
                continue;
            }
            return Some(PathBuf::from(manifest.socket_path));
        }
    }
    None
}

/// Resolve a hub socket by local hub ID using its persisted runtime manifest.
///
/// Returns `None` when the manifest is missing or stale (dead PID / missing socket).
pub fn resolve_socket_for_hub_id(hub_id: &str) -> Option<PathBuf> {
    let manifest = read_manifest(hub_id)?;
    if !manifest_is_live(&manifest) {
        cleanup_stale_files(&manifest.hub_id);
        return None;
    }
    Some(PathBuf::from(manifest.socket_path))
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
    let current_pid = std::process::id();

    // Only the owning hub process may remove runtime artifacts.
    // This prevents a stale/duplicate shutdown path from unlinking a live hub
    // socket that belongs to a different PID.
    if let Some(owner_pid) = read_pid_file(hub_id) {
        if owner_pid != current_pid {
            log::warn!(
                "Skipping daemon cleanup for hub {}: pid file owned by {} (current pid={})",
                &hub_id[..hub_id.len().min(8)],
                owner_pid,
                current_pid
            );
            return;
        }
    }

    if let Some(manifest) = read_manifest(hub_id) {
        if manifest.pid != current_pid {
            log::warn!(
                "Skipping daemon cleanup for hub {}: manifest owned by {} (current pid={})",
                &hub_id[..hub_id.len().min(8)],
                manifest.pid,
                current_pid
            );
            return;
        }
    }

    if let Ok(path) = pid_file_path(hub_id) {
        let _ = fs::remove_file(&path);
    }
    if let Ok(path) = socket_path(hub_id) {
        let _ = fs::remove_file(&path);
    }
    if let Ok(path) = manifest_path(hub_id) {
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
    fn test_cleanup_on_shutdown_skips_foreign_pid_file_owner() {
        let test_id = format!("_test_foreign_pid_{}", std::process::id());
        let pid_path = pid_file_path(&test_id).unwrap();
        let socket_path = socket_path(&test_id).unwrap();

        fs::write(&pid_path, "999999").unwrap();
        fs::write(&socket_path, b"").unwrap();

        cleanup_on_shutdown(&test_id);

        assert!(pid_path.exists(), "foreign pid file should remain");
        assert!(socket_path.exists(), "foreign socket should remain");

        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(&socket_path);
        if let Ok(path) = manifest_path(&test_id) {
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn test_cleanup_on_shutdown_skips_foreign_manifest_owner() {
        let test_id = format!("_test_foreign_manifest_{}", std::process::id());
        let manifest = HubManifest {
            hub_id: test_id.clone(),
            server_id: Some("server-xyz".to_string()),
            socket_path: socket_path(&test_id).unwrap().to_string_lossy().into_owned(),
            pid: 999999,
            updated_at: 1,
        };
        let manifest_content = serde_json::to_string_pretty(&manifest).unwrap();
        fs::write(manifest_path(&test_id).unwrap(), manifest_content).unwrap();

        let pid_path = pid_file_path(&test_id).unwrap();
        let socket_path = socket_path(&test_id).unwrap();
        fs::write(&pid_path, std::process::id().to_string()).unwrap();
        fs::write(&socket_path, b"").unwrap();

        cleanup_on_shutdown(&test_id);

        assert!(manifest_path(&test_id).unwrap().exists(), "foreign manifest should remain");
        assert!(pid_path.exists(), "pid file should remain when cleanup is skipped");
        assert!(socket_path.exists(), "socket should remain when cleanup is skipped");

        let _ = fs::remove_file(pid_path);
        let _ = fs::remove_file(socket_path);
        let _ = fs::remove_file(manifest_path(&test_id).unwrap());
    }

    #[test]
    fn test_manifest_round_trip() {
        let test_id = format!("_test_manifest_{}", std::process::id());
        write_manifest(&test_id, Some("123")).unwrap();
        let manifest = read_manifest(&test_id).expect("manifest should exist");
        assert_eq!(manifest.hub_id, test_id);
        assert_eq!(manifest.server_id.as_deref(), Some("123"));
        assert!(manifest.socket_path.ends_with(".sock"));
        assert!(manifest.updated_at > 0);
        cleanup_on_shutdown(&manifest.hub_id);
    }

    #[test]
    fn test_resolve_socket_for_server_id() {
        let test_id = format!("_test_server_lookup_{}", std::process::id());
        write_manifest(&test_id, Some("hub-server-id-xyz")).unwrap();
        // Make the socket path exist so liveness check passes.
        let socket = socket_path(&test_id).unwrap();
        fs::write(&socket, b"").unwrap();
        let socket =
            resolve_socket_for_server_id("hub-server-id-xyz").expect("socket should resolve");
        assert!(socket.to_string_lossy().ends_with(&format!("/{test_id}.sock")));
        cleanup_on_shutdown(&test_id);
    }

    #[test]
    fn test_resolve_socket_for_hub_id() {
        let test_id = format!("_test_local_lookup_{}", std::process::id());
        write_manifest(&test_id, Some("ignored-server-id")).unwrap();
        // Make the socket path exist so liveness check passes.
        let socket = socket_path(&test_id).unwrap();
        fs::write(&socket, b"").unwrap();
        let resolved = resolve_socket_for_hub_id(&test_id).expect("socket should resolve");
        assert!(resolved.to_string_lossy().ends_with(&format!("/{test_id}.sock")));
        cleanup_on_shutdown(&test_id);
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
