//! Hub registration and connection management.
//!
//! This module handles device/hub registration with the Rails server
//! and Tailscale connection setup for browser access.
//!
//! # Responsibilities
//!
//! - Device identity registration
//! - Hub registration for message routing
//! - Tunnel connection for HTTP forwarding
//! - Tailscale connection for browser access

// Rust guideline compliant 2025-01

use std::sync::Arc;

use reqwest::blocking::Client;

use crate::browser_connect::BrowserConnector;
use crate::config::Config;
use crate::device::Device;
use crate::relay::BrowserState;
use crate::tunnel::TunnelManager;

/// Register the device with the server if not already registered.
///
/// This should be called after Hub creation to ensure the device identity
/// is known to the server for browser-based key exchange.
pub fn register_device(device: &mut Device, client: &Client, config: &Config) {
    if device.device_id.is_some() {
        return;
    }

    match device.register(client, &config.server_url, config.get_api_key()) {
        Ok(id) => log::info!("Device registered with server: id={id}"),
        Err(e) => log::warn!("Device registration failed: {e} - will retry later"),
    }
}

/// Register the hub with the server before connecting to channels.
///
/// This creates the Hub record on the server so that the terminal
/// relay channel can find it when the CLI subscribes.
pub fn register_hub_with_server(
    hub_identifier: &str,
    server_url: &str,
    api_key: &str,
    device_id: Option<i64>,
) {
    let repo_name = crate::git::WorktreeManager::detect_current_repo()
        .map(|(_, name)| name)
        .unwrap_or_default();

    let url = format!("{server_url}/hubs/{hub_identifier}");
    let payload = serde_json::json!({
        "repo": repo_name,
        "agents": [],
        "device_id": device_id,
    });

    log::info!("Registering hub with server before channel connections...");
    match reqwest::blocking::Client::new()
        .put(&url)
        .header("Content-Type", "application/json")
        .header("X-Hub-Identifier", hub_identifier)
        .bearer_auth(api_key)
        .json(&payload)
        .send()
    {
        Ok(response) if response.status().is_success() => {
            log::info!("Hub registered successfully");
        }
        Ok(response) => {
            log::warn!("Hub registration returned status: {}", response.status());
        }
        Err(e) => {
            log::warn!("Failed to register hub: {e} - channels may not work");
        }
    }
}

/// Start the tunnel connection in background.
///
/// The tunnel provides HTTP forwarding for agent dev servers.
pub fn start_tunnel(tunnel_manager: &Arc<TunnelManager>, runtime: &tokio::runtime::Runtime) {
    let tm = Arc::clone(tunnel_manager);
    runtime.spawn(async move {
        loop {
            if let Err(e) = tm.connect().await {
                log::warn!("Tunnel connection error: {e}, reconnecting in 5s...");
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
    });
}

/// Connect to the Tailscale tailnet for browser access.
///
/// This connects the CLI to the user's Headscale-managed tailnet and
/// generates the connection URL for browser QR code scanning.
///
/// # Security
///
/// - CLI joins tailnet using pre-auth key from Rails
/// - Browser pre-auth key is placed in URL fragment (server never sees it)
/// - All traffic E2E encrypted via WireGuard
pub fn connect_tailscale(
    browser: &mut BrowserState,
    hub_identifier: &str,
    server_url: &str,
    api_key: &str,
    headscale_url: Option<&str>,
    runtime: &tokio::runtime::Runtime,
) {
    log::info!("Connecting to Tailscale tailnet for browser access...");

    let mut connector = BrowserConnector::new(hub_identifier, server_url, api_key, headscale_url);

    // Connect to tailnet in blocking fashion
    match runtime.block_on(connector.connect_to_tailnet()) {
        Ok(()) => {
            log::info!("Connected to tailnet");
        }
        Err(e) => {
            log::warn!("Failed to connect to tailnet: {e} - browser access disabled");
            return;
        }
    }

    // Get connection info for QR code
    match runtime.block_on(connector.get_connection_info()) {
        Ok(info) => {
            log::info!(
                "Tailscale connection ready: hostname={}",
                info.hostname
            );
            browser.set_tailscale_info(info.connection_url, info.hostname);
        }
        Err(e) => {
            log::warn!("Failed to get connection info: {e}");
        }
    }
}

/// Send shutdown notification to server.
///
/// Call this when the hub is shutting down to unregister from the server.
pub fn shutdown(client: &Client, server_url: &str, hub_identifier: &str, api_key: &str) {
    log::info!("Sending shutdown notification to server...");
    let shutdown_url = format!("{server_url}/hubs/{hub_identifier}");

    match client.delete(&shutdown_url).bearer_auth(api_key).send() {
        Ok(response) if response.status().is_success() => {
            log::info!("Hub unregistered from server");
        }
        Ok(response) => {
            log::warn!("Failed to unregister hub: {}", response.status());
        }
        Err(e) => {
            log::warn!("Failed to send shutdown notification: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires keyring access - run manually"]
    fn test_register_device_skips_if_already_registered() {
        // This test verifies the early return path
        let mut device = Device::load_or_create().expect("Device creation failed");
        device.device_id = Some(123);

        // Should not panic or make network calls
        let client = Client::new();
        let config = Config {
            server_url: "http://localhost:3000".to_string(),
            token: "test".to_string(),
            api_key: String::new(),
            poll_interval: 10,
            agent_timeout: 300,
            max_sessions: 10,
            worktree_base: std::path::PathBuf::from("/tmp"),
        };

        register_device(&mut device, &client, &config);
        // Success = no panic
    }
}
