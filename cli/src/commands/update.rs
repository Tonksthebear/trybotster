//! Self-update functionality for botster.
//!
//! Provides commands to check for updates and automatically download/install
//! new versions from GitHub releases. Includes a boot-time update check that
//! runs on every startup and prompts the user to update interactively.
//!
//! # Security
//!
//! Downloads are verified using SHA256 checksums when available.
//!
//! # Examples
//!
//! ```bash
//! # Check if updates are available
//! botster update-check
//!
//! # Download and install the latest version
//! botster update
//! ```

use anyhow::Result;
use semver::Version;
use serde_json::Value;
use std::time::Duration;

/// The current version of botster, derived from Cargo.toml.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// GitHub API URL for fetching the latest release information.
const GITHUB_RELEASES_API: &str =
    "https://api.github.com/repos/Tonksthebear/trybotster/releases/latest";

/// Base URL for downloading release binaries.
const GITHUB_RELEASES_DOWNLOAD: &str =
    "https://github.com/Tonksthebear/trybotster/releases/download";

/// User-Agent header value for GitHub API requests.
const USER_AGENT: &str = "botster";

/// Timeout for the boot-time version fetch.
/// Must be long enough for cold DNS + TLS handshake to api.github.com.
const BOOT_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// Timeout for Lua-triggered version checks (`update.check()`).
/// Prevents blocking the hub event loop on slow/unreachable GitHub API.
const LUA_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// Result of checking for updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// A newer version is available.
    UpdateAvailable {
        /// Currently installed version.
        current: String,
        /// Latest available version.
        latest: String,
    },
    /// Already running the latest version.
    UpToDate {
        /// Current version string.
        version: String,
    },
    /// Running a version newer than the latest release.
    AheadOfRelease {
        /// Currently installed version.
        current: String,
        /// Latest release version.
        latest: String,
    },
}

/// Checks for updates at boot time and prompts the user to update if available.
///
/// This is designed to be called early in startup, before the TUI takes over.
/// Failures are logged at warn level — an update check must never block startup.
///
/// # Errors
///
/// Returns an error only if the update/exec-restart itself fails after the user
/// accepts. All other failures (network, parse) are logged and swallowed.
pub fn check_on_boot() -> Result<()> {
    if crate::env::is_test_mode() {
        return Ok(());
    }

    let latest_str = match fetch_latest_version_with_timeout(BOOT_CHECK_TIMEOUT) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("Boot update check failed: {e}");
            return Ok(());
        }
    };

    let current = match Version::parse(VERSION) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let latest = match Version::parse(&latest_str) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };

    if latest <= current {
        return Ok(());
    }

    println!("Update available: v{VERSION} -> v{latest_str}");

    if !atty::is(atty::Stream::Stdin) {
        log::warn!("Update available: v{VERSION} -> v{latest_str}");
        return Ok(());
    }

    use std::io::{self, Write};
    print!("Update now? [Y/n] ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_lowercase();

    if answer.is_empty() || answer == "y" || answer == "yes" {
        install()?;
        exec_restart()?;
    }

    Ok(())
}

/// Non-interactive variant for headless mode.
///
/// Logs a warning if an update is available but never prompts.
pub fn check_on_boot_headless() -> Result<()> {
    if crate::env::is_test_mode() {
        return Ok(());
    }

    let latest_str = match fetch_latest_version_with_timeout(BOOT_CHECK_TIMEOUT) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("Boot update check failed: {e}");
            return Ok(());
        }
    };

    let current = Version::parse(VERSION).ok();
    let latest = Version::parse(&latest_str).ok();

    if let (Some(cur), Some(lat)) = (current, latest) {
        if lat > cur {
            log::warn!("Update available: v{VERSION} -> v{latest_str}. Run 'botster update' to install.");
        }
    }

    Ok(())
}

/// Replaces the current process with the updated binary using the same arguments.
///
/// On success this function never returns — the current process image is replaced.
///
/// # Errors
///
/// Returns an error if `exec` fails (e.g., binary not found or permission denied).
pub fn exec_restart() -> Result<()> {
    let exe = std::env::current_exe()?;
    let args: Vec<String> = std::env::args().collect();

    println!("Restarting with updated binary...");

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe)
            .args(&args[1..])
            .exec();
        // exec() only returns on error
        anyhow::bail!("Failed to exec into updated binary: {err}");
    }

    #[cfg(not(unix))]
    {
        // Fallback for non-Unix: just tell the user to restart
        println!("Please restart botster to use the new version.");
        Ok(())
    }
}

/// Fetches the latest version string from GitHub with a custom timeout.
fn fetch_latest_version_with_timeout(timeout: Duration) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()?;

    let response = client
        .get(GITHUB_RELEASES_API)
        .header("User-Agent", USER_AGENT)
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("Failed to check for updates: {}", response.status());
    }

    let release: Value = response.json()?;
    let version = release["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid release data: missing tag_name"))?
        .trim_start_matches('v')
        .to_string();

    Ok(version)
}

