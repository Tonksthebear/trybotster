//! Hub registration and connection management.
//!
//! This module handles hub registration with the Rails server
//! and WebSocket connection setup for browser relay.
//!
//! # Responsibilities
//!
//! - Hub registration for message routing (with device fingerprint)
//! - Browser pairing authority initialization
//!
//! # Lazy Bundle Generation
//!
//! To avoid blocking boot, device key bundle generation is deferred until the
//! connection URL is first requested. The flow is:
//!
//! 1. `init_pairing_authority()` loads the hub key and browser credentials.
//! 2. `get_or_generate_connection_bundle()` creates a one-time pairing secret.
//! 3. `write_connection_url_lazy()` writes the secret only in the URL fragment.
//!
//! Bundle generation is triggered by:
//! - TUI QR code display request (`GetConnectionCode` command)
//! - External automation requesting the connection URL

// Rust guideline compliant 2026-02

use reqwest::blocking::Client;

use crate::relay::BrowserState;

/// Register the hub with the server and get the Rails-assigned ID.
///
/// This creates the Hub record on the server and returns the database ID
/// which should be used for all subsequent URLs and WebSocket subscriptions
/// to guarantee uniqueness. The device `fingerprint` is sent so the server
/// can associate identity without a separate device registration step.
///
/// # Returns
///
/// The Rails-assigned hub ID as a string, or the local identifier if
/// registration fails (for offline/degraded mode).
pub fn register_hub_with_server(
    local_identifier: &str,
    server_url: &str,
    api_key: &str,
    fingerprint: &str,
) -> String {
    // Detect repo: env var > git detection (optional — not stored on server)
    let repo_name: Option<String> = std::env::var("BOTSTER_REPO").ok().or_else(|| {
        crate::git::WorktreeManager::detect_current_repo()
            .map(|(_, name)| name)
            .ok()
    });

    if repo_name.is_none() && !crate::env::is_any_test() {
        log::warn!("Not in a git repository — GitHub event subscription disabled. Run from a git repo or set BOTSTER_REPO env var.");
    }

    // POST /hubs to register and get server-assigned ID
    let url = format!("{server_url}/hubs");
    let mut payload = serde_json::json!({
        "identifier": local_identifier,
        "fingerprint": fingerprint,
    });
    if let Some(ref repo) = repo_name {
        payload["repo"] = serde_json::Value::String(repo.clone());
    }
    log::info!("Registering hub with server to get Botster ID...");
    match reqwest::blocking::Client::builder()
        .user_agent(crate::constants::user_agent())
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("failed to build HTTP client")
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

/// Initialize the hub-owned browser pairing authority.
pub fn init_pairing_authority(
    browser: &mut BrowserState,
    signing_key: ed25519_dalek::SigningKey,
    local_identifier: &str,
    server_hub_id: &str,
) -> Result<(), String> {
    let route = format!("/hubs/{server_hub_id}");
    let authority = crate::relay::PairingAuthority::load(signing_key, local_identifier, route)
        .map_err(|error| format!("Cannot initialize browser pairing authority: {error}"))?;
    browser.pairing_authority = Some(authority);
    browser.relay_connected = true;
    log::info!("Browser pairing authority initialized");
    Ok(())
}

/// Generate the pairing bundle lazily on first request.
///
/// This is called when the connection URL is first needed (TUI QR display,
/// external automation, etc.). The bundle is cached in `BrowserState` for
/// subsequent requests.
///
/// # Returns
///
/// The generated pairing bundle, or an error if generation fails.
///
/// # Errors
///
/// Returns an error if:
/// - The pairing authority is not initialized
/// - Bundle generation fails
pub fn get_or_generate_connection_bundle(
    browser: &mut BrowserState,
    _runtime: &tokio::runtime::Runtime,
) -> Result<crate::relay::PairingBundle, String> {
    // Return cached bundle if available and not used
    if let Some(ref bundle) = browser.pairing_bundle {
        if !browser.bundle_used {
            return Ok(bundle.clone());
        }
        // Bundle was used, need to regenerate
        log::info!("Previous pairing secret was used, generating a fresh secret");
    }

    let authority = browser
        .pairing_authority
        .as_mut()
        .ok_or_else(|| "Browser pairing authority is not initialized".to_string())?;
    let bundle = authority.create_pairing_bundle();

    // Cache the bundle
    browser.pairing_bundle = Some(bundle.clone());
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
        .map_err(|e| format!("Cannot serialize pairing bundle: {e}"))?;
    let encoded = BASE32_NOPAD.encode(&bytes);

    // Build connection URL
    // Mixed-mode QR: byte for URL, alphanumeric for Base32 bundle
    let connection_url = format!("{}/hubs/{}/pairing#{}", server_url, server_hub_id, encoded);

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
/// Marks the hub as offline (`alive: false`) via PUT. The hub record is
/// preserved so the same ID is reused on next startup. Full destruction
/// only happens on `cli reset` (which deletes the device, cascading to hubs).
pub fn shutdown(client: &Client, server_url: &str, hub_identifier: &str, api_key: &str) {
    log::info!("Sending shutdown notification to server...");
    let shutdown_url = format!("{server_url}/hubs/{hub_identifier}");

    let payload = serde_json::json!({ "alive": false });

    match client
        .put(&shutdown_url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
    {
        Ok(response) if response.status().is_success() => {
            log::info!("Hub marked offline on server");
        }
        Ok(response) => {
            log::warn!("Failed to mark hub offline: {}", response.status());
        }
        Err(e) => {
            log::warn!("Failed to send shutdown notification: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn browser() -> BrowserState {
        BrowserState {
            pairing_authority: Some(crate::relay::PairingAuthority::in_memory(
                SigningKey::from_bytes(&[5; 32]),
                "/hubs/42".into(),
            )),
            relay_connected: true,
            ..BrowserState::default()
        }
    }

    #[test]
    fn pairing_bundle_is_cached_until_consumed() {
        let mut browser = browser();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let first = get_or_generate_connection_bundle(&mut browser, &runtime).unwrap();
        let second = get_or_generate_connection_bundle(&mut browser, &runtime).unwrap();
        assert_eq!(first, second);
        assert!(browser.pairing_bundle.is_some());
    }

    #[test]
    fn consumed_pairing_bundle_gets_a_fresh_secret() {
        let mut browser = browser();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let first = get_or_generate_connection_bundle(&mut browser, &runtime).unwrap();
        browser.bundle_used = true;
        let second = get_or_generate_connection_bundle(&mut browser, &runtime).unwrap();
        assert_ne!(first.pairing_secret, second.pairing_secret);
        assert_eq!(first.hub_public_key, second.hub_public_key);
        assert!(!browser.bundle_used);
    }
}
