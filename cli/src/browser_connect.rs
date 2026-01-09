//! Browser connection via Tailscale mesh networking.
//!
//! This module handles the setup flow for browser-to-CLI connectivity:
//!
//! 1. CLI connects to tailnet using pre-auth key from Rails
//! 2. CLI requests ephemeral browser key from Rails
//! 3. CLI generates connection URL with browser key in fragment
//! 4. Browser scans QR code, joins tailnet via tsconnect WASM
//! 5. Browser connects to CLI via Tailscale SSH
//!
//! # Security Properties
//!
//! - **Zero-knowledge key exchange**: Browser pre-auth key is in URL fragment
//!   (`#key=xxx`), which is never sent to the server per HTTP spec
//! - **Per-user isolation**: Each user has isolated Headscale namespace
//! - **E2E encryption**: WireGuard encrypts all traffic between browser and CLI
//! - **P2P when possible**: DERP relay only used when NAT prevents direct connection

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::tailscale::TailscaleClient;

/// Connection info for browser to connect via Tailscale.
#[derive(Debug, Clone)]
pub struct BrowserConnectionInfo {
    /// Full URL for browser to connect (includes fragment with key).
    pub connection_url: String,
    /// CLI's tailnet hostname (for display).
    pub hostname: String,
    /// Whether CLI is connected to tailnet.
    pub connected: bool,
}

/// Response from Rails /hubs/:id/tailscale/browser_key endpoint.
#[derive(Debug, Deserialize)]
struct BrowserKeyResponse {
    key: String,
}

/// Response from Rails /hubs/:id endpoint with tailscale_preauth_key.
#[derive(Debug, Deserialize)]
struct HubResponse {
    tailscale_preauth_key: Option<String>,
}

/// Manager for browser connection via Tailscale.
///
/// Handles the lifecycle of Tailscale connectivity for browser access:
/// - Connecting CLI to tailnet
/// - Fetching browser ephemeral keys
/// - Generating connection URLs
#[derive(Debug)]
pub struct BrowserConnector {
    /// Tailscale client for mesh connectivity.
    tailscale: TailscaleClient,
    /// HTTP client for Rails API calls (async).
    http_client: reqwest::Client,
    /// Rails server URL.
    server_url: String,
    /// API key for Rails authentication.
    api_key: String,
    /// Hub identifier.
    hub_identifier: String,
    /// Cached browser pre-auth key (refreshed periodically).
    browser_key: Option<String>,
}

impl BrowserConnector {
    /// Create a new browser connector.
    ///
    /// # Arguments
    ///
    /// * `hub_identifier` - The hub identifier
    /// * `server_url` - Rails server URL
    /// * `api_key` - API key for Rails authentication
    /// * `headscale_url` - Optional Headscale control server URL
    pub fn new(
        hub_identifier: &str,
        server_url: &str,
        api_key: &str,
        headscale_url: Option<&str>,
    ) -> Self {
        Self {
            tailscale: TailscaleClient::new(hub_identifier, headscale_url),
            http_client: reqwest::Client::new(),
            server_url: server_url.to_string(),
            api_key: api_key.to_string(),
            hub_identifier: hub_identifier.to_string(),
            browser_key: None,
        }
    }

    /// Connect CLI to the tailnet.
    ///
    /// This fetches the pre-auth key from Rails and joins the user's tailnet.
    /// Must be called before generating connection URLs.
    pub async fn connect_to_tailnet(&mut self) -> Result<()> {
        // Fetch CLI's pre-auth key from Rails
        let preauth_key = self.fetch_cli_preauth_key().await?;

        // Join the tailnet
        self.tailscale.up(&preauth_key).await?;

        // Report hostname back to Rails
        self.report_hostname().await?;

        log::info!("CLI connected to tailnet, ready for browser connections");
        Ok(())
    }

