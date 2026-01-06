//! Input handling for the TUI application.
//!
//! This module provides keyboard input handling for the botster-hub TUI,
//! separating input logic from the main application state.
//!
//! # Overview
//!
//! Input is processed based on the current application mode:
//! - **Normal**: Regular agent interaction and navigation
//! - **Menu**: Menu navigation and selection
//! - **NewAgentSelectWorktree**: Worktree selection for new agents
//! - **NewAgentCreateWorktree**: Text input for new branch names
//! - **NewAgentPrompt**: Text input for agent prompts
//! - **CloseAgentConfirm**: Confirmation dialog for closing agents

// Rust guideline compliant 2025-01

use crossterm::event::{KeyCode, KeyModifiers};

use super::AppMode;

/// Result of handling a key event.
///
/// Indicates what action the application should take after processing input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputAction {
    /// No action needed, input was consumed.
    None,
    /// Request application quit.
    Quit,
    /// Open the menu.
    OpenMenu,
    /// Close menu/modal and return to normal mode.
    CloseModal,
    /// Navigate to previous agent.
    PreviousAgent,
    /// Navigate to next agent.
    NextAgent,
    /// Kill the selected agent.
    KillAgent,
    /// Toggle PTY view (CLI/Server).
    TogglePtyView,
    /// Scroll up in the terminal buffer.
    ScrollUp(usize),
    /// Scroll down in the terminal buffer.
    ScrollDown(usize),
    /// Scroll to top of buffer.
    ScrollToTop,
    /// Scroll to bottom of buffer.
    ScrollToBottom,
    /// Forward input bytes to the active PTY.
    ForwardToPty(Vec<u8>),
    /// Menu navigation up.
    MenuUp,
    /// Menu navigation down.
    MenuDown,
    /// Execute menu selection.
    MenuSelect(usize),
    /// Worktree list navigation up.
    WorktreeUp,
    /// Worktree list navigation down.
    WorktreeDown,
    /// Select worktree at index (0 = "Create New").
    WorktreeSelect(usize),
    /// Add character to input buffer.
    InputChar(char),
    /// Remove last character from input buffer.
    InputBackspace,
    /// Submit input buffer (create worktree or spawn agent).
    InputSubmit,
    /// Close agent without deleting worktree.
    CloseAgentKeepWorktree,
    /// Close agent and delete worktree.
    CloseAgentDeleteWorktree,
    /// Copy connection URL to clipboard.
    CopyConnectionUrl,
}

/// Converts a key event to bytes for forwarding to a PTY.
///
/// Handles special keys like arrows, function keys, and control sequences.
///
/// # Arguments
///
/// * `code` - The key code to convert
/// * `modifiers` - Key modifiers (Ctrl, Shift, etc.)
///
/// # Returns
///
/// The byte sequence to send to the PTY, or `None` if the key shouldn't be forwarded.
pub fn key_to_pty_bytes(code: KeyCode, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    match code {
        KeyCode::Char(c) => {
            if modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii_alphabetic() {
                // Send control character (Ctrl+A = 1, Ctrl+B = 2, etc.)
                let ctrl_code = (c.to_ascii_uppercase() as u8) - b'@';
                Some(vec![ctrl_code])
            } else {
                Some(c.to_string().into_bytes())
            }
        }
        KeyCode::Backspace => Some(vec![8]),
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Esc => Some(vec![27]),
        KeyCode::Left => Some(vec![27, 91, 68]),
        KeyCode::Right => Some(vec![27, 91, 67]),
        KeyCode::Up => Some(vec![27, 91, 65]),
        KeyCode::Down => Some(vec![27, 91, 66]),
        KeyCode::Home => Some(vec![27, 91, 72]),
        KeyCode::End => Some(vec![27, 91, 70]),
        KeyCode::PageUp => Some(vec![27, 91, 53, 126]),
        KeyCode::PageDown => Some(vec![27, 91, 54, 126]),
        KeyCode::Tab => Some(vec![9]),
        KeyCode::BackTab => Some(vec![27, 91, 90]),
        KeyCode::Delete => Some(vec![27, 91, 51, 126]),
        KeyCode::Insert => Some(vec![27, 91, 50, 126]),
        _ => None,
    }
}

