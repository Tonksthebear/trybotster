//! Process management utilities for botster.
//!
//! This module provides utilities for managing child processes,
//! including orphan detection and cleanup.
//!
//! # Overview
//!
//! When agents are spawned, they may create child processes (e.g., dev servers)
//! that can become orphaned if the agent terminates unexpectedly. This module
//! provides functions to detect and clean up such processes.
//!
//! # Platform Support
//!
//! Process detection uses platform-specific mechanisms:
//! - **macOS**: Uses `lsof -d cwd` to find processes by working directory
//! - **Linux**: Reads `/proc/<pid>/cwd` symlinks directly
//!
//! # Safety
//!
//! The cleanup functions include safety checks to prevent accidental termination
//! of unrelated processes. Only processes with working directories inside
//! `botster-sessions` directories are considered for cleanup.

// Rust guideline compliant 2025-01

use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Kills orphaned processes that have their working directory inside the given worktree.
///
/// This function identifies processes that may have been left behind when an agent
/// terminates (e.g., background dev servers) and sends them SIGTERM followed by
/// SIGKILL if they don't exit gracefully.
///
/// # Arguments
///
/// * `worktree_path` - Path to the worktree directory to check for orphaned processes
///
/// # Safeguards
///
/// Only processes with working directories containing "botster-sessions" in their
/// path are considered. The calling process and its parent are always excluded.
///
/// # Platform Support
///
/// - **macOS**: Uses `lsof -d cwd -Fpn` for process discovery
/// - **Linux**: Reads `/proc/<pid>/cwd` symlinks
/// - **Other**: No-op (no orphan cleanup performed)
///
/// # Example
///
/// ```ignore
/// use std::path::Path;
/// use botster::process::kill_orphaned_processes;
///
/// let worktree = Path::new("/home/user/botster-sessions/repo-issue-42");
/// kill_orphaned_processes(worktree);
/// ```
pub fn kill_orphaned_processes(worktree_path: &Path) {
    let worktree_str = worktree_path.to_string_lossy();

    // Safety check: only proceed if the worktree path contains "botster-sessions"
    if !worktree_str.contains("botster-sessions") {
        log::debug!(
            "[orphan-cleanup] Skipping - path doesn't contain botster-sessions: {}",
            worktree_str
        );
        return;
    }

    log::debug!("[orphan-cleanup] Checking for orphans in: {}", worktree_str);

    // Get our own PID and parent PID to exclude from killing
    let our_pid = std::process::id();
    let our_ppid = get_parent_pid(our_pid);

    log::debug!(
        "[orphan-cleanup] Our PID: {}, Parent PID: {:?}",
        our_pid,
        our_ppid
    );

    let pids_to_kill = find_processes_in_directory(worktree_path, our_pid, our_ppid);

    if pids_to_kill.is_empty() {
        log::debug!("[orphan-cleanup] No orphaned processes found");
        return;
    }

    graceful_kill_processes(&pids_to_kill);
}

/// Finds all processes with their working directory inside the given path.
///
/// # Arguments
///
/// * `worktree_path` - Path to search for processes
/// * `exclude_pid` - PID to exclude (typically the current process)
/// * `exclude_ppid` - Optional parent PID to exclude
///
/// # Returns
///
/// Vector of PIDs that have their working directory inside the given path.
fn find_processes_in_directory(
    worktree_path: &Path,
    exclude_pid: u32,
    exclude_ppid: Option<u32>,
) -> Vec<u32> {
    let worktree_str = worktree_path.to_string_lossy();
    let mut pids_to_kill: Vec<u32> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        // Get all processes with their CWDs using lsof
        // Format: -F pn outputs "p<pid>" and "n<path>" lines
        let lsof_output = Command::new("lsof")
            .arg("-d")
            .arg("cwd")
            .arg("-Fpn")
            .output();

        if let Ok(output) = lsof_output {
            let lsof_str = String::from_utf8_lossy(&output.stdout);
            let mut current_pid: Option<u32> = None;

            for line in lsof_str.lines() {
                if let Some(pid_str) = line.strip_prefix('p') {
                    current_pid = pid_str.parse().ok();
                } else if let Some(cwd) = line.strip_prefix('n') {
                    if let Some(pid) = current_pid {
                        // Check if CWD matches our worktree
                        if cwd == worktree_str || cwd.starts_with(&format!("{}/", worktree_str)) {
                            // Skip our own process and parent
                            if pid == exclude_pid {
                                log::debug!("[orphan-cleanup] Skipping own PID {}", pid);
                            } else if Some(pid) == exclude_ppid {
                                log::debug!("[orphan-cleanup] Skipping parent PID {}", pid);
                            } else {
                                log::debug!(
                                    "[orphan-cleanup] Found orphan PID {} (CWD: {})",
                                    pid,
                                    cwd
                                );
                                pids_to_kill.push(pid);
                            }
                        }
                    }
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let pid_str = entry.file_name().to_string_lossy().to_string();
                if let Ok(pid) = pid_str.parse::<u32>() {
                    // Skip our own process and parent
                    if pid == exclude_pid || Some(pid) == exclude_ppid {
                        continue;
                    }

                    let cwd_path = format!("/proc/{}/cwd", pid);
                    if let Ok(cwd) = std::fs::read_link(&cwd_path) {
                        let cwd_str = cwd.to_string_lossy();

                        if cwd_str == worktree_str
                            || cwd_str.starts_with(&format!("{}/", worktree_str))
                        {
                            log::debug!(
                                "[orphan-cleanup] Found orphan PID {} (CWD: {})",
                                pid,
                                cwd_str
                            );
                            pids_to_kill.push(pid);
                        }
                    }
                }
            }
        }
    }

    pids_to_kill
}

