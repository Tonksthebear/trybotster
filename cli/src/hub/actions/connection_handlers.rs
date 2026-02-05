//! Connection URL handlers.
//!
//! Handlers for connection URL clipboard copy and bundle regeneration.

use crate::hub::Hub;

/// Handle copying connection URL to clipboard.
///
/// Generates the connection URL fresh from the current Signal bundle rather than
/// using a cache. This ensures the copied URL always contains the current bundle.
pub fn handle_copy_connection_url(hub: &mut Hub) {
    // Generate URL fresh from current Signal bundle (canonical source)
    match hub.generate_connection_url() {
        Ok(url) => match arboard::Clipboard::new() {
            Ok(mut clipboard) => {
                if clipboard.set_text(url).is_ok() {
                    log::info!("Connection URL copied to clipboard");
                }
            }
            Err(e) => log::warn!("Could not access clipboard: {}", e),
        },
        Err(e) => log::warn!("Cannot copy connection URL: {}", e),
    }
}

/// Handle regenerating the connection code.
///
/// Force-regenerates the PreKeyBundle via CryptoService, updates Hub state,
/// caches the new URL (via `generate_connection_url()`), and fires a Lua
/// event so all hub subscribers receive the fresh URL.
pub fn handle_regenerate_connection_code(hub: &mut Hub) {
    // Get crypto service from browser state
    let Some(crypto_service) = hub.browser.crypto_service.clone() else {
        log::warn!("Cannot regenerate bundle: crypto service not initialized");
        return;
    };

    // Regenerate bundle directly via crypto service
    let result = hub.tokio_runtime.block_on(async {
        let next_id = crypto_service.next_prekey_id().await.unwrap_or(1);
        crypto_service.get_prekey_bundle(next_id).await
    });

    match result {
        Ok(bundle) => {
            log::info!(
                "New PreKeyBundle generated with PreKey {}",
                bundle.prekey_id.unwrap_or(0)
            );
            hub.browser.signal_bundle = Some(bundle);
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
            log::error!("Failed to regenerate PreKeyBundle: {}", e);
        }
    }
}