/// Handles key input in normal mode.
///
/// # Arguments
///
/// * `code` - The key code pressed
/// * `modifiers` - Key modifiers (Ctrl, Shift, etc.)
/// * `terminal_rows` - Number of terminal rows (for scroll calculations)
///
/// # Returns
///
/// The action to take in response to the key press.
pub fn handle_normal_mode_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    terminal_rows: u16,
) -> InputAction {
    match code {
        KeyCode::Char('q') if modifiers.contains(KeyModifiers::CONTROL) => InputAction::Quit,
        KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => InputAction::OpenMenu,
        KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => InputAction::NextAgent,
        KeyCode::Char('k') if modifiers.contains(KeyModifiers::CONTROL) => {
            InputAction::PreviousAgent
        }
        KeyCode::Char('x') if modifiers.contains(KeyModifiers::CONTROL) => InputAction::KillAgent,
        KeyCode::Char('t') if modifiers.contains(KeyModifiers::CONTROL) => {
            InputAction::TogglePtyView
        }
        KeyCode::PageUp if modifiers.contains(KeyModifiers::SHIFT) => {
            InputAction::ScrollUp(terminal_rows as usize / 2)
        }
        KeyCode::PageDown if modifiers.contains(KeyModifiers::SHIFT) => {
            InputAction::ScrollDown(terminal_rows as usize / 2)
        }
        KeyCode::Home if modifiers.contains(KeyModifiers::SHIFT) => InputAction::ScrollToTop,
        KeyCode::End if modifiers.contains(KeyModifiers::SHIFT) => InputAction::ScrollToBottom,
        _ => {
            // Forward to PTY
            if let Some(bytes) = key_to_pty_bytes(code, modifiers) {
                InputAction::ForwardToPty(bytes)
            } else {
                InputAction::None
            }
        }
    }
}

