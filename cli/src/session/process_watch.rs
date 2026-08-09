//! Hub-side watchers for per-session OS processes.
//!
//! The hub used to `mem::forget` each `botster session` child. That left zombies
//! and erased the OS exit status, so hard deaths always looked like
//! `exit_code=None` socket EOF.
//!
//! This registry keeps the `std::process::Child` for processes this hub spawned,
//! reaps them on demand, and reports exit code vs terminating signal.

// Rust guideline compliant 2026-08

use std::collections::HashMap;
use std::process::{Child, ExitStatus};
use std::sync::Mutex;

use std::os::unix::process::ExitStatusExt;

/// Outcome of reaping a session process the hub spawned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReapStatus {
    /// Session process pid that was waited on.
    pub pid: u32,
    /// Normal exit code when the process exited without a signal.
    pub exit_code: Option<i32>,
    /// POSIX signal that terminated the process, when applicable.
    pub signal: Option<i32>,
    /// Whether `wait`/`try_wait` actually reaped a status (false if unknown).
    pub reaped: bool,
}

impl SessionReapStatus {
    /// Shell-style status for hard-death consumers.
    ///
    /// - Normal exit → that code
    /// - Signal death → `128 + signal` (Unix shell convention)
    /// - Unreaped → `None`
    #[must_use]
    pub fn effective_exit_code(&self) -> Option<i32> {
        if let Some(code) = self.exit_code {
            return Some(code);
        }
        self.signal.map(|sig| 128 + sig)
    }

    /// Compact log line for hub diagnostics.
    #[must_use]
    pub fn summary(&self) -> String {
        match (self.exit_code, self.signal, self.reaped) {
            (Some(code), _, true) => format!("pid={} exit_code={}", self.pid, code),
            (None, Some(sig), true) => {
                format!(
                    "pid={} signal={} effective_exit={}",
                    self.pid,
                    sig,
                    128 + sig
                )
            }
            (_, _, false) => format!("pid={} unreaped", self.pid),
            (None, None, true) => format!("pid={} exit_unknown", self.pid),
        }
    }
}

fn status_from_exit(pid: u32, status: ExitStatus) -> SessionReapStatus {
    let signal = status.signal();
    let exit_code = if signal.is_some() {
        None
    } else {
        status.code()
    };
    SessionReapStatus {
        pid,
        exit_code,
        signal,
        reaped: true,
    }
}

/// Thread-safe map of session UUID → OS child this hub spawned.
#[derive(Debug, Default)]
pub struct SessionProcessRegistry {
    children: Mutex<HashMap<String, Child>>,
}

