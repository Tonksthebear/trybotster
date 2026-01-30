//! Hub registration and connection management.
//!
//! This module handles device/hub registration with the Rails server
//! and WebSocket connection setup for browser relay.
//!
//! # Responsibilities
//!
//! - Device identity registration (E2E encryption keypairs)
//! - Hub registration for message routing
//! - Signal Protocol initialization for E2E encryption (lazy bundle generation)
//!
//! # Lazy Bundle Generation
//!
//! To avoid blocking boot for up to 10 seconds, Signal Protocol bundle generation
//! is deferred until the connection URL is first requested. The flow is:
//!
//! 1. `init_signal_protocol()` - starts CryptoService only (fast)
//! 2. `get_or_generate_connection_bundle()` - generates bundle on first request
//! 3. `write_connection_url_lazy()` - generates bundle + writes URL to disk
//!
//! Bundle generation is triggered by:
//! - TUI QR code display request (`GetConnectionCode` command)
//! - External automation requesting the connection URL

// Rust guideline compliant 2026-01-29


use reqwest::blocking::Client;

use crate::config::Config;
use crate::device::Device;
use crate::relay::{BrowserState, CryptoService};

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

/// Register the hub with the server and get the Rails-assigned ID.
///
/// This creates the Hub record on the server and returns the database ID
/// which should be used for all subsequent URLs and WebSocket subscriptions
/// to guarantee uniqueness.
///
/// # Returns
///
/// The Rails-assigned hub ID as a string, or the local identifier if
/// registration fails (for offline/degraded mode).
pub fn register_hub_with_server(
    local_identifier: &str,
    server_url: &str,
    api_key: &str,
    device_id: Option<i64>,
) -> String {
    // Detect repo: env var > git detection > test fallback > error
    let repo_name = std::env::var("BOTSTER_REPO")
        .ok()
        .or_else(|| {
            crate::git::WorktreeManager::detect_current_repo()
                .map(|(_, name)| name)
                .ok()
        })
        .unwrap_or_else(|| {
            if crate::env::is_any_test() {
                "test/repo".to_string()
            } else {
                log::error!(
                    "Not in a git repository. Run from a git repo or set BOTSTER_REPO env var."
                );
                String::new() // Will fail validation on server
            }
        });

    // POST /hubs to register and get server-assigned ID
    let url = format!("{server_url}/hubs");
    let payload = serde_json::json!({
        "identifier": local_identifier,
        "repo": repo_name,
        "device_id": device_id,
    });

    log::info!("Registering hub with server to get Botster ID...");
    match reqwest::blocking::Client::new()
        .post(&url)
        .header("Content-Type", "application/json")
        .bearer_auth(api_key)
        .json(&payload)
        .send()
    {
        Ok(response) if response.status().is_success() => {
            // Parse response to get server-assigned ID
            match response.json::<serde_json::Value>() {
                Ok(json) => {
                    if let Some(id) = json.get("id").and_then(|v| v.as_i64()) {
                        let botster_id = id.to_string();
                        log::info!("Hub registered with Botster ID: {botster_id}");
                        return botster_id;
                    }
                    log::warn!("Response missing 'id' field, using local identifier");
                }
                Err(e) => {
                    log::warn!("Failed to parse registration response: {e}");
                }
            }
        }
        Ok(response) => {
            log::warn!("Hub registration returned status: {}", response.status());
        }
        Err(e) => {
            log::warn!("Failed to register hub: {e} - using local identifier");
        }
    }

    // Fallback to local identifier if registration fails
    log::info!("Using local identifier as fallback: {local_identifier}");
    local_identifier.to_string()
}

