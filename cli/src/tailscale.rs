//! Tailscale integration for secure browser connectivity.
//!
//! This module wraps the Tailscale CLI to connect to the user's
//! Headscale-managed tailnet for E2E encrypted terminal access.
//!
//! # Architecture
//!
//! - Each user has an isolated Headscale namespace (tailnet)
//! - CLI joins the tailnet using a pre-auth key from Rails
//! - Browser connects via tsconnect WASM with ephemeral key
//! - Direct mesh connectivity (P2P when possible, DERP relay fallback)
//!
//! # Security
//!
//! - Pre-auth keys exchanged via URL fragment (server never sees them)
//! - Per-user namespace isolation enforced at Headscale infrastructure level
//! - SSH authentication via tailnet membership (no Unix users)
//!
//! # Embedded Binary
//!
//! The Tailscale binary is embedded at compile time and extracted to
//! `~/.botster_hub/bin/tailscale` on first use. This ensures zero external
//! dependencies - users just need the botster-hub binary.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use tokio::time::sleep;

use crate::embedded_tailscale;

/// Get the path to the embedded Tailscale binary.
///
/// Extracts the binary on first use if needed.
fn get_tailscale_binary() -> Result<PathBuf> {
    // Check that the embedded binary is valid (not a placeholder)
    if !embedded_tailscale::is_binary_valid() {
        bail!(
            "Tailscale binary not available. The build may have failed to download Tailscale. \
             Please rebuild with internet access or set TAILSCALE_BINARY_PATH to a valid binary."
        );
    }

    embedded_tailscale::get_tailscale_binary_path()
}

/// Default Headscale control server URL for local development.
const DEFAULT_HEADSCALE_URL: &str = "http://localhost:8080";

/// Tailscale client for managing mesh connectivity.
#[derive(Debug)]
pub struct TailscaleClient {
    /// Headscale control server URL.
    control_url: String,
    /// Hub identifier (used for hostname).
    hub_id: String,
    /// Whether we've successfully connected.
    connected: bool,
}

impl TailscaleClient {
    /// Create a new Tailscale client.
    ///
    /// # Arguments
    ///
    /// * `hub_id` - The hub identifier (used for tailnet hostname)
    /// * `headscale_url` - Optional Headscale control server URL (defaults to localhost:8080)
    pub fn new(hub_id: &str, headscale_url: Option<&str>) -> Self {
        Self {
            control_url: headscale_url
                .unwrap_or(DEFAULT_HEADSCALE_URL)
                .to_string(),
            hub_id: hub_id.to_string(),
            connected: false,
        }
    }

    /// Connect to the tailnet using a pre-auth key.
    ///
    /// This shells out to `tailscale up` with the appropriate flags:
    /// - `--login-server` points to our Headscale instance
    /// - `--authkey` is the pre-auth key from Rails
    /// - `--ssh` enables Tailscale SSH (browser connects via SSH)
    /// - `--hostname` sets a recognizable name in the tailnet
    ///
    /// # Arguments
    ///
    /// * `preauth_key` - Pre-auth key from Rails for joining the tailnet
    pub async fn up(&mut self, preauth_key: &str) -> Result<()> {
        let hostname = format!("cli-{}", &self.hub_id);

        log::info!(
            "Connecting to tailnet via {} as {}",
            self.control_url,
            hostname
        );

        let tailscale_bin = get_tailscale_binary()?;
        log::debug!("Using tailscale binary at: {}", tailscale_bin.display());

        let output = Command::new(&tailscale_bin)
            .args([
                "up",
                "--login-server",
                &self.control_url,
                "--authkey",
                preauth_key,
                "--ssh",
                "--hostname",
                &hostname,
                "--accept-routes",
                "--reset",
            ])
            .output()
            .context("Failed to execute tailscale up")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tailscale up failed: {}", stderr);
        }

        // Wait for connection to establish
        self.wait_for_connection().await?;
        self.connected = true;