/// Handles key input in menu mode.
///
/// # Arguments
///
/// * `code` - The key code pressed
/// * `current_selection` - Currently selected menu item index
/// * `menu_size` - Total number of menu items
///
/// # Returns
///
/// The action to take in response to the key press.
pub fn handle_menu_mode_key(
    code: KeyCode,
    current_selection: usize,
    menu_size: usize,
) -> InputAction {
    match code {
        KeyCode::Esc | KeyCode::Char('q') => InputAction::CloseModal,
        KeyCode::Up | KeyCode::Char('k') => {
            if current_selection > 0 {
                InputAction::MenuUp
            } else {
                InputAction::None
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if current_selection < menu_size.saturating_sub(1) {
                InputAction::MenuDown
            } else {
                InputAction::None
            }
        }
        KeyCode::Enter => InputAction::MenuSelect(current_selection),
        _ => InputAction::None,
    }
}

/// Handles key input in worktree selection mode.
///
/// # Arguments
///
/// * `code` - The key code pressed
/// * `current_selection` - Currently selected worktree index
/// * `total_items` - Total number of items (including "Create New")
///
/// # Returns
///
/// The action to take in response to the key press.
pub fn handle_worktree_select_key(
    code: KeyCode,
    current_selection: usize,
    total_items: usize,
) -> InputAction {
    match code {
        KeyCode::Esc | KeyCode::Char('q') => InputAction::CloseModal,
        KeyCode::Up | KeyCode::Char('k') => {
            if current_selection > 0 {
                InputAction::WorktreeUp
            } else {
                InputAction::None
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if current_selection < total_items.saturating_sub(1) {
                InputAction::WorktreeDown
            } else {
                InputAction::None
            }
        }
        KeyCode::Enter => InputAction::WorktreeSelect(current_selection),
        _ => InputAction::None,
    }
}

/// Handles key input in create worktree mode (branch name input).
///
/// # Arguments
///
/// * `code` - The key code pressed
///
/// # Returns
///
/// The action to take in response to the key press.
pub fn handle_create_worktree_key(code: KeyCode) -> InputAction {
    match code {
        KeyCode::Esc => InputAction::CloseModal,
        KeyCode::Char(c) if !c.is_control() && c != ' ' => InputAction::InputChar(c),
        KeyCode::Backspace => InputAction::InputBackspace,
        KeyCode::Enter => InputAction::InputSubmit,
        _ => InputAction::None,
    }
}

/// Handles key input in prompt input mode.
///
/// # Arguments
///
/// * `code` - The key code pressed
///
/// # Returns
///
/// The action to take in response to the key press.
pub fn handle_prompt_input_key(code: KeyCode) -> InputAction {
    match code {
        KeyCode::Esc => InputAction::CloseModal,
        KeyCode::Char(c) => InputAction::InputChar(c),
        KeyCode::Backspace => InputAction::InputBackspace,
        KeyCode::Enter => InputAction::InputSubmit,
        _ => InputAction::None,
    }
}

/// Handles key input in close agent confirmation mode.
///
/// # Arguments
///
/// * `code` - The key code pressed
///
/// # Returns
///
/// The action to take in response to the key press.
pub fn handle_close_confirm_key(code: KeyCode) -> InputAction {
    match code {
        KeyCode::Esc | KeyCode::Char('n') => InputAction::CloseModal,
        KeyCode::Char('y') => InputAction::CloseAgentKeepWorktree,
        KeyCode::Char('d') => InputAction::CloseAgentDeleteWorktree,
        _ => InputAction::None,
    }
}

/// Handles key input in connection code display mode.
///
/// # Arguments
///
/// * `code` - The key code pressed
///
/// # Returns
///
/// The action to take in response to the key press.
pub fn handle_connection_code_key(code: KeyCode) -> InputAction {
    match code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => InputAction::CloseModal,
        KeyCode::Char('c') => InputAction::CopyConnectionUrl,
        _ => InputAction::None,
    }
}

/// Parses raw terminal input bytes into KeyCode and KeyModifiers.
///
/// This is used to convert browser input (which comes as raw escape sequences)
/// into structured key events for processing.
///
/// # Arguments
///
/// * `input` - Raw input string from browser terminal
///
/// # Returns
///
/// A vector of (KeyCode, KeyModifiers) pairs parsed from the input.
pub fn parse_terminal_input(input: &str) -> Vec<(KeyCode, KeyModifiers)> {
    let bytes = input.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 27 && i + 2 < bytes.len() && bytes[i + 1] == 91 {
            // ESC [ sequence - arrow keys, function keys, etc.
            match bytes.get(i + 2) {
                Some(65) => {
                    // Up arrow
                    result.push((KeyCode::Up, KeyModifiers::NONE));
                    i += 3;
                }
                Some(66) => {
                    // Down arrow
                    result.push((KeyCode::Down, KeyModifiers::NONE));
                    i += 3;
                }
                Some(67) => {
                    // Right arrow
                    result.push((KeyCode::Right, KeyModifiers::NONE));
                    i += 3;
                }
                Some(68) => {
                    // Left arrow
                    result.push((KeyCode::Left, KeyModifiers::NONE));
                    i += 3;
                }
                Some(72) => {
                    // Home
                    result.push((KeyCode::Home, KeyModifiers::NONE));
                    i += 3;
                }
                Some(70) => {
                    // End
                    result.push((KeyCode::End, KeyModifiers::NONE));
                    i += 3;
                }
                Some(90) => {
                    // Shift+Tab (BackTab)
                    result.push((KeyCode::BackTab, KeyModifiers::SHIFT));
                    i += 3;
                }
                Some(51) if bytes.get(i + 3) == Some(&126) => {
                    // Delete
                    result.push((KeyCode::Delete, KeyModifiers::NONE));
                    i += 4;
                }
                Some(50) if bytes.get(i + 3) == Some(&126) => {
                    // Insert
                    result.push((KeyCode::Insert, KeyModifiers::NONE));
                    i += 4;
                }
                Some(53) if bytes.get(i + 3) == Some(&126) => {
                    // Page Up
                    result.push((KeyCode::PageUp, KeyModifiers::NONE));
                    i += 4;
                }
                Some(54) if bytes.get(i + 3) == Some(&126) => {
                    // Page Down
                    result.push((KeyCode::PageDown, KeyModifiers::NONE));
                    i += 4;
                }
                _ => {
                    // Unknown escape sequence, skip ESC
                    result.push((KeyCode::Esc, KeyModifiers::NONE));
                    i += 1;
                }
            }
        } else if bytes[i] == 27 {
            // Bare ESC
            result.push((KeyCode::Esc, KeyModifiers::NONE));
            i += 1;
        } else if bytes[i] == 13 || bytes[i] == 10 {
            // Enter (CR or LF)
            result.push((KeyCode::Enter, KeyModifiers::NONE));
            i += 1;
        } else if bytes[i] == 9 {
            // Tab
            result.push((KeyCode::Tab, KeyModifiers::NONE));
            i += 1;
        } else if bytes[i] == 127 || bytes[i] == 8 {
            // Backspace (DEL or BS)
            result.push((KeyCode::Backspace, KeyModifiers::NONE));
            i += 1;
        } else if bytes[i] < 32 {
            // Control character (Ctrl+A = 1, Ctrl+B = 2, etc.)
            let char_code = bytes[i] + b'@';
            if char_code.is_ascii_alphabetic() {
                result.push((
                    KeyCode::Char((char_code as char).to_ascii_lowercase()),
                    KeyModifiers::CONTROL,
                ));
            }
            i += 1;
        } else {
            // Regular character
            if let Some(c) = char::from_u32(bytes[i] as u32) {
                result.push((KeyCode::Char(c), KeyModifiers::NONE));
            }
            i += 1;
        }
    }

    result
}

