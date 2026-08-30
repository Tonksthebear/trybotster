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
/// Generate a fresh one-time pairing secret and publish its URL.
pub fn handle_regenerate_connection_code(hub: &mut Hub) {
    hub.browser.pairing_bundle = None;
    hub.browser.bundle_used = false;
    match hub.generate_connection_url() {
        Ok(ref url) => {
            log::info!("Connection URL updated with a fresh pairing secret");
            if let Err(error) = hub.lua.fire_connection_code_ready(url) {
                log::error!("Failed to fire connection_code_ready: {error}");
            }
        }
        Err(error) => log::error!("Failed to generate a fresh connection URL: {error}"),
    }
}
