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
/// Requests a new PreKeyBundle from the relay. The new bundle will arrive
/// asynchronously via a BundleRegenerated event.
pub fn handle_regenerate_connection_code(hub: &mut Hub) {
    // Request a new PreKeyBundle from the relay
    if let Some(ref sender) = hub.browser.sender {
        let sender = sender.clone();
        hub.tokio_runtime.spawn(async move {
            if let Err(e) = sender.request_bundle_regeneration().await {
                log::error!("Failed to request bundle regeneration: {}", e);
            }
        });
        log::info!("Requested bundle regeneration");
    } else {
        log::warn!("Cannot regenerate bundle: relay not connected");
    }
}