/// Checks for available updates by querying the GitHub releases API.
///
/// Compares the current version against the latest release and reports
/// whether an update is available.
///
/// # Errors
///
/// Returns an error if:
/// - The GitHub API request fails
/// - The response cannot be parsed
/// - Version parsing fails
///
/// # Examples
///
/// ```ignore
/// update::check()?;
/// ```
pub fn check() -> Result<()> {
    let status = get_update_status()?;

    match status {
        UpdateStatus::UpdateAvailable { current, latest } => {
            println!("Current version: {}", current);
            println!("Latest version: {}", latest);
            println!("→ Update available! Run 'botster update' to install");
        }
        UpdateStatus::UpToDate { version } => {
            println!("Current version: {}", version);
            println!("✓ You are running the latest version");
        }
        UpdateStatus::AheadOfRelease { current, latest } => {
            println!("Current version: {}", current);
            println!("Latest version: {}", latest);
            println!("✓ You are running a newer version than the latest release");
        }
    }

    Ok(())
}

/// Gets the current update status without printing.
///
/// Useful for programmatic access to update information.
///
/// # Errors
///
/// Returns an error if:
/// - The GitHub API request fails
/// - The response cannot be parsed
/// - Version parsing fails
pub fn get_update_status() -> Result<UpdateStatus> {
    let latest_version_str = fetch_latest_version()?;

    let current = Version::parse(VERSION)?;
    let latest = Version::parse(&latest_version_str)?;

    match latest.cmp(&current) {
        std::cmp::Ordering::Greater => Ok(UpdateStatus::UpdateAvailable {
            current: VERSION.to_string(),
            latest: latest_version_str,
        }),
        std::cmp::Ordering::Equal => Ok(UpdateStatus::UpToDate {
            version: VERSION.to_string(),
        }),
        std::cmp::Ordering::Less => Ok(UpdateStatus::AheadOfRelease {
            current: VERSION.to_string(),
            latest: latest_version_str,
        }),
    }
}

/// Like [`get_update_status`] but with a bounded timeout.
///
/// Used by the Lua `update.check()` primitive to avoid blocking the hub
/// event loop indefinitely on slow or unreachable GitHub API responses.
pub fn get_update_status_with_timeout() -> Result<UpdateStatus> {
    let latest_version_str = fetch_latest_version_with_timeout(LUA_CHECK_TIMEOUT)?;

    let current = Version::parse(VERSION)?;
    let latest = Version::parse(&latest_version_str)?;

    match latest.cmp(&current) {
        std::cmp::Ordering::Greater => Ok(UpdateStatus::UpdateAvailable {
            current: VERSION.to_string(),
            latest: latest_version_str,
        }),
        std::cmp::Ordering::Equal => Ok(UpdateStatus::UpToDate {
            version: VERSION.to_string(),
        }),
        std::cmp::Ordering::Less => Ok(UpdateStatus::AheadOfRelease {
            current: VERSION.to_string(),
            latest: latest_version_str,
        }),
    }
}

/// Fetches the latest version string from GitHub.
fn fetch_latest_version() -> Result<String> {
    let client = reqwest::blocking::Client::new();
    let response = client
        .get(GITHUB_RELEASES_API)
        .header("User-Agent", USER_AGENT)
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("Failed to check for updates: {}", response.status());
    }

    let release: Value = response.json()?;
    let version = release["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid release data: missing tag_name"))?
        .trim_start_matches('v')
        .to_string();

    Ok(version)
}

