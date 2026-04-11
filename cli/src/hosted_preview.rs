//! Hosted preview command resolution and readiness gate.
//!
//! cloudflared prints the quick-tunnel URL before the hostname is globally
//! resolvable. A preview is only considered ready once Cloudflare DNS returns
//! an address and the HTTPS origin itself responds.

use std::path::{Path, PathBuf};

const CLOUDFLARE_DOH_URL: &str = "https://1.1.1.1/dns-query";
const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

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

/// Poll Cloudflare DNS plus the preview HTTPS origin until both are ready.
pub async fn wait_until_dns_ready(
    hostname: &str,
    url: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    wait_until_dns_ready_with_doh_url(hostname, url, timeout, CLOUDFLARE_DOH_URL).await
}

async fn wait_until_dns_ready_with_doh_url(
    hostname: &str,
    url: &str,
    timeout: std::time::Duration,
    doh_url: &str,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("failed to build preview probe client: {e}"))?;

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
    use super::{resolve_command_path, wait_until_dns_ready};
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
        let path = dir.path().join("cloudflared");
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
        let path = dir.path().join("cloudflared");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        make_executable(&path);

        let old_path = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());

        let resolved = resolve_command_path("cloudflared");

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

        let resolved = resolve_command_path("cloudflared");

        match old_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }

        assert!(resolved.is_none());
    }

    #[tokio::test]
    async fn rejects_bogus_hostname_quickly() {
        let result = wait_until_dns_ready(
            "this-hostname-will-never-exist.trycloudflare.com",
            "https://this-hostname-will-never-exist.trycloudflare.com",
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(result.is_err());
    }
}
