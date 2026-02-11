//! Terminal input handling for the TUI.
//!
//! This module converts crossterm terminal events into [`InputResult`] values
//! that TuiRunner can process locally. Input handling is mode-aware and produces
//! either TUI-local actions, raw PTY input, or resize events.
//!
//! # Architecture
//!
//! ```text
//! crossterm::Event --> process_event() --> InputResult
//!                                             |
//!                      TuiRunner.handle_*() <-+
//! ```
//!
//! # Input Categories
//!
//! - **TUI-local actions** (`TuiAction`) - Menu navigation, modal control, scrolling
//! - **PTY input** - Keystrokes forwarded to the selected agent's terminal
//! - **Resize events** - Terminal dimension changes
//!
//! # Normal Mode Key Bindings
//!
//! Hub control uses Ctrl+key combinations to avoid interfering with PTY input:
//! - `Ctrl+Q` - Quit
//! - `Ctrl+P` - Open menu
//! - `Ctrl+J` - Next agent
//! - `Ctrl+K` - Previous agent
//! - `Ctrl+]` - Toggle PTY view (CLI/Server)
//!
//! Note: `Ctrl+\` is SIGQUIT in Unix terminals, so we use `Ctrl+]` instead.
//!
//! All other keys in Normal mode are forwarded to the active PTY.

// Rust guideline compliant 2026-01

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};

use crate::app::AppMode;

use super::actions::{InputResult, TuiAction};
use super::layout::terminal_widget_inner_area;

/// Context information needed for input handling.
///
/// Provides state that affects how input events are converted to results.
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

/// Process a crossterm event into an input result.
///
/// Returns `InputResult::None` if the event doesn't map to any action
/// (e.g., key release events).
///
/// # Arguments
///
/// * `event` - The crossterm event to process
/// * `mode` - Current application mode (affects key bindings)
/// * `context` - Additional context for input processing
#[must_use]
pub fn process_event(event: &Event, mode: &AppMode, context: &InputContext) -> InputResult {
    match event {
        Event::Key(key) => process_key_event(key, mode, context),
        Event::Mouse(mouse) => process_mouse_event(*mouse, *mode),
        Event::Resize(cols, rows) => {
            // Calculate the actual terminal widget inner area (70% width minus borders)
            // to ensure PTY dimensions match the visible rendering area.
            let (inner_rows, inner_cols) = terminal_widget_inner_area(*cols, *rows);
            InputResult::Resize {
                rows: inner_rows,
                cols: inner_cols,
            }
        }
        _ => InputResult::None,
    }
}

/// Process a key event into an input result.
///
/// Dispatches to mode-specific handlers.
fn process_key_event(key: &KeyEvent, mode: &AppMode, context: &InputContext) -> InputResult {
    // Only process key press events
    if key.kind != KeyEventKind::Press {
        return InputResult::None;
    }

    match mode {
        AppMode::Normal => process_normal_mode_key(key, context),
        AppMode::Menu => process_menu_mode_key(key, context),
        AppMode::NewAgentSelectWorktree => process_worktree_select_key(key, context),
        AppMode::NewAgentCreateWorktree | AppMode::NewAgentPrompt => process_text_input_key(key),
        AppMode::CloseAgentConfirm => process_close_confirm_key(key),
        AppMode::ConnectionCode => process_connection_code_key(key),
        AppMode::Error => process_error_mode_key(key),
    }
}

