//! Terminal input handling for the TUI.
//!
//! Provides two pure functions that bridge crossterm key events to the Lua
//! keybinding system:
//!
//! - [`key_event_to_descriptor`] — converts a crossterm `KeyEvent` into a
//!   string descriptor that Lua binding tables use as keys (e.g., `"ctrl+p"`,
//!   `"shift+enter"`, `"a"`).
//!
//! - [`key_to_pty_bytes`] — converts a `KeyEvent` to raw bytes suitable for
//!   PTY forwarding. Used as fallback when Lua returns `nil` (unbound key in
//!   Normal mode).
//!
//! # Architecture
//!
//! ```text
//! crossterm::KeyEvent
//!     |
//!     +---> key_event_to_descriptor() ---> Lua handle_key()
//!     |                                        |
//!     |     Some(action)  <--------------------+----> nil
//!     |         |                                      |
//!     |    TuiRunner.handle_tui_action()    key_to_pty_bytes()
//!     |                                        |
//!     |                                   PTY forwarding
//!     |
//!     +---> Mouse/Resize handled in Rust (unchanged)
//! ```
//!
//! # Key Descriptor Format
//!
//! Modifiers are prefix-sorted: `ctrl+shift+alt+<key>`. The key name is
//! always lowercase. Examples:
//! - `"a"`, `"enter"`, `"escape"`, `"pageup"`, `"backspace"`
//! - `"ctrl+c"`, `"ctrl+p"`, `"shift+enter"`, `"shift+pageup"`
//! - `"ctrl+]"` (bracket literal)
//!
//! # Safety
//!
//! `Ctrl+Q` is intercepted by TuiRunner *before* calling into Lua. Even if
//! Lua is broken, the user can always exit.

// Rust guideline compliant 2026-02

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Convert a crossterm `KeyEvent` to a Lua key descriptor string.
///
/// The descriptor format is `modifier+modifier+keyname` where modifiers
/// are sorted as `ctrl`, `shift`, `alt`. The key name is always lowercase.
///
/// # Examples
///
/// ```ignore
/// // Plain key
/// key_event_to_descriptor(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))
///     == "a"
///
/// // Ctrl combo
/// key_event_to_descriptor(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL))
///     == "ctrl+p"
///
/// // Shift+Enter
/// key_event_to_descriptor(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
///     == "shift+enter"
/// ```
#[must_use]
pub fn key_event_to_descriptor(key: &KeyEvent) -> String {
    let mut parts = Vec::new();

    // Modifiers in canonical order: ctrl, shift, alt
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl");
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("shift");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt");
    }

    let key_name = keycode_to_name(&key.code, key.modifiers);
    parts.push(&key_name);

    parts.join("+")
}

/// Convert a crossterm `KeyEvent` to raw PTY bytes.
///
/// Returns `None` for key events that have no PTY byte representation
/// (e.g., function keys without ANSI sequences). This is the fallback
/// for unbound keys in Normal mode — Lua returned `nil`, so we forward
/// the raw keystroke to the PTY.
///
/// Handles modifier-aware encoding:
/// - `Ctrl+<char>` sends the control byte (Ctrl+A = 0x01, Ctrl+C = 0x03)
/// - Arrow keys, Home, End, etc. send ANSI escape sequences
/// - Enter always sends `\r` regardless of modifiers
#[must_use]
pub fn key_to_pty_bytes(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        KeyCode::Char(c) if ctrl => {
            // Control character: Ctrl+A = 1, Ctrl+C = 3, etc.
            let ctrl_byte = (c.to_ascii_uppercase() as u8).wrapping_sub(b'@');
            Some(vec![ctrl_byte])
        }
        KeyCode::Char(c) => Some(c.to_string().into_bytes()),
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => Some(vec![0x1b, b'[', b'A']),
        KeyCode::Down => Some(vec![0x1b, b'[', b'B']),
        KeyCode::Right => Some(vec![0x1b, b'[', b'C']),
        KeyCode::Left => Some(vec![0x1b, b'[', b'D']),
        KeyCode::Home => Some(vec![0x1b, b'[', b'H']),
        KeyCode::End => Some(vec![0x1b, b'[', b'F']),
        KeyCode::PageUp => Some(vec![0x1b, b'[', b'5', b'~']),
        KeyCode::PageDown => Some(vec![0x1b, b'[', b'6', b'~']),
        KeyCode::Delete => Some(vec![0x1b, b'[', b'3', b'~']),
        KeyCode::Insert => Some(vec![0x1b, b'[', b'2', b'~']),
        KeyCode::BackTab => Some(vec![0x1b, b'[', b'Z']),
        _ => None,
    }
}