/// Gracefully kills a list of processes.
///
/// Sends SIGTERM first, waits up to 3 seconds for graceful exit,
/// then sends SIGKILL to any remaining processes.
///
/// # Arguments
///
/// * `pids` - Slice of process IDs to terminate
fn graceful_kill_processes(pids: &[u32]) {
    // Send SIGTERM first
    for pid in pids {
        log::debug!("[orphan-cleanup] Sending SIGTERM to PID {}", pid);
        let _ = Command::new("kill").arg(pid.to_string()).output();
    }

    // Wait up to 3 seconds for processes to exit gracefully
    for _ in 0..6 {
        std::thread::sleep(Duration::from_millis(500));

        // Check if all processes have exited
        let mut all_dead = true;
        for pid in pids {
            if Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                all_dead = false;
                break;
            }
        }
        if all_dead {
            log::debug!("[orphan-cleanup] All processes exited gracefully");
            return;
        }
    }

    // Force kill any remaining processes
    for pid in pids {
        if Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            log::debug!("[orphan-cleanup] Force killing PID {} with SIGKILL", pid);
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).output();
        }
    }
}

/// Gets the parent PID of a process.
///
/// # Arguments
///
/// * `pid` - The process ID to query
///
/// # Returns
///
/// The parent process ID, or `None` if it cannot be determined.
///
/// # Platform Support
///
/// - **macOS**: Uses `ps -o ppid= -p <pid>`
/// - **Linux**: Reads `/proc/<pid>/stat`
/// - **Other**: Always returns `None`
pub fn get_parent_pid(pid: u32) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("ps")
            .arg("-o")
            .arg("ppid=")
            .arg("-p")
            .arg(pid.to_string())
            .output()
            .ok()?;
        let ppid_str = String::from_utf8_lossy(&output.stdout);
        ppid_str.trim().parse().ok()
    }

    #[cfg(target_os = "linux")]
    {
        let stat_path = format!("/proc/{}/stat", pid);
        let stat = std::fs::read_to_string(&stat_path).ok()?;
        // Format: pid (comm) state ppid ...
        let parts: Vec<&str> = stat.split_whitespace().collect();
        parts.get(3)?.parse().ok()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_get_parent_pid_for_current_process() {
        let our_pid = std::process::id();
        let ppid = get_parent_pid(our_pid);

        // We should be able to get our parent's PID on supported platforms
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        assert!(ppid.is_some());

        // Parent PID should be different from our PID
        if let Some(parent) = ppid {
            assert_ne!(parent, our_pid);
        }
    }

    #[test]
    fn test_get_parent_pid_invalid_pid() {
        // Use an unlikely PID that shouldn't exist
        let ppid = get_parent_pid(u32::MAX);
        assert!(ppid.is_none());
    }

    #[test]
    fn test_kill_orphaned_processes_skips_non_botster_paths() {
        // This should return early without doing anything harmful
        let path = PathBuf::from("/tmp/some-random-path");
        kill_orphaned_processes(&path);
        // If we get here without panicking, the test passes
    }

    #[test]
    fn test_find_processes_excludes_current() {
        let our_pid = std::process::id();
        let our_ppid = get_parent_pid(our_pid);

        // Use a path that definitely doesn't match any real worktree
        let fake_path = PathBuf::from("/nonexistent/botster-sessions/test");
        let pids = find_processes_in_directory(&fake_path, our_pid, our_ppid);

        // Should not find our own process or parent
        assert!(!pids.contains(&our_pid));
        if let Some(ppid) = our_ppid {
            assert!(!pids.contains(&ppid));
        }
    }
}
