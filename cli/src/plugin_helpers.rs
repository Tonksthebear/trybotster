//! Generic plugin command resolution and URL readiness helpers.
//!
//! Plugins sometimes discover a public URL before the hostname is globally
//! resolvable. The URL is only considered ready once public DNS returns an
//! address and the HTTPS origin itself responds.

use std::path::{Path, PathBuf};

const PUBLIC_DOH_URL: &str = "https://1.1.1.1/dns-query";
const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Prepared command returned after any blocking plugin setup completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPluginCommand {
    /// Absolute executable path resolved from the Hub process environment.
    pub command: PathBuf,
    /// Optional config file path written for the plugin command.
    pub config_path: Option<PathBuf>,
}

/// Structured reason for plugin command preparation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCommandPrepareErrorKind {
    /// Command was empty after trimming.
    CommandBlank,
    /// Command could not be found or was not executable.
    CommandMissing,
    /// Optional config file could not be written.
    ConfigWriteFailed,
}

impl PluginCommandPrepareErrorKind {
    /// Stable string exposed to Lua plugins.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CommandBlank => "command_blank",
            Self::CommandMissing => "command_missing",
            Self::ConfigWriteFailed => "config_write_failed",
        }
    }
}

/// Error returned by plugin command preparation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommandPrepareError {
    /// Stable machine-readable failure kind.
    pub kind: PluginCommandPrepareErrorKind,
    /// User/log-facing failure message.
    pub message: String,
}

impl PluginCommandPrepareError {
    fn new(kind: PluginCommandPrepareErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PluginCommandPrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PluginCommandPrepareError {}

/// Resolve a command using the current process environment.
///
/// Returns an absolute path when the command exists and is executable.
#[must_use]
pub fn resolve_command_path(command: &str) -> Option<PathBuf> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }

    if looks_like_path(trimmed) {
        return resolve_candidate(Path::new(trimmed));
    }

    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(trimmed);
        if let Some(resolved) = resolve_candidate(&candidate) {
            return Some(resolved);
        }
    }

    None
}

/// Resolve a plugin command and optionally write a config file.
///
/// This helper is intentionally synchronous so callers can run it inside
/// `spawn_blocking` while keeping Lua action handlers responsive.
pub fn prepare_plugin_command(
    command: &str,
    config_path: Option<&Path>,
    config_contents: Option<&str>,
) -> Result<PreparedPluginCommand, PluginCommandPrepareError> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err(PluginCommandPrepareError::new(
            PluginCommandPrepareErrorKind::CommandBlank,
            "Command cannot be blank",
        ));
    }

    let resolved = resolve_command_path(trimmed).ok_or_else(|| {
        PluginCommandPrepareError::new(
            PluginCommandPrepareErrorKind::CommandMissing,
            format!("Command not found: {trimmed}"),
        )
    })?;

    let prepared_config_path = if let Some(path) = config_path {
        if let Some(contents) = config_contents {
            std::fs::write(path, contents).map_err(|e| {
                PluginCommandPrepareError::new(
                    PluginCommandPrepareErrorKind::ConfigWriteFailed,
                    format!(
                        "Failed to write plugin command config {}: {e}",
                        path.display()
                    ),
                )
            })?;
        }
        Some(path.to_path_buf())
    } else {
        None
    };

    Ok(PreparedPluginCommand {
        command: resolved,
        config_path: prepared_config_path,
    })
}

fn looks_like_path(command: &str) -> bool {
    Path::new(command).is_absolute() || command.contains('/') || command.contains('\\')
}