/// Map a `KeyCode` to its lowercase string name for the descriptor.
///
/// For `Char` codes, the character itself is used (lowercase). Special
/// keys use their canonical names (e.g., `"enter"`, `"pageup"`).
fn keycode_to_name(code: &KeyCode, modifiers: KeyModifiers) -> String {
    match code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => {
            // When Ctrl is held, crossterm may report uppercase. Normalize.
            if modifiers.contains(KeyModifiers::CONTROL) {
                c.to_ascii_lowercase().to_string()
            } else {
                c.to_string()
            }
        }
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "escape".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => "backtab".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Insert => "insert".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Null => "null".to_string(),
        _ => format!("{code:?}").to_lowercase(),
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

    // === Descriptor Tests ===

    #[test]
    fn test_plain_char_descriptor() {
        assert_eq!(key_event_to_descriptor(&make_key(KeyCode::Char('a'))), "a");
        assert_eq!(key_event_to_descriptor(&make_key(KeyCode::Char('z'))), "z");
        assert_eq!(key_event_to_descriptor(&make_key(KeyCode::Char('1'))), "1");
    }

    #[test]
    fn test_ctrl_descriptor() {
        assert_eq!(
            key_event_to_descriptor(&make_key_ctrl(KeyCode::Char('p'))),
            "ctrl+p"
        );
        assert_eq!(
            key_event_to_descriptor(&make_key_ctrl(KeyCode::Char('c'))),
            "ctrl+c"
        );
        assert_eq!(
            key_event_to_descriptor(&make_key_ctrl(KeyCode::Char(']'))),
            "ctrl+]"
        );
    }

    #[test]
    fn test_shift_descriptor() {
        assert_eq!(
            key_event_to_descriptor(&make_key_shift(KeyCode::Enter)),
            "shift+enter"
        );
        assert_eq!(
            key_event_to_descriptor(&make_key_shift(KeyCode::PageUp)),
            "shift+pageup"
        );
        assert_eq!(
            key_event_to_descriptor(&make_key_shift(KeyCode::Home)),
            "shift+home"
        );
    }

    #[test]
    fn test_special_key_descriptors() {
        assert_eq!(key_event_to_descriptor(&make_key(KeyCode::Enter)), "enter");
        assert_eq!(key_event_to_descriptor(&make_key(KeyCode::Esc)), "escape");
        assert_eq!(
            key_event_to_descriptor(&make_key(KeyCode::Backspace)),
            "backspace"
        );
        assert_eq!(key_event_to_descriptor(&make_key(KeyCode::Up)), "up");
        assert_eq!(key_event_to_descriptor(&make_key(KeyCode::Down)), "down");
        assert_eq!(
            key_event_to_descriptor(&make_key(KeyCode::PageUp)),
            "pageup"
        );
        assert_eq!(
            key_event_to_descriptor(&make_key(KeyCode::PageDown)),
            "pagedown"
        );
    }

    #[test]
    fn test_ctrl_shift_combined() {
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        assert_eq!(key_event_to_descriptor(&key), "ctrl+shift+a");
    }

    #[test]
    fn test_space_descriptor() {
        assert_eq!(
            key_event_to_descriptor(&make_key(KeyCode::Char(' '))),
            "space"
        );
    }

    // === PTY Bytes Tests ===

    #[test]
    fn test_pty_bytes_plain_char() {
        assert_eq!(
            key_to_pty_bytes(&make_key(KeyCode::Char('a'))),
            Some(vec![b'a'])
        );
    }

    #[test]
    fn test_pty_bytes_ctrl_char() {
        // Ctrl+C = 0x03
        assert_eq!(
            key_to_pty_bytes(&make_key_ctrl(KeyCode::Char('c'))),
            Some(vec![3])
        );
    }

    #[test]
    fn test_pty_bytes_enter() {
        assert_eq!(
            key_to_pty_bytes(&make_key(KeyCode::Enter)),
            Some(vec![b'\r'])
        );
        // Shift+Enter should also produce \r
        assert_eq!(
            key_to_pty_bytes(&make_key_shift(KeyCode::Enter)),
            Some(vec![b'\r'])
        );
    }

    #[test]
    fn test_pty_bytes_special_keys() {
        assert_eq!(
            key_to_pty_bytes(&make_key(KeyCode::Backspace)),
            Some(vec![0x7f])
        );
        assert_eq!(
            key_to_pty_bytes(&make_key(KeyCode::Up)),
            Some(vec![0x1b, b'[', b'A'])
        );
        assert_eq!(
            key_to_pty_bytes(&make_key(KeyCode::Delete)),
            Some(vec![0x1b, b'[', b'3', b'~'])
        );
    }

    #[test]
    fn test_pty_bytes_tab() {
        assert_eq!(
            key_to_pty_bytes(&make_key(KeyCode::Tab)),
            Some(vec![b'\t'])
        );
    }
}