/// Dispatches key handling to the appropriate handler based on mode.
///
/// # Arguments
///
/// * `mode` - Current application mode
/// * `code` - The key code pressed
/// * `modifiers` - Key modifiers (Ctrl, Shift, etc.)
/// * `terminal_rows` - Number of terminal rows
/// * `menu_selection` - Current menu selection index
/// * `menu_size` - Total menu items
/// * `worktree_selection` - Current worktree selection index
/// * `worktree_count` - Total available worktrees (not including "Create New")
///
/// # Returns
///
/// The action to take in response to the key press.
pub fn dispatch_key_event(
    mode: &AppMode,
    code: KeyCode,
    modifiers: KeyModifiers,
    terminal_rows: u16,
    menu_selection: usize,
    menu_size: usize,
    worktree_selection: usize,
    worktree_count: usize,
) -> InputAction {
    match mode {
        AppMode::Normal => handle_normal_mode_key(code, modifiers, terminal_rows),
        AppMode::Menu => handle_menu_mode_key(code, menu_selection, menu_size),
        AppMode::NewAgentSelectWorktree => {
            // Total items = 1 (Create New) + worktree_count
            handle_worktree_select_key(code, worktree_selection, worktree_count + 1)
        }
        AppMode::NewAgentCreateWorktree => handle_create_worktree_key(code),
        AppMode::NewAgentPrompt => handle_prompt_input_key(code),
        AppMode::CloseAgentConfirm => handle_close_confirm_key(code),
        AppMode::ConnectionCode => handle_connection_code_key(code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_to_pty_bytes_regular_char() {
        let bytes = key_to_pty_bytes(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(bytes, Some(vec![b'a']));
    }

    #[test]
    fn test_key_to_pty_bytes_control_char() {
        let bytes = key_to_pty_bytes(KeyCode::Char('c'), KeyModifiers::CONTROL);
        // Ctrl+C = 3
        assert_eq!(bytes, Some(vec![3]));
    }

    #[test]
    fn test_key_to_pty_bytes_enter() {
        let bytes = key_to_pty_bytes(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(bytes, Some(vec![b'\r']));
    }

    #[test]
    fn test_key_to_pty_bytes_arrow_keys() {
        assert_eq!(
            key_to_pty_bytes(KeyCode::Up, KeyModifiers::NONE),
            Some(vec![27, 91, 65])
        );
        assert_eq!(
            key_to_pty_bytes(KeyCode::Down, KeyModifiers::NONE),
            Some(vec![27, 91, 66])
        );
        assert_eq!(
            key_to_pty_bytes(KeyCode::Right, KeyModifiers::NONE),
            Some(vec![27, 91, 67])
        );
        assert_eq!(
            key_to_pty_bytes(KeyCode::Left, KeyModifiers::NONE),
            Some(vec![27, 91, 68])
        );
    }

    #[test]
    fn test_normal_mode_quit() {
        let action = handle_normal_mode_key(KeyCode::Char('q'), KeyModifiers::CONTROL, 24);
        assert_eq!(action, InputAction::Quit);
    }

    #[test]
    fn test_normal_mode_open_menu() {
        let action = handle_normal_mode_key(KeyCode::Char('p'), KeyModifiers::CONTROL, 24);
        assert_eq!(action, InputAction::OpenMenu);
    }

    #[test]
    fn test_normal_mode_navigation() {
        let next = handle_normal_mode_key(KeyCode::Char('j'), KeyModifiers::CONTROL, 24);
        assert_eq!(next, InputAction::NextAgent);

        let prev = handle_normal_mode_key(KeyCode::Char('k'), KeyModifiers::CONTROL, 24);
        assert_eq!(prev, InputAction::PreviousAgent);
    }

    #[test]
    fn test_normal_mode_scroll() {
        let up = handle_normal_mode_key(KeyCode::PageUp, KeyModifiers::SHIFT, 24);
        assert_eq!(up, InputAction::ScrollUp(12));

        let down = handle_normal_mode_key(KeyCode::PageDown, KeyModifiers::SHIFT, 24);
        assert_eq!(down, InputAction::ScrollDown(12));
    }

    #[test]
    fn test_normal_mode_forward_to_pty() {
        let action = handle_normal_mode_key(KeyCode::Char('x'), KeyModifiers::NONE, 24);
        assert_eq!(action, InputAction::ForwardToPty(vec![b'x']));
    }

    #[test]
    fn test_menu_mode_navigation() {
        let up = handle_menu_mode_key(KeyCode::Up, 1, 3);
        assert_eq!(up, InputAction::MenuUp);

        let down = handle_menu_mode_key(KeyCode::Down, 1, 3);
        assert_eq!(down, InputAction::MenuDown);

        // At top, can't go up
        let at_top = handle_menu_mode_key(KeyCode::Up, 0, 3);
        assert_eq!(at_top, InputAction::None);

        // At bottom, can't go down
        let at_bottom = handle_menu_mode_key(KeyCode::Down, 2, 3);
        assert_eq!(at_bottom, InputAction::None);
    }

    #[test]
    fn test_menu_mode_select() {
        let action = handle_menu_mode_key(KeyCode::Enter, 1, 3);
        assert_eq!(action, InputAction::MenuSelect(1));
    }

    #[test]
    fn test_menu_mode_escape() {
        let esc = handle_menu_mode_key(KeyCode::Esc, 0, 3);
        assert_eq!(esc, InputAction::CloseModal);

        let q = handle_menu_mode_key(KeyCode::Char('q'), 0, 3);
        assert_eq!(q, InputAction::CloseModal);
    }

    #[test]
    fn test_worktree_select_navigation() {
        let up = handle_worktree_select_key(KeyCode::Up, 1, 5);
        assert_eq!(up, InputAction::WorktreeUp);

        let down = handle_worktree_select_key(KeyCode::Down, 1, 5);
        assert_eq!(down, InputAction::WorktreeDown);
    }

    #[test]
    fn test_worktree_select_enter() {
        let action = handle_worktree_select_key(KeyCode::Enter, 2, 5);
        assert_eq!(action, InputAction::WorktreeSelect(2));
    }

    #[test]
    fn test_create_worktree_input() {
        let char_input = handle_create_worktree_key(KeyCode::Char('a'));
        assert_eq!(char_input, InputAction::InputChar('a'));

        let backspace = handle_create_worktree_key(KeyCode::Backspace);
        assert_eq!(backspace, InputAction::InputBackspace);

        let enter = handle_create_worktree_key(KeyCode::Enter);
        assert_eq!(enter, InputAction::InputSubmit);

        // Space not allowed in branch names
        let space = handle_create_worktree_key(KeyCode::Char(' '));
        assert_eq!(space, InputAction::None);
    }

    #[test]
    fn test_prompt_input() {
        // Spaces allowed in prompts
        let space = handle_prompt_input_key(KeyCode::Char(' '));
        assert_eq!(space, InputAction::InputChar(' '));

        let char_input = handle_prompt_input_key(KeyCode::Char('a'));
        assert_eq!(char_input, InputAction::InputChar('a'));
    }

    #[test]
    fn test_close_confirm_responses() {
        let yes = handle_close_confirm_key(KeyCode::Char('y'));
        assert_eq!(yes, InputAction::CloseAgentKeepWorktree);

        let delete = handle_close_confirm_key(KeyCode::Char('d'));
        assert_eq!(delete, InputAction::CloseAgentDeleteWorktree);

        let no = handle_close_confirm_key(KeyCode::Char('n'));
        assert_eq!(no, InputAction::CloseModal);
    }

    #[test]
    fn test_dispatch_key_event_normal_mode() {
        let action = dispatch_key_event(
            &AppMode::Normal,
            KeyCode::Char('q'),
            KeyModifiers::CONTROL,
            24,
            0,
            3,
            0,
            5,
        );
        assert_eq!(action, InputAction::Quit);
    }

    #[test]
    fn test_dispatch_key_event_menu_mode() {
        let action = dispatch_key_event(
            &AppMode::Menu,
            KeyCode::Enter,
            KeyModifiers::NONE,
            24,
            1,
            3,
            0,
            5,
        );
        assert_eq!(action, InputAction::MenuSelect(1));
    }

    #[test]
    fn test_connection_code_close() {
        let esc = handle_connection_code_key(KeyCode::Esc);
        assert_eq!(esc, InputAction::CloseModal);

        let q = handle_connection_code_key(KeyCode::Char('q'));
        assert_eq!(q, InputAction::CloseModal);

        let enter = handle_connection_code_key(KeyCode::Enter);
        assert_eq!(enter, InputAction::CloseModal);
    }

    #[test]
    fn test_connection_code_ignores_other_keys() {
        let action = handle_connection_code_key(KeyCode::Char('a'));
        assert_eq!(action, InputAction::None);
    }

    #[test]
    fn test_dispatch_key_event_connection_code_mode() {
        let action = dispatch_key_event(
            &AppMode::ConnectionCode,
            KeyCode::Esc,
            KeyModifiers::NONE,
            24,
            0,
            3,
            0,
            5,
        );
        assert_eq!(action, InputAction::CloseModal);
    }
}