/// Initialize Signal Protocol CryptoService for E2E encryption.
///
/// This starts the CryptoService only. PreKeyBundle generation is deferred until
/// the connection URL is first requested (lazy initialization). This avoids
/// blocking boot for up to 10 seconds on bundle generation.
///
/// Browser command handling is done by BrowserClient (via HubChannel subscription).
/// Hub-level events are handled by HubCommandChannel.
///
/// # Usage
///
/// After calling this, use `get_or_generate_connection_bundle()` to lazily
/// generate the bundle when the TUI displays the QR code or external automation
/// requests the connection URL.
pub fn init_signal_protocol(browser: &mut BrowserState, local_identifier: &str) {
    // Start CryptoService - runs Signal Protocol in its own LocalSet thread
    // The handle is Send + Clone and can be used from any thread
    let crypto_service = match CryptoService::start(local_identifier) {
        Ok(handle) => handle,
        Err(e) => {
            log::error!("Failed to start crypto service: {e}");
            return;
        }
    };

    // Store the crypto service handle for agent channel encryption
    browser.crypto_service = Some(crypto_service);

    // Mark relay as ready to connect (bundle generated lazily on first request)
    browser.relay_connected = true;
    log::info!("CryptoService started - bundle will be generated on first request");
}

/// Generate the PreKeyBundle lazily on first request.
///
/// This is called when the connection URL is first needed (TUI QR display,
/// external automation, etc.). The bundle is cached in `BrowserState` for
/// subsequent requests.
///
/// # Returns
///
/// The generated `PreKeyBundleData`, or an error if generation fails.
///
/// # Errors
///
/// Returns an error if:
/// - CryptoService is not initialized
/// - Bundle generation fails
pub fn get_or_generate_connection_bundle(
    browser: &mut BrowserState,
    runtime: &tokio::runtime::Runtime,
) -> Result<crate::relay::PreKeyBundleData, String> {
    // Return cached bundle if available and not used
    if let Some(ref bundle) = browser.signal_bundle {
        if !browser.bundle_used {
            return Ok(bundle.clone());
        }
        // Bundle was used, need to regenerate
        log::info!("Previous bundle was used, generating fresh bundle");
    }

    // Get crypto service
    let crypto_service = browser
        .crypto_service
        .clone()
        .ok_or_else(|| "CryptoService not initialized".to_string())?;

    // Generate bundle (blocking call via runtime)
    let bundle = runtime.block_on(async {
        let next_id = crypto_service.next_prekey_id().await.unwrap_or(1);
        crypto_service
            .get_prekey_bundle(next_id)
            .await
            .map_err(|e| format!("Failed to generate PreKeyBundle: {e}"))
    })?;

    log::info!(
        "Signal Protocol bundle generated: identity={:.8}...",
        bundle.identity_key
    );

    // Cache the bundle
    browser.signal_bundle = Some(bundle.clone());
    browser.bundle_used = false;

    Ok(bundle)
}

/// Build and write the connection URL for external access.
///
/// This generates the bundle if needed and writes the URL to disk for external
/// tools (test harnesses, automation).
///
/// # Returns
///
/// The connection URL string, or an error if generation/writing fails.
///
/// # Errors
///
/// Returns an error if bundle generation or file writing fails.
pub fn write_connection_url_lazy(
    browser: &mut BrowserState,
    runtime: &tokio::runtime::Runtime,
    server_hub_id: &str,
    local_identifier: &str,
    server_url: &str,
) -> Result<String, String> {
    use crate::relay::write_connection_url;
    use data_encoding::BASE32_NOPAD;

    // Generate bundle if needed
    let bundle = get_or_generate_connection_bundle(browser, runtime)?;

    // Serialize to binary and encode
    let bytes = bundle
        .to_binary()
        .map_err(|e| format!("Cannot serialize PreKeyBundle: {e}"))?;
    let encoded = BASE32_NOPAD.encode(&bytes);

    // Build connection URL
    // Mixed-mode QR: byte for URL, alphanumeric for Base32 bundle
    let connection_url = format!("{}/hubs/{}#{}", server_url, server_hub_id, encoded);

    // Write to file for external access (testing/automation)
    if let Err(e) = write_connection_url(local_identifier, &connection_url) {
        log::warn!("Failed to write connection URL: {e}");
    } else {
        log::info!("Connection URL written for external access");
    }

    Ok(connection_url)
}