/// Key handling for normal mode.
///
/// Hub control uses Ctrl+key combinations. All other keys forward to PTY.
fn process_normal_mode_key(key: &KeyEvent, context: &InputContext) -> InputResult {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        // Hub control - Ctrl combinations only
        KeyCode::Char('q') if ctrl => TuiAction::Quit.into(),
        KeyCode::Char('p') if ctrl => TuiAction::OpenMenu.into(),
        KeyCode::Char('j') if ctrl => TuiAction::SelectNext.into(),
        KeyCode::Char('k') if ctrl => TuiAction::SelectPrevious.into(),
        KeyCode::Char(']') if ctrl => TuiAction::TogglePtyView.into(),

        // Scrolling (Shift+PageUp/Down don't interfere with normal typing)
        KeyCode::PageUp if key.modifiers.contains(KeyModifiers::SHIFT) => {
            TuiAction::ScrollUp(context.terminal_rows as usize / 2).into()
        }
        KeyCode::PageDown if key.modifiers.contains(KeyModifiers::SHIFT) => {
            TuiAction::ScrollDown(context.terminal_rows as usize / 2).into()
        }
        KeyCode::Home if key.modifiers.contains(KeyModifiers::SHIFT) => {
            TuiAction::ScrollToTop.into()
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::SHIFT) => {
            TuiAction::ScrollToBottom.into()
        }

        // Forward everything else to PTY
        KeyCode::Char(c) if ctrl => {
            // Send control character (Ctrl+A = 1, Ctrl+C = 3, etc.)
            let ctrl_byte = (c.to_ascii_uppercase() as u8).wrapping_sub(b'@');
            InputResult::pty_input(vec![ctrl_byte])
        }
        KeyCode::Char(c) => InputResult::pty_input(c.to_string().into_bytes()),
        KeyCode::Enter => InputResult::pty_input(vec![b'\r']),
        KeyCode::Backspace => InputResult::pty_input(vec![0x7f]),
        KeyCode::Tab => InputResult::pty_input(vec![b'\t']),
        KeyCode::Esc => InputResult::pty_input(vec![0x1b]),
        KeyCode::Up => InputResult::pty_input(vec![0x1b, b'[', b'A']),
        KeyCode::Down => InputResult::pty_input(vec![0x1b, b'[', b'B']),
        KeyCode::Right => InputResult::pty_input(vec![0x1b, b'[', b'C']),
        KeyCode::Left => InputResult::pty_input(vec![0x1b, b'[', b'D']),
        KeyCode::Home => InputResult::pty_input(vec![0x1b, b'[', b'H']),
        KeyCode::End => InputResult::pty_input(vec![0x1b, b'[', b'F']),
        KeyCode::PageUp => InputResult::pty_input(vec![0x1b, b'[', b'5', b'~']),
        KeyCode::PageDown => InputResult::pty_input(vec![0x1b, b'[', b'6', b'~']),
        KeyCode::Delete => InputResult::pty_input(vec![0x1b, b'[', b'3', b'~']),
        KeyCode::Insert => InputResult::pty_input(vec![0x1b, b'[', b'2', b'~']),

        _ => InputResult::None,
    }
}

/// Key handling for menu mode.
fn process_menu_mode_key(key: &KeyEvent, context: &InputContext) -> InputResult {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => TuiAction::CloseModal.into(),
        KeyCode::Up | KeyCode::Char('k') => TuiAction::MenuUp.into(),
        KeyCode::Down | KeyCode::Char('j') => TuiAction::MenuDown.into(),
        KeyCode::Enter | KeyCode::Char(' ') => TuiAction::MenuSelect(context.menu_selected).into(),
        KeyCode::Char(c @ '1'..='9') => {
            if let Some(digit) = c.to_digit(10) {
                let idx = (digit as usize).saturating_sub(1);
                if idx < context.menu_count {
                    return TuiAction::MenuSelect(idx).into();
                }
            }
            InputResult::None
        }
        _ => InputResult::None,
    }
}

/// Key handling for worktree selection mode.
fn process_worktree_select_key(key: &KeyEvent, context: &InputContext) -> InputResult {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => TuiAction::CloseModal.into(),
        KeyCode::Up | KeyCode::Char('k') => TuiAction::WorktreeUp.into(),
        KeyCode::Down | KeyCode::Char('j') => TuiAction::WorktreeDown.into(),
        KeyCode::Enter | KeyCode::Char(' ') => {
            TuiAction::WorktreeSelect(context.worktree_selected).into()
        }
        _ => InputResult::None,
    }
}

/// Key handling for text input modes.
fn process_text_input_key(key: &KeyEvent) -> InputResult {
    match key.code {
        KeyCode::Esc => TuiAction::CloseModal.into(),
        KeyCode::Enter => TuiAction::InputSubmit.into(),
        KeyCode::Backspace => TuiAction::InputBackspace.into(),
        KeyCode::Char(c) => TuiAction::InputChar(c).into(),
        _ => InputResult::None,
    }
}

/// Key handling for close agent confirmation.
fn process_close_confirm_key(key: &KeyEvent) -> InputResult {
    match key.code {
        KeyCode::Esc | KeyCode::Char('n' | 'q') => TuiAction::CloseModal.into(),
        KeyCode::Char('y') | KeyCode::Enter => TuiAction::ConfirmCloseAgent.into(),
        KeyCode::Char('d') => TuiAction::ConfirmCloseAgentDeleteWorktree.into(),
        _ => InputResult::None,
    }
}