    /// Get connection info for browser.
    ///
    /// Returns the URL that should be displayed as a QR code.
    /// The browser pre-auth key is in the URL fragment so the server never sees it.
    pub async fn get_connection_info(&mut self) -> Result<BrowserConnectionInfo> {
        // Ensure we're connected
        if !self.tailscale.is_connected() {
            bail!("CLI not connected to tailnet");
        }

        // Fetch ephemeral browser key if we don't have one
        if self.browser_key.is_none() {
            self.refresh_browser_key().await?;
        }

        let hostname = self.tailscale.hostname()?;
        let browser_key = self
            .browser_key
            .as_ref()
            .context("No browser key available")?;

        // Build connection URL with key in fragment
        // Fragment is never sent to server per HTTP spec
        let connection_url = format!(
            "{}/hubs/{}#key={}",
            self.server_url, self.hub_identifier, browser_key
        );

        Ok(BrowserConnectionInfo {
            connection_url,
            hostname,
            connected: true,
        })
    }

    /// Refresh the ephemeral browser key.
    ///
    /// Call this periodically (e.g., every 30 minutes) since ephemeral keys expire.
    pub async fn refresh_browser_key(&mut self) -> Result<()> {
        let url = format!(
            "{}/hubs/{}/tailscale/browser_key",
            self.server_url, self.hub_identifier
        );

        let response = self
            .http_client
            .post(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await
            .context("Failed to request browser key")?;

        if !response.status().is_success() {
            bail!("Failed to get browser key: {}", response.status());
        }

        let key_response: BrowserKeyResponse = response
            .json()
            .await
            .context("Failed to parse browser key response")?;

        self.browser_key = Some(key_response.key);
        log::info!("Refreshed ephemeral browser key");
        Ok(())
    }

    /// Check if CLI is connected to tailnet.
    pub fn is_connected(&self) -> bool {
        self.tailscale.is_connected()
    }

    /// Get the CLI's tailnet hostname.
    pub fn hostname(&self) -> Result<String> {
        self.tailscale.hostname()
    }

    /// Disconnect from tailnet.
    pub fn disconnect(&mut self) -> Result<()> {
        self.tailscale.down()
    }

    /// Fetch CLI's pre-auth key from Rails.
    ///
    /// The pre-auth key is stored on the Hub record when the hub is created.
    async fn fetch_cli_preauth_key(&self) -> Result<String> {
        let url = format!("{}/hubs/{}", self.server_url, self.hub_identifier);

        let response = self
            .http_client
            .get(&url)
            .bearer_auth(&self.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Failed to fetch hub details")?;

        if !response.status().is_success() {
            bail!("Failed to get hub details: {}", response.status());
        }

        let hub: HubResponse = response
            .json()
            .await
            .context("Failed to parse hub response")?;

        hub.tailscale_preauth_key
            .context("Hub has no Tailscale pre-auth key")
    }

    /// Report CLI's tailnet hostname back to Rails.
    ///
    /// This allows Rails to know how to reach the CLI within the tailnet.
    async fn report_hostname(&self) -> Result<()> {
        let hostname = self.tailscale.hostname()?;
        let url = format!(
            "{}/hubs/{}/tailscale/hostname",
            self.server_url, self.hub_identifier
        );

        let response = self
            .http_client
            .patch(&url)
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({ "hostname": hostname }))
            .send()
            .await
            .context("Failed to report hostname")?;

        if !response.status().is_success() {
            log::warn!("Failed to report hostname to Rails: {}", response.status());
        }

        Ok(())
    }
}

/// Generate QR code data for browser connection.
///
/// This creates a compact URL that includes:
/// - Hub page URL
/// - Browser pre-auth key in fragment (never sent to server)
///
/// # Example URL
///
/// ```text
/// https://trybotster.com/hubs/abc123#key=hskey_xxxxx
/// ```
pub fn generate_connection_url(server_url: &str, hub_identifier: &str, browser_key: &str) -> String {
    format!("{}hubs/{}#key={}", server_url, hub_identifier, browser_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_connection_url() {
        let url = generate_connection_url(
            "https://trybotster.com/",
            "hub123",
            "hskey_browser_abc",
        );
        assert_eq!(
            url,
            "https://trybotster.com/hubs/hub123#key=hskey_browser_abc"
        );
    }
}