/// Send shutdown notification to server.
///
/// Call this when the hub is shutting down to unregister from the server.
pub fn shutdown(client: &Client, server_url: &str, hub_identifier: &str, api_key: &str) {
    log::info!("Sending shutdown notification to server...");
    let shutdown_url = format!("{server_url}/hubs/{hub_identifier}");

    match client
        .delete(&shutdown_url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
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
            token: "test".to_string(),
            poll_interval: 10,
            agent_timeout: 300,
            max_sessions: 10,
            worktree_base: std::path::PathBuf::from("/tmp"),
        };

        register_device(&mut device, &client, &config);
        // Success = no panic
    }

    #[test]
    fn test_init_signal_protocol_only_starts_crypto_service() {
        // Verify that init_signal_protocol doesn't block or generate bundle
        let mut browser = BrowserState::default();

        // Before init: no crypto service
        assert!(browser.crypto_service.is_none());
        assert!(browser.signal_bundle.is_none());
        assert!(!browser.relay_connected);

        // Init should start crypto service but NOT generate bundle
        init_signal_protocol(&mut browser, "test-hub-id");

        // After init: crypto service started, but no bundle yet
        assert!(browser.crypto_service.is_some(), "CryptoService should be started");
        assert!(browser.signal_bundle.is_none(), "Bundle should NOT be generated at init");
        assert!(browser.relay_connected, "relay_connected should be true after init");
    }

    #[test]
    fn test_lazy_bundle_generation_returns_cached() {
        // Use unique hub_id to avoid test interference
        let hub_id = format!("test-lazy-bundle-{}", uuid::Uuid::new_v4());

        // Create a browser state with crypto service
        let mut browser = BrowserState::default();
        init_signal_protocol(&mut browser, &hub_id);

        // Create runtime for bundle generation
        let runtime = tokio::runtime::Runtime::new().unwrap();

        // First call should generate bundle
        let result1 = get_or_generate_connection_bundle(&mut browser, &runtime);
        assert!(result1.is_ok(), "First bundle generation should succeed");
        assert!(browser.signal_bundle.is_some(), "Bundle should be cached");
        assert!(!browser.bundle_used, "Bundle should not be marked as used");

        // Get the identity key from first bundle
        let identity1 = result1.unwrap().identity_key;

        // Second call should return cached bundle (same identity)
        let result2 = get_or_generate_connection_bundle(&mut browser, &runtime);
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap().identity_key, identity1, "Should return cached bundle");
    }

    #[test]
    fn test_lazy_bundle_regenerates_when_used() {
        // Use unique hub_id to avoid test interference
        let hub_id = format!("test-regen-bundle-{}", uuid::Uuid::new_v4());

        let mut browser = BrowserState::default();
        init_signal_protocol(&mut browser, &hub_id);

        let runtime = tokio::runtime::Runtime::new().unwrap();

        // Generate first bundle
        let result1 = get_or_generate_connection_bundle(&mut browser, &runtime);
        assert!(result1.is_ok(), "First bundle generation should succeed");
        let bundle1 = result1.unwrap();
        let prekey_id_1 = bundle1.prekey_id;

        // Mark bundle as used (simulating a browser connection)
        browser.bundle_used = true;

        // Next call should regenerate with new prekey
        let result2 = get_or_generate_connection_bundle(&mut browser, &runtime);
        assert!(result2.is_ok(), "Second bundle generation should succeed");
        let bundle2 = result2.unwrap();
        let prekey_id_2 = bundle2.prekey_id;

        // New bundle should have different prekey ID (incremented)
        assert_ne!(prekey_id_1, prekey_id_2, "Should generate new prekey after bundle used");
        assert!(!browser.bundle_used, "bundle_used should be reset after regeneration");

        // Both bundles should have the same identity key (same crypto service)
        assert_eq!(bundle1.identity_key, bundle2.identity_key, "Identity key should remain same");
    }
}
