//! Menu and modal handlers.
//!
//! Handlers for menu navigation, selection, and modal operations.

use crate::app::AppMode;
use crate::hub::Hub;
use crate::PtyView;

use super::{dispatch, HubAction};

/// Handle closing a modal/overlay.
pub fn handle_close_modal(hub: &mut Hub) {
    // If closing ConnectionCode modal, delete any Kitty graphics images
    if hub.mode == AppMode::ConnectionCode {
        use crate::tui::qr::kitty_delete_images;
        use std::io::Write;
        let _ = std::io::stdout().write_all(kitty_delete_images().as_bytes());
        let _ = std::io::stdout().flush();
    }
    hub.mode = AppMode::Normal;
    hub.input_buffer.clear();
    hub.error_message = None; // Clear error message if in Error mode
}

/// Handle showing the connection QR code.
pub fn handle_show_connection_code(hub: &mut Hub) {
    // Generate connection URL with Signal PreKeyBundle
    // Format: /hubs/{id}#{base32_binary_bundle}
    // - All uppercase for QR alphanumeric mode (4296 char capacity vs 2953 byte mode)
    // - Binary format (1813 bytes) + Base32 = ~2900 chars (fits easily)
    // - Hub ID in path, bundle in fragment (never sent to server)
    hub.connection_url = if let Some(ref bundle) = hub.browser.signal_bundle {
        use data_encoding::BASE32_NOPAD;
        match bundle.to_binary() {
            Ok(bytes) => {
                let encoded = BASE32_NOPAD.encode(&bytes);
                // URL uses mixed-mode QR encoding:
                // - URL portion (up to #): byte mode (any case allowed)
                // - Bundle (after #): alphanumeric mode (must be uppercase Base32)
                // Rails ID is numeric, uppercase is no-op but harmless
                let url = format!(
                    "{}/hubs/{}#{}",
                    hub.config.server_url,
                    hub.server_hub_id(),
                    encoded
                );
                log::debug!(
                    "Connection URL: {} chars (QR alphanumeric capacity: 4296)",
                    url.len()
                );
                Some(url)
            }
            Err(e) => {
                log::error!("Cannot serialize PreKeyBundle to binary: {e}");
                None
            }
        }
    } else {
        log::error!("Cannot show connection code: Signal bundle not initialized");
        None
    };
    // Reset QR image flag so it renders fresh when modal opens
    hub.qr_image_displayed = false;
    hub.mode = AppMode::ConnectionCode;
}

/// Handle copying connection URL to clipboard.
///
/// Generates the connection URL fresh from the current Signal bundle rather than
/// using the potentially stale `hub.connection_url` cache. This ensures the copied
/// URL always contains the current Kyber bundle.
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
        // Reset QR image flag so it renders fresh when new bundle arrives
        hub.qr_image_displayed = false;
    } else {
        log::warn!("Cannot regenerate bundle: relay not connected");
    }
}

/// Handle menu navigation up.
pub fn handle_menu_up(hub: &mut Hub) {
    if hub.menu_selected > 0 {
        hub.menu_selected -= 1;
    }
}

/// Handle menu navigation down.
pub fn handle_menu_down(hub: &mut Hub) {
    let menu_ctx = build_menu_context(hub);
    let items = crate::tui::menu::build_menu(&menu_ctx);
    let selectable = crate::tui::menu::selectable_count(&items);
    if hub.menu_selected < selectable.saturating_sub(1) {
        hub.menu_selected += 1;
    }
}

/// Handle menu item selection.
pub fn handle_menu_select(hub: &mut Hub, selection_index: usize) {
    use crate::tui::menu::{build_menu, get_action_for_selection, MenuAction};

    let ctx = build_menu_context(hub);
    let items = build_menu(&ctx);

    let Some(action) = get_action_for_selection(&items, selection_index) else {
        hub.mode = AppMode::Normal;
        return;
    };

    match action {
        MenuAction::TogglePtyView => {
            // PTY view toggle is handled by TuiRunner directly, not via Hub dispatch
            // The menu just closes; TuiRunner handles the actual toggle via TuiAction
            log::debug!("MenuAction::TogglePtyView - handled by TuiRunner, not Hub");
            hub.mode = AppMode::Normal;
        }
        MenuAction::CloseAgent => {
            if hub.state.read().unwrap().agent_keys_ordered.is_empty() {
                hub.mode = AppMode::Normal;
            } else {
                hub.mode = AppMode::CloseAgentConfirm;
            }
        }
        MenuAction::NewAgent => {
            if let Err(e) = hub.load_available_worktrees() {
                log::error!("Failed to load worktrees: {}", e);
                hub.show_error(format!("Failed to load worktrees: {}", e));
            } else {
                hub.mode = AppMode::NewAgentSelectWorktree;
                hub.worktree_selected = 0;
            }
        }
        MenuAction::ShowConnectionCode => {
            dispatch(hub, HubAction::ShowConnectionCode);
        }
    }
}

/// Build menu context from current hub state.
///
/// IMPORTANT: This must use the same selection logic as render.rs to ensure
/// the displayed menu matches the navigation bounds. If TUI has no explicit
/// selection, we fall back to the first agent (index 0) for consistency.
pub fn build_menu_context(hub: &Hub) -> crate::tui::MenuContext {
    let state = hub.state.read().unwrap();
    // Use same fallback logic as render.rs: if no TUI selection, use first agent
    let tui_key = hub.get_tui_selected_agent_key();
    let selected_agent = tui_key
        .as_ref()
        .and_then(|key| state.agents.get(key))
        .or_else(|| {
            // Fallback: use first agent if any exist (matches render.rs behavior)
            state
                .agent_keys_ordered
                .first()
                .and_then(|key| state.agents.get(key))
        });

    crate::tui::MenuContext {
        has_agent: selected_agent.is_some(),
        has_server_pty: selected_agent.map_or(false, |a| a.has_server_pty()),
        // Active PTY view is client-local state (TuiClient owns it, not Agent)
        // Default to CLI view for menu context
        active_pty: PtyView::Cli,
    }
}
