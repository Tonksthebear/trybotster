//! Terminal input handling for the TUI.
//!
//! This module provides conversion from crossterm terminal events to
//! [`HubAction`]s that the Hub can process. It acts as a bridge between
//! the terminal input layer and the Hub's action system.
//!
//! # Architecture
//!
//! ```text
//! crossterm::Event ──► event_to_hub_action() ──► HubAction
//!                                                    │
//!                                Hub::handle_action() ◄┘
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use crossterm::event;
//! use tui::input::event_to_hub_action;
//!
//! while event::poll(Duration::from_millis(100))? {
//!     let evt = event::read()?;
//!     if let Some(action) = event_to_hub_action(&evt, &app_mode) {
//!         hub.handle_action(action)?;
//!     }
//! }
//! ```

// Rust guideline compliant 2025-01

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};

use crate::app::AppMode;
use crate::client::ClientId;
use crate::hub::HubAction;

use super::layout::terminal_widget_inner_area;

/// Convert a crossterm Event to a HubAction.
///
/// Returns `None` if the event doesn't map to any action (e.g., key release events).
///
/// # Arguments
///
/// * `event` - The crossterm event to convert
/// * `mode` - Current application mode (affects key bindings)
/// * `context` - Additional context for action generation
#[must_use]
pub fn event_to_hub_action(
    event: &Event,
    mode: &AppMode,
    context: &InputContext,
) -> Option<HubAction> {
    match event {
        Event::Key(key) => key_event_to_action(key, mode, context),
        Event::Mouse(mouse) => mouse_event_to_action(*mouse, *mode),
        // Use client-scoped resize action for TUI
        // Calculate the actual terminal widget inner area (70% width minus borders)
        // to ensure PTY dimensions match the visible rendering area
        Event::Resize(cols, rows) => {
            let (inner_rows, inner_cols) = terminal_widget_inner_area(*cols, *rows);
            Some(HubAction::ResizeForClient {
                client_id: ClientId::Tui,
                rows: inner_rows,
                cols: inner_cols,
            })
        }
        _ => None,
    }
}

/// Context information needed for input handling.
///
/// Provides additional state that affects how input events are converted to actions.
#[derive(Debug, Clone, Default)]
pub struct InputContext {
    /// Current terminal height.
    pub terminal_rows: u16,
    /// Currently selected menu item.
    pub menu_selected: usize,
    /// Total number of menu items.
    pub menu_count: usize,
    /// Currently selected worktree.
    pub worktree_selected: usize,
    /// Total number of available worktrees.
    pub worktree_count: usize,
}

/// Convert a key event to a HubAction.
///
/// This is public to allow the Hub to convert browser input events
/// using the same logic as local terminal input.
pub fn key_event_to_action(
    key: &KeyEvent,
    mode: &AppMode,
    context: &InputContext,
) -> Option<HubAction> {
    // Only process key press events
    if key.kind != KeyEventKind::Press {
        return None;
    }

    match mode {
        AppMode::Normal => normal_mode_key(key, context),
        AppMode::Menu => menu_mode_key(key, context),
        AppMode::NewAgentSelectWorktree => worktree_select_key(key, context),
        AppMode::NewAgentCreateWorktree | AppMode::NewAgentPrompt => text_input_key(key),
        AppMode::CloseAgentConfirm => close_confirm_key(key),
        AppMode::ConnectionCode => connection_code_key(key),
        AppMode::Error => error_mode_key(key),
    }
}