impl SessionProcessRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
        }
    }

    /// Track a session process spawned by this hub.
    ///
    /// Replaces any prior entry for the same UUID (reaps the old child first).
    pub fn track(&self, session_uuid: impl Into<String>, child: Child) {
        let session_uuid = session_uuid.into();
        let pid = child.id();
        let mut map = self.children.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut old) = map.insert(session_uuid.clone(), child) {
            let old_pid = old.id();
            match old.try_wait() {
                Ok(Some(status)) => {
                    let reaped = status_from_exit(old_pid, status);
                    log::warn!(
                        "[SessionWatch] replaced live watch for '{}' old={}",
                        &session_uuid[..session_uuid.len().min(16)],
                        reaped.summary()
                    );
                }
                Ok(None) => {
                    log::warn!(
                        "[SessionWatch] replaced still-running watch for '{}' old_pid={}",
                        &session_uuid[..session_uuid.len().min(16)],
                        old_pid
                    );
                    // Detach old: avoid blocking spawn path; OS will keep zombie until hub exit.
                    std::mem::forget(old);
                }
                Err(e) => {
                    log::warn!(
                        "[SessionWatch] try_wait on replaced child pid={old_pid} failed: {e}"
                    );
                    std::mem::forget(old);
                }
            }
        }
        log::debug!(
            "[SessionWatch] tracking '{}' pid={}",
            &session_uuid[..session_uuid.len().min(16)],
            pid
        );
    }

    /// Non-blocking reap if the child has already exited.
    ///
    /// Removes the entry when a status is available. Returns `None` when the
    /// UUID is unknown, or when the process is still running.
    pub fn try_reap(&self, session_uuid: &str) -> Option<SessionReapStatus> {
        let mut map = self.children.lock().unwrap_or_else(|e| e.into_inner());
        let child = map.get_mut(session_uuid)?;
        let pid = child.id();
        match child.try_wait() {
            Ok(Some(status)) => {
                map.remove(session_uuid);
                Some(status_from_exit(pid, status))
            }
            Ok(None) => None,
            Err(e) => {
                log::warn!(
                    "[SessionWatch] try_wait failed for '{}' pid={pid}: {e}",
                    &session_uuid[..session_uuid.len().min(16)]
                );
                map.remove(session_uuid);
                Some(SessionReapStatus {
                    pid,
                    exit_code: None,
                    signal: None,
                    reaped: false,
                })
            }
        }
    }

    /// Blocking wait for a tracked session process.
    ///
    /// Use after socket EOF when the process is expected to be dead or dying.
    /// Returns `None` when this hub never tracked the UUID (recovered session).
    pub fn wait_reap(&self, session_uuid: &str) -> Option<SessionReapStatus> {
        let mut map = self.children.lock().unwrap_or_else(|e| e.into_inner());
        let mut child = map.remove(session_uuid)?;
        let pid = child.id();
        match child.wait() {
            Ok(status) => Some(status_from_exit(pid, status)),
            Err(e) => {
                log::warn!(
                    "[SessionWatch] wait failed for '{}' pid={pid}: {e}",
                    &session_uuid[..session_uuid.len().min(16)]
                );
                Some(SessionReapStatus {
                    pid,
                    exit_code: None,
                    signal: None,
                    reaped: false,
                })
            }
        }
    }

    /// Drop the watch without waiting (hub shutdown / unregister).
    ///
    /// Prefer [`Self::try_reap`] or [`Self::wait_reap`] when diagnosing death.
    pub fn forget(&self, session_uuid: &str) -> bool {
        let mut map = self.children.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(child) = map.remove(session_uuid) {
            std::mem::forget(child);
            true
        } else {
            false
        }
    }

    /// Number of tracked session processes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.children
            .lock()
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// Whether the registry has no tracked children.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn try_reap_returns_exit_code_for_exited_child() {
        let registry = SessionProcessRegistry::new();
        let child = Command::new("true")
            .spawn()
            .expect("spawn true");
        let uuid = "sess-test-reap-exit";
        registry.track(uuid, child);

        // true exits immediately; spin briefly for the OS to mark it waited.
        let status = (0..50)
            .find_map(|_| {
                if let Some(s) = registry.try_reap(uuid) {
                    return Some(s);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                None
            })
            .expect("child should exit");

        assert!(status.reaped);
        assert_eq!(status.exit_code, Some(0));
        assert_eq!(status.signal, None);
        assert_eq!(status.effective_exit_code(), Some(0));
        assert!(registry.is_empty());
    }

    #[test]
    fn wait_reap_reports_signal_for_killed_child() {
        let registry = SessionProcessRegistry::new();
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let uuid = "sess-test-reap-signal";
        registry.track(uuid, child);

        // SAFETY: kill is used only on the test child we just spawned.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }

        let status = registry.wait_reap(uuid).expect("tracked child");
        assert!(status.reaped);
        assert_eq!(status.signal, Some(libc::SIGKILL));
        assert_eq!(status.exit_code, None);
        assert_eq!(status.effective_exit_code(), Some(128 + libc::SIGKILL));
        assert!(registry.is_empty());
    }

    #[test]
    fn try_reap_unknown_uuid_is_none() {
        let registry = SessionProcessRegistry::new();
        assert!(registry.try_reap("sess-missing").is_none());
    }
}