        log::info!("Connected to tailnet as {}", self.hostname()?);
        Ok(())
    }

    /// Disconnect from the tailnet.
    pub fn down(&mut self) -> Result<()> {
        log::info!("Disconnecting from tailnet");

        let tailscale_bin = get_tailscale_binary()?;
        let output = Command::new(&tailscale_bin)
            .args(["down"])
            .output()
            .context("Failed to execute tailscale down")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!("tailscale down failed: {}", stderr);
        }

        self.connected = false;
        Ok(())
    }

    /// Get the current tailnet hostname.
    ///
    /// Returns the FQDN that browsers can use to connect via SSH.
    pub fn hostname(&self) -> Result<String> {
        let tailscale_bin = get_tailscale_binary()?;
        let output = Command::new(&tailscale_bin)
            .args(["status", "--json"])
            .output()
            .context("Failed to execute tailscale status")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tailscale status failed: {}", stderr);
        }

        let status: serde_json::Value =
            serde_json::from_slice(&output.stdout).context("Failed to parse tailscale status")?;

        // Extract our hostname from the Self entry
        let self_entry = status
            .get("Self")
            .context("Missing Self in tailscale status")?;

        let dns_name = self_entry
            .get("DNSName")
            .and_then(|v| v.as_str())
            .context("Missing DNSName in tailscale status")?;

        // Remove trailing dot if present
        Ok(dns_name.trim_end_matches('.').to_string())
    }

    /// Check if currently connected to the tailnet.
    pub fn is_connected(&self) -> bool {
        if !self.connected {
            return false;
        }

        // Verify with tailscale status
        self.check_connection_status().unwrap_or(false)
    }

    /// Get the tailnet IP address.
    pub fn ip(&self) -> Result<String> {
        let tailscale_bin = get_tailscale_binary()?;
        let output = Command::new(&tailscale_bin)
            .args(["ip", "-4"])
            .output()
            .context("Failed to execute tailscale ip")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("tailscale ip failed: {}", stderr);
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Wait for the tailnet connection to be established.
    async fn wait_for_connection(&self) -> Result<()> {
        let max_attempts = 30;
        let poll_interval = Duration::from_millis(500);

        for attempt in 1..=max_attempts {
            if self.check_connection_status()? {
                return Ok(());
            }

            log::debug!("Waiting for tailnet connection... (attempt {}/{})", attempt, max_attempts);
            sleep(poll_interval).await;
        }

        bail!("Timed out waiting for tailnet connection")
    }

    /// Check if tailscale reports as connected.
    fn check_connection_status(&self) -> Result<bool> {
        let tailscale_bin = get_tailscale_binary()?;
        let output = Command::new(&tailscale_bin)
            .args(["status", "--json"])
            .output()
            .context("Failed to execute tailscale status")?;

        if !output.status.success() {
            return Ok(false);
        }

        let status: serde_json::Value =
            serde_json::from_slice(&output.stdout).context("Failed to parse tailscale status")?;

        // Check BackendState
        let backend_state = status
            .get("BackendState")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        Ok(backend_state == "Running")
    }
}

// Note: No Drop impl - we don't auto-disconnect because:
// 1. The connection should persist for the hub's lifetime
// 2. Dropping from async context causes tokio runtime panics
// Call down() explicitly when shutting down.

/// Check if the Tailscale daemon is running.
///
/// Uses the embedded binary to check status.
pub fn is_tailscaled_running() -> bool {
    let tailscale_bin = match get_tailscale_binary() {
        Ok(bin) => bin,
        Err(_) => return false,
    };

    Command::new(&tailscale_bin)
        .args(["status"])
        .output()
        .map(|o| o.status.success() || !String::from_utf8_lossy(&o.stderr).contains("not running"))
        .unwrap_or(false)
}

/// Get the path to the embedded tailscale binary.
///
/// Extracts the binary on first use if needed.
/// Returns `None` if the binary is not available (placeholder from failed build).
pub fn find_tailscale_binary() -> Option<String> {
    get_tailscale_binary()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// Get information about the embedded Tailscale binary.
pub fn get_embedded_binary_info() -> embedded_tailscale::BinaryInfo {
    embedded_tailscale::get_binary_info()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_with_default_url() {
        let client = TailscaleClient::new("hub123", None);
        assert_eq!(client.control_url, DEFAULT_HEADSCALE_URL);
        assert_eq!(client.hub_id, "hub123");
        assert!(!client.connected);
    }

    #[test]
    fn test_new_with_custom_url() {
        let client = TailscaleClient::new("hub456", Some("https://headscale.example.com"));
        assert_eq!(client.control_url, "https://headscale.example.com");
    }
}