/// Key handling for normal mode.
///
/// Hub control uses Ctrl+key combinations to avoid interfering with PTY input:
/// - `Ctrl+Q` - Quit
/// - `Ctrl+P` - Open menu
/// - `Ctrl+J` - Next agent
/// - `Ctrl+K` - Previous agent
/// - `Ctrl+]` - Toggle PTY view (CLI/Server)
///
/// Note: Ctrl+\ is SIGQUIT in Unix terminals, so we use Ctrl+] instead.
///
/// All other keys are forwarded to the active PTY.
fn normal_mode_key(key: &KeyEvent, context: &InputContext) -> Option<HubAction> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        // Hub control - Ctrl combinations only
        KeyCode::Char('q') if ctrl => Some(HubAction::Quit),
        KeyCode::Char('p') if ctrl => Some(HubAction::OpenMenu),
        KeyCode::Char('j') if ctrl => Some(HubAction::SelectNext),
        KeyCode::Char('k') if ctrl => Some(HubAction::SelectPrevious),
        KeyCode::Char(']') if ctrl => Some(HubAction::TogglePtyView),

        // Scrolling (Shift+PageUp/Down don't interfere with normal typing)
        KeyCode::PageUp if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(HubAction::ScrollUp(context.terminal_rows as usize / 2))
        }
        KeyCode::PageDown if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(HubAction::ScrollDown(context.terminal_rows as usize / 2))
        }
        KeyCode::Home if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(HubAction::ScrollToTop)
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(HubAction::ScrollToBottom)
        }

        // Forward everything else to PTY (using client-scoped action for TUI)
        KeyCode::Char(c) if ctrl => {
            // Send control character (Ctrl+A = 1, Ctrl+C = 3, etc.)
            let ctrl_byte = (c.to_ascii_uppercase() as u8).wrapping_sub(b'@');
            Some(HubAction::SendInputForClient {
                client_id: ClientId::Tui,
                data: vec![ctrl_byte],
            })
        }
        KeyCode::Char(c) => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: c.to_string().into_bytes(),
        }),
        KeyCode::Enter => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![b'\r'],
        }),
        KeyCode::Backspace => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x7f],
        }),
        KeyCode::Tab => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![b'\t'],
        }),
        KeyCode::Esc => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x1b],
        }),
        KeyCode::Up => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x1b, b'[', b'A'],
        }),
        KeyCode::Down => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x1b, b'[', b'B'],
        }),
        KeyCode::Right => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x1b, b'[', b'C'],
        }),
        KeyCode::Left => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x1b, b'[', b'D'],
        }),
        KeyCode::Home => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x1b, b'[', b'H'],
        }),
        KeyCode::End => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x1b, b'[', b'F'],
        }),
        KeyCode::PageUp => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x1b, b'[', b'5', b'~'],
        }),
        KeyCode::PageDown => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x1b, b'[', b'6', b'~'],
        }),
        KeyCode::Delete => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x1b, b'[', b'3', b'~'],
        }),
        KeyCode::Insert => Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![0x1b, b'[', b'2', b'~'],
        }),

        _ => None,
    }
}

/// Key handling for menu mode.
fn menu_mode_key(key: &KeyEvent, context: &InputContext) -> Option<HubAction> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => Some(HubAction::CloseModal),
        KeyCode::Up | KeyCode::Char('k') => Some(HubAction::MenuUp),
        KeyCode::Down | KeyCode::Char('j') => Some(HubAction::MenuDown),
        KeyCode::Enter | KeyCode::Char(' ') => {
            Some(HubAction::MenuSelect(context.menu_selected))
        }
        KeyCode::Char(c @ '1'..='9') => {
            let idx = (c.to_digit(10)? as usize).saturating_sub(1);
            (idx < context.menu_count).then_some(HubAction::MenuSelect(idx))
        }
        _ => None,
    }
}

/// Key handling for worktree selection mode.
fn worktree_select_key(key: &KeyEvent, context: &InputContext) -> Option<HubAction> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => Some(HubAction::CloseModal),
        KeyCode::Up | KeyCode::Char('k') => Some(HubAction::WorktreeUp),
        KeyCode::Down | KeyCode::Char('j') => Some(HubAction::WorktreeDown),
        KeyCode::Enter | KeyCode::Char(' ') => {
            Some(HubAction::WorktreeSelect(context.worktree_selected))
        }
        _ => None,
    }
}

/// Key handling for text input modes.
fn text_input_key(key: &KeyEvent) -> Option<HubAction> {
    match key.code {
        KeyCode::Esc => Some(HubAction::CloseModal),
        KeyCode::Enter => Some(HubAction::InputSubmit),
        KeyCode::Backspace => Some(HubAction::InputBackspace),
        KeyCode::Char(c) => Some(HubAction::InputChar(c)),
        _ => None,
    }
}

/// Key handling for close agent confirmation.
fn close_confirm_key(key: &KeyEvent) -> Option<HubAction> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('n' | 'q') => Some(HubAction::CloseModal),
        KeyCode::Char('y') | KeyCode::Enter => Some(HubAction::ConfirmCloseAgent),
        KeyCode::Char('d') => Some(HubAction::ConfirmCloseAgentDeleteWorktree),
        _ => None,
    }
}

/// Key handling for connection code display.
fn connection_code_key(key: &KeyEvent) -> Option<HubAction> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => Some(HubAction::CloseModal),
        KeyCode::Char('c') => Some(HubAction::CopyConnectionUrl),
        KeyCode::Char('r') => Some(HubAction::RegenerateConnectionCode),
        _ => None,
    }
}

