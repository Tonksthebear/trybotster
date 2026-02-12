//! Connection URL handlers.
//!
//! Handlers for connection URL clipboard copy and bundle regeneration.

use crate::hub::Hub;

/// Handle copying connection URL to clipboard via OSC 52.
///
/// Uses the OSC 52 terminal escape sequence to set the clipboard on the
/// user's local terminal. Works over SSH, tmux, and other remote sessions
/// (unlike arboard which requires a local display server).
pub fn handle_copy_connection_url(hub: &mut Hub) {
    use base64::Engine;

    match hub.generate_connection_url() {
        Ok(url) => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(&url);
            // OSC 52: \x1b]52;c;<base64>\x07
            // "c" = clipboard selection, BEL (\x07) terminates
            print!("\x1b]52;c;{}\x07", encoded);
            log::info!("Connection URL sent to clipboard via OSC 52");
        }
        Err(e) => log::warn!("Cannot copy connection URL: {}", e),
    }
}

/// Handle regenerating the connection code.
///
/// Force-regenerates the DeviceKeyBundle via CryptoService, updates Hub state,
/// caches the new URL (via `generate_connection_url()`), and fires a Lua
/// event so all hub subscribers receive the fresh URL.
pub fn handle_regenerate_connection_code(hub: &mut Hub) {
    // Get crypto service from browser state
    let Some(ref crypto_service) = hub.browser.crypto_service else {
        log::warn!("Cannot regenerate bundle: crypto service not initialized");
        return;
    };

    // Regenerate bundle directly via crypto service (synchronous mutex)
    let result = crypto_service
        .lock()
        .map_err(|e| anyhow::anyhow!("Mutex poisoned: {e}"))
        .and_then(|mut guard| guard.build_device_key_bundle());

    match result {
        Ok(bundle) => {
            log::info!(
                "New DeviceKeyBundle generated (identity: {}...)",
                &bundle.curve25519_key[..bundle.curve25519_key.len().min(16)]
            );
            hub.browser.device_key_bundle = Some(bundle);
            hub.browser.bundle_used = false;

            // generate_connection_url() handles both caching and file writing
            match hub.generate_connection_url() {
                Ok(ref url) => {
                    // Write to file for external access
                    let _ = crate::relay::write_connection_url(&hub.hub_identifier, url);
                    log::info!("Connection URL updated with new bundle");

                    // Fire Lua event (generates QR PNG, broadcasts to all hub subscribers)
                    if let Err(e) = hub.lua.fire_connection_code_ready(url) {
                        log::error!("Failed to fire connection_code_ready: {e}");
                    }
                }
                Err(e) => {
                    log::error!("Failed to generate connection URL after regeneration: {e}");
                }
            }
        }
        Err(e) => {
            log::error!("Failed to regenerate DeviceKeyBundle: {}", e);
        }
    }
}
