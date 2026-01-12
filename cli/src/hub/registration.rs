//! Hub registration and connection management.
//!
//! This module handles device/hub registration with the Rails server
//! and WebSocket connection setup for browser relay.
//!
//! # Responsibilities
//!
//! - Device identity registration (E2E encryption keypairs)
//! - Hub registration for message routing
//! - Tunnel connection for HTTP forwarding
//! - Terminal relay connection for browser access

// Rust guideline compliant 2025-01

use std::sync::Arc;

use reqwest::blocking::Client;

use crate::config::Config;
use crate::device::Device;
use crate::relay::{connection::TerminalRelay, signal::SignalProtocolManager, BrowserState};
use crate::tunnel::TunnelManager;

/// Register the device with the server if not already registered.
///
/// This should be called after Hub creation to ensure the device identity
/// is known to the server for browser-based key exchange.
pub fn register_device(
    device: &mut Device,
    client: &Client,
    config: &Config,
) {
    if device.device_id.is_some() {
        return;
    }

    match device.register(
        client,
        &config.server_url,
        config.get_api_key(),
    ) {
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
    // Detect repo: env var > git detection > test fallback > error
    let repo_name = std::env::var("BOTSTER_REPO").ok()
        .or_else(|| {
            crate::git::WorktreeManager::detect_current_repo()
                .map(|(_, name)| name)
                .ok()
        })
        .unwrap_or_else(|| {
            if crate::env::is_test_mode() {
                "test/repo".to_string()
            } else {
                log::error!("Not in a git repository. Run from a git repo or set BOTSTER_REPO env var.");
                String::new() // Will fail validation on server
            }
        });

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
pub fn start_tunnel(
    tunnel_manager: &Arc<TunnelManager>,
    runtime: &tokio::runtime::Runtime,
) {
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

/// Connect to the terminal relay for browser access.
///
/// This establishes an Action Cable WebSocket connection with E2E encryption
/// using Signal Protocol for secure browser-based terminal access.
///
/// The relay runs on a dedicated thread with its own LocalSet because
/// Signal Protocol uses non-Send futures that require spawn_local.
///
/// # Returns
///
/// Returns `Ok(())` if connection succeeds and the browser state is updated,
/// or logs a warning and continues if connection fails.
pub fn connect_terminal_relay(
    browser: &mut BrowserState,
    hub_identifier: &str,
    server_url: &str,
    api_key: &str,
    _runtime: &tokio::runtime::Runtime,
) {
    use std::sync::mpsc as std_mpsc;
    use tokio::sync::mpsc;

    let hub_id = hub_identifier.to_string();
    let server = server_url.to_string();
    let key = api_key.to_string();

    // Channels for cross-thread communication
    let (bundle_tx, bundle_rx) = std_mpsc::channel();
    let (sender_tx, sender_rx) = std_mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel(100);

    // Spawn dedicated thread for relay (Signal Protocol needs LocalSet)
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create relay runtime");

        let local = tokio::task::LocalSet::new();

        local.block_on(&rt, async {
            // Load or create Signal Protocol manager
            let signal_manager = match SignalProtocolManager::load_or_create(&hub_id).await {
                Ok(manager) => manager,
                Err(e) => {
                    log::error!("Failed to load/create Signal Protocol manager: {e}");
                    let _ = bundle_tx.send(None);
                    return;
                }
            };

            // Build PreKeyBundle data for QR code
            let bundle = match signal_manager.build_prekey_bundle_data(1).await {
                Ok(bundle) => bundle,
                Err(e) => {
                    log::error!("Failed to build PreKeyBundle: {e}");
                    let _ = bundle_tx.send(None);
                    return;
                }
            };

            log::info!(
                "Signal Protocol ready: identity={:.8}...",
                bundle.identity_key
            );

            // Send bundle back to main thread
            let _ = bundle_tx.send(Some(bundle));

            let relay = TerminalRelay::new(
                signal_manager,
                hub_id.clone(),
                server,
                key,
            );

            match relay.connect_with_event_channel(event_tx).await {
                Ok(sender) => {
                    log::info!("Connected to terminal relay for E2E encrypted browser access");
                    let _ = sender_tx.send(Some(sender));

                    // Keep the LocalSet running forever to process messages
                    // The spawned tasks inside connect_with_event_channel will handle messages
                    loop {
                        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                    }
                }
                Err(e) => {
                    log::warn!("Failed to connect to terminal relay: {e} - browser access disabled");
                    let _ = sender_tx.send(None);
                }
            }
        });
    });

    // Wait for bundle from relay thread
    match bundle_rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Some(bundle)) => {
            // Build connection URL and write to file for external access (testing/automation)
            use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
            use crate::relay::write_connection_url;

            if let Ok(json) = serde_json::to_string(&bundle) {
                let encoded = URL_SAFE_NO_PAD.encode(json.as_bytes());
                let connection_url = format!(
                    "{}/hubs/{}#bundle={}",
                    server_url, hub_identifier, encoded
                );

                if let Err(e) = write_connection_url(hub_identifier, &connection_url) {
                    log::warn!("Failed to write connection URL: {e}");
                } else {
                    log::info!("Connection URL available for external access");
                }
            }

            browser.signal_bundle = Some(bundle);
        }
        Ok(None) => {
            log::error!("Relay thread failed to create bundle");
            return;
        }
        Err(_) => {
            log::error!("Timeout waiting for Signal bundle");
            return;
        }
    }

    // Wait for sender from relay thread
    match sender_rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Some(sender)) => {
            browser.sender = Some(sender);
            browser.event_rx = Some(event_rx);
        }
        Ok(None) => {
            log::warn!("Relay connection failed");
        }
        Err(_) => {
            log::error!("Timeout waiting for relay connection");
        }
    }
}

/// Send shutdown notification to server.
///
/// Call this when the hub is shutting down to unregister from the server.
pub fn shutdown(
    client: &Client,
    server_url: &str,
    hub_identifier: &str,
    api_key: &str,
) {
    log::info!("Sending shutdown notification to server...");
    let shutdown_url = format!("{server_url}/hubs/{hub_identifier}");

    match client
        .delete(&shutdown_url)
        .bearer_auth(api_key)
        .send()
    {
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
            headscale_url: None,
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