/// Key handling for error display mode.
fn error_mode_key(key: &KeyEvent) -> Option<HubAction> {
    match key.code {
        // Any of these keys dismiss the error
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => Some(HubAction::CloseModal),
        _ => None,
    }
}

/// Convert a mouse event to a HubAction.
fn mouse_event_to_action(mouse: MouseEvent, mode: AppMode) -> Option<HubAction> {
    // Only handle mouse in normal mode
    if mode != AppMode::Normal {
        return None;
    }

    match mouse.kind {
        MouseEventKind::ScrollUp => Some(HubAction::ScrollUp(3)),
        MouseEventKind::ScrollDown => Some(HubAction::ScrollDown(3)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn make_key_ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    fn default_context() -> InputContext {
        InputContext {
            terminal_rows: 24,
            menu_selected: 0,
            menu_count: 4,
            worktree_selected: 0,
            worktree_count: 3,
        }
    }

    #[test]
    fn test_ctrl_q_quits() {
        let context = default_context();
        let action = key_event_to_action(&make_key_ctrl(KeyCode::Char('q')), &AppMode::Normal, &context);
        assert_eq!(action, Some(HubAction::Quit));
    }

    #[test]
    fn test_ctrl_p_opens_menu() {
        let context = default_context();
        let action = key_event_to_action(
            &make_key_ctrl(KeyCode::Char('p')),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(action, Some(HubAction::OpenMenu));
    }

    #[test]
    fn test_ctrl_navigation_keys() {
        let context = default_context();

        let action = key_event_to_action(&make_key_ctrl(KeyCode::Char('j')), &AppMode::Normal, &context);
        assert_eq!(action, Some(HubAction::SelectNext));

        let action = key_event_to_action(&make_key_ctrl(KeyCode::Char('k')), &AppMode::Normal, &context);
        assert_eq!(action, Some(HubAction::SelectPrevious));
    }

    #[test]
    fn test_ctrl_bracket_toggles_pty() {
        let context = default_context();
        let action = key_event_to_action(
            &make_key_ctrl(KeyCode::Char(']')),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(action, Some(HubAction::TogglePtyView));
    }

    #[test]
    fn test_plain_keys_forward_to_pty() {
        use crate::client::ClientId;
        let context = default_context();

        // Plain 'q' should forward to PTY (client-scoped), not quit
        let action = key_event_to_action(&make_key(KeyCode::Char('q')), &AppMode::Normal, &context);
        assert_eq!(action, Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![b'q'],
        }));

        // Plain 'j' should forward to PTY (client-scoped), not navigate
        let action = key_event_to_action(&make_key(KeyCode::Char('j')), &AppMode::Normal, &context);
        assert_eq!(action, Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![b'j'],
        }));

        // Number keys should forward to PTY (client-scoped)
        let action = key_event_to_action(&make_key(KeyCode::Char('3')), &AppMode::Normal, &context);
        assert_eq!(action, Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![b'3'],
        }));
    }

    #[test]
    fn test_ctrl_c_forwards_to_pty() {
        use crate::client::ClientId;
        let context = default_context();
        // Ctrl+C should forward as control character (0x03) via client-scoped action
        let action = key_event_to_action(
            &make_key_ctrl(KeyCode::Char('c')),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(action, Some(HubAction::SendInputForClient {
            client_id: ClientId::Tui,
            data: vec![3],
        }));
    }

    #[test]
    fn test_menu_navigation() {
        let context = default_context();

        let action = key_event_to_action(&make_key(KeyCode::Up), &AppMode::Menu, &context);
        assert_eq!(action, Some(HubAction::MenuUp));

        let action = key_event_to_action(&make_key(KeyCode::Down), &AppMode::Menu, &context);
        assert_eq!(action, Some(HubAction::MenuDown));

        let action = key_event_to_action(&make_key(KeyCode::Enter), &AppMode::Menu, &context);
        assert_eq!(action, Some(HubAction::MenuSelect(0)));
    }

    #[test]
    fn test_escape_closes_modal() {
        let context = default_context();

        let action = key_event_to_action(&make_key(KeyCode::Esc), &AppMode::Menu, &context);
        assert_eq!(action, Some(HubAction::CloseModal));

        let action = key_event_to_action(&make_key(KeyCode::Esc), &AppMode::ConnectionCode, &context);
        assert_eq!(action, Some(HubAction::CloseModal));
    }

    #[test]
    fn test_text_input_mode() {
        let context = default_context();

        let action = key_event_to_action(
            &make_key(KeyCode::Char('a')),
            &AppMode::NewAgentCreateWorktree,
            &context,
        );
        assert_eq!(action, Some(HubAction::InputChar('a')));

        let action = key_event_to_action(
            &make_key(KeyCode::Backspace),
            &AppMode::NewAgentPrompt,
            &context,
        );
        assert_eq!(action, Some(HubAction::InputBackspace));
    }

    #[test]
    fn test_mouse_scroll() {
        let mouse_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };

        let action = mouse_event_to_action(mouse_up, AppMode::Normal);
        assert_eq!(action, Some(HubAction::ScrollUp(3)));
    }

    #[test]
    fn test_mouse_ignored_in_menu() {
        let mouse_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };

        let action = mouse_event_to_action(mouse_up, AppMode::Menu);
        assert_eq!(action, None);
    }

    #[test]
    fn test_resize_event() {
        use crate::client::ClientId;
        use super::terminal_widget_inner_area;

        let event = Event::Resize(120, 40);
        let context = default_context();
        let action = event_to_hub_action(&event, &AppMode::Normal, &context);

        // Resize should use the calculated inner area, not raw terminal dims
        let (expected_rows, expected_cols) = terminal_widget_inner_area(120, 40);
        assert_eq!(action, Some(HubAction::ResizeForClient {
            client_id: ClientId::Tui,
            cols: expected_cols,
            rows: expected_rows,
        }));
    }

    // === Phase 2B: TUI input should create client-scoped actions ===
    //
    // TUI input events should produce actions that include ClientId::Tui,
    // enabling proper client isolation in multi-client scenarios.

    #[test]
    fn test_tui_send_input_uses_client_scoped_action() {
        use crate::client::ClientId;
        let context = default_context();

        // Plain key 'a' should produce SendInputForClient with ClientId::Tui
        let action = key_event_to_action(&make_key(KeyCode::Char('a')), &AppMode::Normal, &context);
        assert_eq!(
            action,
            Some(HubAction::SendInputForClient {
                client_id: ClientId::Tui,
                data: vec![b'a'],
            }),
            "TUI input should produce SendInputForClient with ClientId::Tui"
        );
    }

    #[test]
    fn test_tui_ctrl_char_uses_client_scoped_action() {
        use crate::client::ClientId;
        let context = default_context();

        // Ctrl+C (0x03) should produce SendInputForClient with ClientId::Tui
        let action = key_event_to_action(
            &make_key_ctrl(KeyCode::Char('c')),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(
            action,
            Some(HubAction::SendInputForClient {
                client_id: ClientId::Tui,
                data: vec![3],
            }),
            "TUI Ctrl+C should produce SendInputForClient with ClientId::Tui"
        );
    }

    #[test]
    fn test_tui_resize_uses_client_scoped_action() {
        use crate::client::ClientId;
        use super::terminal_widget_inner_area;
        let context = default_context();

        let event = Event::Resize(120, 40);
        let action = event_to_hub_action(&event, &AppMode::Normal, &context);

        // Resize should use the calculated inner area for PTY dimensions
        let (expected_rows, expected_cols) = terminal_widget_inner_area(120, 40);
        assert_eq!(
            action,
            Some(HubAction::ResizeForClient {
                client_id: ClientId::Tui,
                cols: expected_cols,
                rows: expected_rows,
            }),
            "TUI resize should produce ResizeForClient with ClientId::Tui and calculated inner area"
        );
    }

    // === Connection Code Modal Tests ===

    #[test]
    fn test_connection_code_r_regenerates() {
        let context = default_context();
        let action = key_event_to_action(
            &make_key(KeyCode::Char('r')),
            &AppMode::ConnectionCode,
            &context,
        );
        assert_eq!(
            action,
            Some(HubAction::RegenerateConnectionCode),
            "Pressing 'r' in ConnectionCode mode should regenerate the QR code"
        );
    }

    #[test]
    fn test_connection_code_c_copies_url() {
        let context = default_context();
        let action = key_event_to_action(
            &make_key(KeyCode::Char('c')),
            &AppMode::ConnectionCode,
            &context,
        );
        assert_eq!(
            action,
            Some(HubAction::CopyConnectionUrl),
            "Pressing 'c' in ConnectionCode mode should copy URL"
        );
    }

    #[test]
    fn test_connection_code_escape_closes() {
        let context = default_context();
        let action = key_event_to_action(
            &make_key(KeyCode::Esc),
            &AppMode::ConnectionCode,
            &context,
        );
        assert_eq!(
            action,
            Some(HubAction::CloseModal),
            "Pressing Esc in ConnectionCode mode should close modal"
        );
    }
}