/// Key handling for connection code display.
fn process_connection_code_key(key: &KeyEvent) -> InputResult {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => TuiAction::CloseModal.into(),
        KeyCode::Char('c') => TuiAction::CopyConnectionUrl.into(),
        KeyCode::Char('r') => TuiAction::RegenerateConnectionCode.into(),
        _ => InputResult::None,
    }
}

/// Key handling for error display mode.
fn process_error_mode_key(key: &KeyEvent) -> InputResult {
    match key.code {
        // Any of these keys dismiss the error
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => TuiAction::CloseModal.into(),
        _ => InputResult::None,
    }
}

/// Process a mouse event into an input result.
fn process_mouse_event(mouse: MouseEvent, mode: AppMode) -> InputResult {
    // Only handle mouse in normal mode
    if mode != AppMode::Normal {
        return InputResult::None;
    }

    match mouse.kind {
        MouseEventKind::ScrollUp => TuiAction::ScrollUp(3).into(),
        MouseEventKind::ScrollDown => TuiAction::ScrollDown(3).into(),
        _ => InputResult::None,
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

    fn make_key_shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
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

    // === Normal Mode Tests ===

    #[test]
    fn test_ctrl_q_quits() {
        let context = default_context();
        let result = process_event(
            &Event::Key(make_key_ctrl(KeyCode::Char('q'))),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::Quit));
    }

    #[test]
    fn test_ctrl_p_opens_menu() {
        let context = default_context();
        let result = process_event(
            &Event::Key(make_key_ctrl(KeyCode::Char('p'))),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::OpenMenu));
    }

    #[test]
    fn test_ctrl_navigation_keys() {
        let context = default_context();

        let result = process_event(
            &Event::Key(make_key_ctrl(KeyCode::Char('j'))),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::SelectNext));

        let result = process_event(
            &Event::Key(make_key_ctrl(KeyCode::Char('k'))),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::SelectPrevious));
    }

    #[test]
    fn test_ctrl_bracket_toggles_pty() {
        let context = default_context();
        let result = process_event(
            &Event::Key(make_key_ctrl(KeyCode::Char(']'))),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::TogglePtyView));
    }

    #[test]
    fn test_plain_keys_forward_to_pty() {
        let context = default_context();

        // Plain 'q' should forward to PTY, not quit
        let result = process_event(
            &Event::Key(make_key(KeyCode::Char('q'))),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::PtyInput(vec![b'q']));

        // Plain 'j' should forward to PTY, not navigate
        let result = process_event(
            &Event::Key(make_key(KeyCode::Char('j'))),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::PtyInput(vec![b'j']));

        // Number keys should forward to PTY
        let result = process_event(
            &Event::Key(make_key(KeyCode::Char('3'))),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::PtyInput(vec![b'3']));
    }

    #[test]
    fn test_ctrl_c_forwards_to_pty() {
        let context = default_context();
        // Ctrl+C should forward as control character (0x03)
        let result = process_event(
            &Event::Key(make_key_ctrl(KeyCode::Char('c'))),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::PtyInput(vec![3]));
    }

    #[test]
    fn test_special_keys_forward_to_pty() {
        let context = default_context();

        let result = process_event(
            &Event::Key(make_key(KeyCode::Enter)),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::PtyInput(vec![b'\r']));

        let result = process_event(
            &Event::Key(make_key(KeyCode::Backspace)),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::PtyInput(vec![0x7f]));

        let result = process_event(
            &Event::Key(make_key(KeyCode::Up)),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::PtyInput(vec![0x1b, b'[', b'A']));
    }

    #[test]
    fn test_shift_scroll_keys() {
        let context = default_context();

        let result = process_event(
            &Event::Key(make_key_shift(KeyCode::PageUp)),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(
            result,
            InputResult::Action(TuiAction::ScrollUp(context.terminal_rows as usize / 2))
        );

        let result = process_event(
            &Event::Key(make_key_shift(KeyCode::Home)),
            &AppMode::Normal,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::ScrollToTop));
    }

    // === Menu Mode Tests ===

    #[test]
    fn test_menu_navigation() {
        let context = default_context();

        let result = process_event(&Event::Key(make_key(KeyCode::Up)), &AppMode::Menu, &context);
        assert_eq!(result, InputResult::Action(TuiAction::MenuUp));

        let result = process_event(
            &Event::Key(make_key(KeyCode::Down)),
            &AppMode::Menu,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::MenuDown));

        let result = process_event(
            &Event::Key(make_key(KeyCode::Enter)),
            &AppMode::Menu,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::MenuSelect(0)));
    }

    #[test]
    fn test_menu_number_shortcuts() {
        let context = default_context();

        let result = process_event(
            &Event::Key(make_key(KeyCode::Char('1'))),
            &AppMode::Menu,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::MenuSelect(0)));

        let result = process_event(
            &Event::Key(make_key(KeyCode::Char('4'))),
            &AppMode::Menu,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::MenuSelect(3)));

        // Out of bounds should be ignored
        let result = process_event(
            &Event::Key(make_key(KeyCode::Char('9'))),
            &AppMode::Menu,
            &context,
        );
        assert_eq!(result, InputResult::None);
    }

    #[test]
    fn test_escape_closes_modal() {
        let context = default_context();

        let result = process_event(
            &Event::Key(make_key(KeyCode::Esc)),
            &AppMode::Menu,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::CloseModal));

        let result = process_event(
            &Event::Key(make_key(KeyCode::Esc)),
            &AppMode::ConnectionCode,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::CloseModal));
    }

    // === Text Input Mode Tests ===

    #[test]
    fn test_text_input_mode() {
        let context = default_context();

        let result = process_event(
            &Event::Key(make_key(KeyCode::Char('a'))),
            &AppMode::NewAgentCreateWorktree,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::InputChar('a')));

        let result = process_event(
            &Event::Key(make_key(KeyCode::Backspace)),
            &AppMode::NewAgentPrompt,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::InputBackspace));

        let result = process_event(
            &Event::Key(make_key(KeyCode::Enter)),
            &AppMode::NewAgentPrompt,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::InputSubmit));
    }

    // === Connection Code Mode Tests ===

    #[test]
    fn test_connection_code_r_regenerates() {
        let context = default_context();
        let result = process_event(
            &Event::Key(make_key(KeyCode::Char('r'))),
            &AppMode::ConnectionCode,
            &context,
        );
        assert_eq!(
            result,
            InputResult::Action(TuiAction::RegenerateConnectionCode)
        );
    }

    #[test]
    fn test_connection_code_c_copies_url() {
        let context = default_context();
        let result = process_event(
            &Event::Key(make_key(KeyCode::Char('c'))),
            &AppMode::ConnectionCode,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::CopyConnectionUrl));
    }

    // === Close Confirm Mode Tests ===

    #[test]
    fn test_close_confirm_yes() {
        let context = default_context();
        let result = process_event(
            &Event::Key(make_key(KeyCode::Char('y'))),
            &AppMode::CloseAgentConfirm,
            &context,
        );
        assert_eq!(result, InputResult::Action(TuiAction::ConfirmCloseAgent));
    }

    #[test]
    fn test_close_confirm_delete_worktree() {
        let context = default_context();
        let result = process_event(
            &Event::Key(make_key(KeyCode::Char('d'))),
            &AppMode::CloseAgentConfirm,
            &context,
        );
        assert_eq!(
            result,
            InputResult::Action(TuiAction::ConfirmCloseAgentDeleteWorktree)
        );
    }

    // === Mouse Tests ===

    #[test]
    fn test_mouse_scroll() {
        let context = default_context();

        let mouse_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let result = process_event(&Event::Mouse(mouse_up), &AppMode::Normal, &context);
        assert_eq!(result, InputResult::Action(TuiAction::ScrollUp(3)));

        let mouse_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let result = process_event(&Event::Mouse(mouse_down), &AppMode::Normal, &context);
        assert_eq!(result, InputResult::Action(TuiAction::ScrollDown(3)));
    }

    #[test]
    fn test_mouse_ignored_in_menu() {
        let context = default_context();

        let mouse_up = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let result = process_event(&Event::Mouse(mouse_up), &AppMode::Menu, &context);
        assert_eq!(result, InputResult::None);
    }

    // === Resize Tests ===

    #[test]
    fn test_resize_event() {
        let context = default_context();
        let result = process_event(&Event::Resize(120, 40), &AppMode::Normal, &context);

        // Resize should use the calculated inner area
        let (expected_rows, expected_cols) = terminal_widget_inner_area(120, 40);
        assert_eq!(
            result,
            InputResult::Resize {
                rows: expected_rows,
                cols: expected_cols,
            }
        );
    }

    // === Key Release Ignored ===

    #[test]
    fn test_key_release_ignored() {
        let context = default_context();
        let mut key = make_key(KeyCode::Char('a'));
        key.kind = KeyEventKind::Release;

        let result = process_event(&Event::Key(key), &AppMode::Normal, &context);
        assert_eq!(result, InputResult::None);
    }
}