fn resolve_candidate(path: &Path) -> Option<PathBuf> {
    if !is_executable_file(path) {
        return None;
    }

    Some(
        std::fs::canonicalize(path)
            .ok()
            .unwrap_or_else(|| path.to_path_buf()),
    )
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

/// Poll public DNS plus the preview HTTPS origin until both are ready.
pub async fn wait_until_url_ready(
    hostname: &str,
    url: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    wait_until_url_ready_with_doh_url(hostname, url, timeout, PUBLIC_DOH_URL).await
}

async fn wait_until_url_ready_with_doh_url(
    hostname: &str,
    url: &str,
    timeout: std::time::Duration,
    doh_url: &str,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("failed to build URL probe client: {e}"))?;

    let doh_query = format!("{doh_url}?name={hostname}&type=A");
    let mut last_error = "preview never became reachable".to_string();

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(last_error);
        }

        match dns_has_a_record(&client, &doh_query).await {
            Ok(true) => {}
            Ok(false) => {
                last_error = "dns returned NOERROR but no A records".to_string();
                tokio::time::sleep(PROBE_INTERVAL).await;
                continue;
            }
            Err(e) => {
                last_error = e;
                tokio::time::sleep(PROBE_INTERVAL).await;
                continue;
            }
        }

        match client.get(url).send().await {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_error = format!("HTTPS probe failed: {e}");
                tokio::time::sleep(PROBE_INTERVAL).await;
            }
        }
    }
}

async fn dns_has_a_record(client: &reqwest::Client, doh_url: &str) -> Result<bool, String> {
    let response = client
        .get(doh_url)
        .header("Accept", "application/dns-json")
        .send()
        .await
        .map_err(|e| format!("DNS probe failed: {e}"))?;

    let json = response
        .json::<serde_json::Value>()
        .await
        .map_err(|e| format!("DNS response parse failed: {e}"))?;

    let status = json.get("Status").and_then(|v| v.as_u64()).unwrap_or(99);
    if status != 0 {
        return Err(format!("DNS returned rcode {status}"));
    }

    let has_a = json
        .get("Answer")
        .and_then(|v| v.as_array())
        .is_some_and(|answers| {
            answers
                .iter()
                .any(|answer| answer.get("type").and_then(|v| v.as_u64()) == Some(1))
        });
    Ok(has_a)
}

#[cfg(test)]
mod tests {
    use super::{prepare_plugin_command, resolve_command_path, wait_until_url_ready};
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn resolves_direct_executable_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-connector");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        make_executable(&path);

        let resolved = resolve_command_path(path.to_str().unwrap());
        let expected = std::fs::canonicalize(&path).unwrap();
        assert_eq!(resolved.as_deref(), Some(expected.as_path()));
    }

    #[test]
    fn finds_executable_on_path() {
        let _guard = env_lock().lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-connector");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        make_executable(&path);

        let old_path = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());

        let resolved = resolve_command_path("preview-connector");

        match old_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }

        let expected = std::fs::canonicalize(&path).unwrap();
        assert_eq!(resolved.as_deref(), Some(expected.as_path()));
    }

    #[test]
    fn returns_none_for_missing_command() {
        let _guard = env_lock().lock().unwrap();
        let old_path = std::env::var_os("PATH");
        std::env::set_var("PATH", "/definitely/missing");

        let resolved = resolve_command_path("preview-connector");

        match old_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }

        assert!(resolved.is_none());
    }

    #[test]
    fn prepares_command_and_writes_config() {
        let dir = tempfile::tempdir().unwrap();
        let command = dir.path().join("preview-connector");
        let config = dir.path().join("preview.json");
        std::fs::write(&command, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        make_executable(&command);

        let prepared =
            prepare_plugin_command(command.to_str().unwrap(), Some(&config), Some("{}\n"))
                .expect("command should prepare");

        assert_eq!(
            prepared.command,
            std::fs::canonicalize(&command).expect("canonical command")
        );
        assert_eq!(prepared.config_path.as_deref(), Some(config.as_path()));
        assert_eq!(
            std::fs::read_to_string(&config).expect("config should be written"),
            "{}\n"
        );
    }

    #[test]
    fn rejects_blank_prepared_command() {
        let err = prepare_plugin_command("  ", None, None).expect_err("blank command should fail");
        assert_eq!(err.kind, super::PluginCommandPrepareErrorKind::CommandBlank);
        assert_eq!(err.message, "Command cannot be blank");
    }

    #[tokio::test]
    async fn rejects_bogus_hostname_quickly() {
        let result = wait_until_url_ready(
            "this-hostname-will-never-exist.invalid",
            "https://this-hostname-will-never-exist.invalid",
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(result.is_err());
    }
}