/// Downloads and installs the latest version.
///
/// Performs the following steps:
/// 1. Checks if an update is available
/// 2. Determines the correct binary for the current platform
/// 3. Downloads the new binary
/// 4. Verifies the checksum (if available)
/// 5. Replaces the current binary with the new one
///
/// # Platform Support
///
/// Supported platforms:
/// - macOS ARM64 (Apple Silicon)
/// - macOS x86_64
/// - Linux x86_64
///
/// # Errors
///
/// Returns an error if:
/// - Already running the latest version
/// - Platform is not supported
/// - Download fails
/// - Checksum verification fails
/// - File operations fail
///
/// # Examples
///
/// ```ignore
/// update::install()?;
/// ```
pub fn install() -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::env;
    use std::fs;

    println!("Current version: {}", VERSION);
    println!("Checking for updates...");

    let latest_version_str = fetch_latest_version()?;
    println!("Latest version: {}", latest_version_str);

    let current = Version::parse(VERSION)?;
    let latest = Version::parse(&latest_version_str)?;

    if latest <= current {
        println!("✓ Already running the latest version (or newer)");
        return Ok(());
    }

    // Determine platform
    let platform = get_platform()?;
    let binary_name = format!("botster-{}", platform);
    let download_url = format!(
        "{}/v{}/{}",
        GITHUB_RELEASES_DOWNLOAD, latest_version_str, binary_name
    );
    let checksum_url = format!("{}.sha256", download_url);

    println!("Downloading version {}...", latest_version_str);

    let client = reqwest::blocking::Client::new();

    // Download binary
    let binary_response = client
        .get(&download_url)
        .header("User-Agent", USER_AGENT)
        .send()?;

    if !binary_response.status().is_success() {
        anyhow::bail!("Failed to download update: {}", binary_response.status());
    }

    let binary_data = binary_response.bytes()?;

    // Download and verify checksum
    let checksum_response = client
        .get(&checksum_url)
        .header("User-Agent", USER_AGENT)
        .send()?;

    if checksum_response.status().is_success() {
        let checksum_text = checksum_response.text()?;
        let expected_checksum = checksum_text
            .split_whitespace()
            .next()
            .ok_or_else(|| anyhow::anyhow!("Invalid checksum format"))?;

        // Verify checksum
        let mut hasher = Sha256::new();
        hasher.update(&binary_data);
        let actual_checksum = format!("{:x}", hasher.finalize());

        if actual_checksum != expected_checksum {
            anyhow::bail!("Checksum verification failed!");
        }
        println!("✓ Checksum verified");
    } else {
        log::warn!("Could not verify checksum (not found)");
    }

    // Get current binary path
    let current_exe = env::current_exe()?;

    // Write new binary to a temp file (use /tmp so it always succeeds regardless
    // of permissions on the install directory)
    let temp_path = std::env::temp_dir().join(format!("botster-update-{}", std::process::id()));
    fs::write(&temp_path, &binary_data)?;

    // Make it executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&temp_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&temp_path, perms)?;
    }

    // Replace current binary — try direct first, escalate to sudo if needed
    let replaced = replace_binary(&temp_path, &current_exe);

    // Clean up temp file if it still exists (sudo mv would have moved it)
    let _ = fs::remove_file(&temp_path);

    replaced?;

    println!("✓ Successfully updated to version {}", latest_version_str);

    Ok(())
}

/// Replaces the current binary with the new one, escalating to `sudo` if needed.
///
/// Tries a direct `fs::rename` first. If that fails with a permission error,
/// falls back to `sudo mv` so the user gets a password prompt on their terminal.
fn replace_binary(src: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    use std::fs;

    // Try direct rename first (works when user owns the install dir)
    match fs::rename(src, dest) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            log::debug!("Direct rename failed (permission denied), trying sudo");
        }
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            // Cross-device rename (e.g. /tmp -> /usr/local/bin on different filesystems)
            log::debug!("Direct rename failed (cross-device), trying copy then sudo");
        }
        Err(e) => return Err(e.into()),
    }

    // Fall back to sudo — this passes the password prompt through to the terminal
    println!("Installing to {} requires elevated permissions.", dest.display());
    let status = std::process::Command::new("sudo")
        .arg("mv")
        .arg(src)
        .arg(dest)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()?;

    if !status.success() {
        anyhow::bail!(
            "Failed to install update to {} (sudo exited with {})",
            dest.display(),
            status
        );
    }

    Ok(())
}

/// Determines the platform identifier for downloads.
///
/// Returns a platform string matching the release binary naming convention.
fn get_platform() -> Result<&'static str> {
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        Ok("macos-arm64")
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        Ok("macos-x86_64")
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        Ok("linux-x86_64")
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "aarch64") {
        Ok("linux-arm64")
    } else {
        anyhow::bail!("Unsupported platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_is_valid_semver() {
        let result = Version::parse(VERSION);
        assert!(result.is_ok(), "VERSION should be valid semver");
    }

    #[test]
    fn test_update_status_equality() {
        let status1 = UpdateStatus::UpToDate {
            version: "1.0.0".to_string(),
        };
        let status2 = UpdateStatus::UpToDate {
            version: "1.0.0".to_string(),
        };
        assert_eq!(status1, status2);
    }

    #[test]
    fn test_update_status_variants() {
        let available = UpdateStatus::UpdateAvailable {
            current: "1.0.0".to_string(),
            latest: "1.1.0".to_string(),
        };
        let up_to_date = UpdateStatus::UpToDate {
            version: "1.0.0".to_string(),
        };
        let ahead = UpdateStatus::AheadOfRelease {
            current: "1.1.0".to_string(),
            latest: "1.0.0".to_string(),
        };

        // Ensure different variants are not equal
        assert_ne!(available, up_to_date);
        assert_ne!(up_to_date, ahead);
        assert_ne!(available, ahead);
    }

    #[test]
    fn test_get_platform_returns_valid_value() {
        // This test should pass on any supported platform
        let result = get_platform();

        // If we're on a supported platform, it should succeed
        if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
            assert!(result.is_ok());
            let platform = result.unwrap();
            assert!(
                platform.starts_with("macos-") || platform.starts_with("linux-"),
                "Platform should start with os name"
            );
        }
        // On unsupported platforms, it should fail
    }

}
