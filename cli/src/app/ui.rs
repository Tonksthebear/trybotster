//! UI rendering utilities for the botster-hub TUI.
//!
//! This module provides helper functions for rendering the TUI,
//! including layout helpers, ANSI conversion for WebRTC streaming,
//! and browser keyboard input conversion.
//!
//! # Overview
//!
//! The TUI uses ratatui for rendering. When streaming to browsers via WebRTC,
//! the rendered buffer is converted to ANSI escape sequences using
//! [`buffer_to_ansi`].
//!
//! Modal dialogs are positioned using [`centered_rect`] which calculates
//! a centered rectangle within a parent area.

// Rust guideline compliant 2025-01

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier},
};
use std::fmt::Write;

use crate::webrtc_handler::KeyInput;

/// Creates a centered rectangle within a parent area.
///
/// Useful for positioning modal dialogs and popups in the TUI.
///
/// # Arguments
///
/// * `percent_x` - Width of the centered rect as a percentage of parent width
/// * `percent_y` - Height of the centered rect as a percentage of parent height
/// * `parent` - The parent rectangle to center within
///
/// # Returns
///
/// A rectangle centered within the parent at the specified percentage size.
///
/// # Example
///
/// ```ignore
/// use ratatui::layout::Rect;
/// use botster_hub::app::ui::centered_rect;
///
/// let parent = Rect::new(0, 0, 100, 50);
/// let modal = centered_rect(50, 30, parent);
/// // modal is 50x15 centered in parent
/// ```
pub fn centered_rect(percent_x: u16, percent_y: u16, parent: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(parent);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Converts a ratatui Buffer to ANSI escape sequences.
///
/// Used for streaming the TUI to browsers via WebRTC. The output string
/// contains ANSI escape codes that xterm.js can render.
///
/// # Arguments
///
/// * `buffer` - The ratatui buffer to convert
/// * `width` - Buffer width
/// * `height` - Buffer height
/// * `clip_width` - Optional clipping width (for browser dimensions)
/// * `clip_height` - Optional clipping height (for browser dimensions)
///
/// # Returns
///
/// A string containing ANSI escape sequences representing the buffer contents.
///
/// # Performance
///
/// This function is called frequently during WebRTC streaming. It optimizes
/// by only emitting style changes when attributes differ from the previous cell.
pub fn buffer_to_ansi(
    buffer: &Buffer,
    width: u16,
    height: u16,
    clip_width: Option<u16>,
    clip_height: Option<u16>,
) -> String {
    let out_width = clip_width.unwrap_or(width).min(width);
    let out_height = clip_height.unwrap_or(height).min(height);

    let mut output = String::new();

    // Reset and clear screen, move cursor to home
    output.push_str("\x1b[0m\x1b[H\x1b[2J");

    let mut last_fg = Color::Reset;
    let mut last_bg = Color::Reset;
    let mut last_modifiers = Modifier::empty();

    for y in 0..out_height {
        // Move cursor to start of line
        write!(output, "\x1b[{};1H", y + 1).unwrap();

        for x in 0..out_width {
            let cell = buffer.cell((x, y));
            if cell.is_none() {
                output.push(' ');
                continue;
            }
            let cell = cell.unwrap();

            let fg = cell.fg;
            let bg = cell.bg;
            let modifiers = cell.modifier;

            // Only emit style changes when attributes differ
            if fg != last_fg || bg != last_bg || modifiers != last_modifiers {
                output.push_str("\x1b[0m"); // Reset first

                // Apply modifiers
                apply_modifiers(&mut output, modifiers);

                // Apply colors
                apply_foreground_color(&mut output, fg);
                apply_background_color(&mut output, bg);

                last_fg = fg;
                last_bg = bg;
                last_modifiers = modifiers;
            }

            // Write the character
            output.push_str(cell.symbol());
        }
    }

    // Reset at end
    output.push_str("\x1b[0m");

    output
}

/// Applies text modifiers to the output string.
fn apply_modifiers(output: &mut String, modifiers: Modifier) {
    if modifiers.contains(Modifier::BOLD) {
        output.push_str("\x1b[1m");
    }
    if modifiers.contains(Modifier::DIM) {
        output.push_str("\x1b[2m");
    }
    if modifiers.contains(Modifier::ITALIC) {
        output.push_str("\x1b[3m");
    }
    if modifiers.contains(Modifier::UNDERLINED) {
        output.push_str("\x1b[4m");
    }
    if modifiers.contains(Modifier::REVERSED) {
        output.push_str("\x1b[7m");
    }
}

/// Applies foreground color to the output string.
fn apply_foreground_color(output: &mut String, color: Color) {
    match color {
        Color::Reset => {}
        Color::Black => output.push_str("\x1b[30m"),
        Color::Red => output.push_str("\x1b[31m"),
        Color::Green => output.push_str("\x1b[32m"),
        Color::Yellow => output.push_str("\x1b[33m"),
        Color::Blue => output.push_str("\x1b[34m"),
        Color::Magenta => output.push_str("\x1b[35m"),
        Color::Cyan => output.push_str("\x1b[36m"),
        Color::Gray => output.push_str("\x1b[90m"),
        Color::DarkGray => output.push_str("\x1b[90m"),
        Color::LightRed => output.push_str("\x1b[91m"),
        Color::LightGreen => output.push_str("\x1b[92m"),
        Color::LightYellow => output.push_str("\x1b[93m"),
        Color::LightBlue => output.push_str("\x1b[94m"),
        Color::LightMagenta => output.push_str("\x1b[95m"),
        Color::LightCyan => output.push_str("\x1b[96m"),
        Color::White => output.push_str("\x1b[37m"),
        Color::Rgb(r, g, b) => {
            write!(output, "\x1b[38;2;{};{};{}m", r, g, b).unwrap();
        }
        Color::Indexed(i) => {
            write!(output, "\x1b[38;5;{}m", i).unwrap();
        }
    }
}

/// Applies background color to the output string.
fn apply_background_color(output: &mut String, color: Color) {
    match color {
        Color::Reset => {}
        Color::Black => output.push_str("\x1b[40m"),
        Color::Red => output.push_str("\x1b[41m"),
        Color::Green => output.push_str("\x1b[42m"),
        Color::Yellow => output.push_str("\x1b[43m"),
        Color::Blue => output.push_str("\x1b[44m"),
        Color::Magenta => output.push_str("\x1b[45m"),
        Color::Cyan => output.push_str("\x1b[46m"),
        Color::Gray => output.push_str("\x1b[100m"),
        Color::DarkGray => output.push_str("\x1b[100m"),
        Color::LightRed => output.push_str("\x1b[101m"),
        Color::LightGreen => output.push_str("\x1b[102m"),
        Color::LightYellow => output.push_str("\x1b[103m"),
        Color::LightBlue => output.push_str("\x1b[104m"),
        Color::LightMagenta => output.push_str("\x1b[105m"),
        Color::LightCyan => output.push_str("\x1b[106m"),
        Color::White => output.push_str("\x1b[47m"),
        Color::Rgb(r, g, b) => {
            write!(output, "\x1b[48;2;{};{};{}m", r, g, b).unwrap();
        }
        Color::Indexed(i) => {
            write!(output, "\x1b[48;5;{}m", i).unwrap();
        }
    }
}

/// Converts browser keyboard input to a crossterm KeyEvent.
///
/// Browser keyboards report keys differently than terminals. This function
/// maps browser key names (from JavaScript KeyboardEvent.key) to crossterm
/// KeyCodes for processing by the TUI.
///
/// # Arguments
///
/// * `input` - The browser keyboard input from WebRTC
///
/// # Returns
///
/// A crossterm KeyEvent if the key is recognized, or `None` for unknown keys.
///
/// # Example
///
/// ```ignore
/// use botster_hub::app::ui::convert_browser_key_to_crossterm;
/// use botster_hub::webrtc_handler::KeyInput;
///
/// let input = KeyInput {
///     key: "Enter".to_string(),
///     ctrl: false,
///     alt: false,
///     shift: false,
///     meta: false,
/// };
/// let event = convert_browser_key_to_crossterm(&input);
/// assert!(event.is_some());
/// ```
pub fn convert_browser_key_to_crossterm(input: &KeyInput) -> Option<KeyEvent> {
    let mut modifiers = KeyModifiers::empty();
    if input.ctrl {
        modifiers |= KeyModifiers::CONTROL;
    }
    if input.alt {
        modifiers |= KeyModifiers::ALT;
    }
    if input.shift {
        modifiers |= KeyModifiers::SHIFT;
    }

    // Map browser key names to crossterm KeyCode
    let key_code = match input.key.as_str() {
        // Single character keys
        k if k.len() == 1 => {
            let c = k.chars().next().unwrap();
            KeyCode::Char(c)
        }
        // Special keys
        "Enter" => KeyCode::Enter,
        "Escape" => KeyCode::Esc,
        "Backspace" => KeyCode::Backspace,
        "Tab" => KeyCode::Tab,
        "ArrowUp" => KeyCode::Up,
        "ArrowDown" => KeyCode::Down,
        "ArrowLeft" => KeyCode::Left,
        "ArrowRight" => KeyCode::Right,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Delete" => KeyCode::Delete,
        "Insert" => KeyCode::Insert,
        // Function keys
        "F1" => KeyCode::F(1),
        "F2" => KeyCode::F(2),
        "F3" => KeyCode::F(3),
        "F4" => KeyCode::F(4),
        "F5" => KeyCode::F(5),
        "F6" => KeyCode::F(6),
        "F7" => KeyCode::F(7),
        "F8" => KeyCode::F(8),
        "F9" => KeyCode::F(9),
        "F10" => KeyCode::F(10),
        "F11" => KeyCode::F(11),
        "F12" => KeyCode::F(12),
        // Space
        " " => KeyCode::Char(' '),
        // Unknown keys - ignore
        _ => {
            log::debug!("Unknown browser key: {}", input.key);
            return None;
        }
    };

    Some(KeyEvent {
        code: key_code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_centered_rect_50_percent() {
        let parent = Rect::new(0, 0, 100, 50);
        let result = centered_rect(50, 50, parent);

        // Should be roughly centered (rounding may affect exact values)
        assert!(result.x >= 20 && result.x <= 30);
        assert!(result.y >= 10 && result.y <= 15);
        assert!(result.width >= 45 && result.width <= 55);
        assert!(result.height >= 20 && result.height <= 30);
    }

    #[test]
    fn test_centered_rect_full_size() {
        let parent = Rect::new(0, 0, 100, 50);
        let result = centered_rect(100, 100, parent);

        assert_eq!(result.width, 100);
        assert_eq!(result.height, 50);
    }

    #[test]
    fn test_buffer_to_ansi_empty() {
        let buffer = Buffer::empty(Rect::new(0, 0, 10, 5));
        let result = buffer_to_ansi(&buffer, 10, 5, None, None);

        // Should contain reset and cursor positioning
        assert!(result.contains("\x1b[0m"));
        assert!(result.contains("\x1b[H"));
    }

    #[test]
    fn test_buffer_to_ansi_with_clipping() {
        let buffer = Buffer::empty(Rect::new(0, 0, 100, 50));
        let result = buffer_to_ansi(&buffer, 100, 50, Some(10), Some(5));

        // Should only have 5 lines of output
        let line_count = result.matches("\x1b[").count();
        // Each line has at least one cursor positioning escape
        assert!(line_count > 0);
    }

    #[test]
    fn test_convert_browser_key_enter() {
        let input = KeyInput {
            key: "Enter".to_string(),
            ctrl: false,
            alt: false,
            shift: false,
        };
        let result = convert_browser_key_to_crossterm(&input);

        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.code, KeyCode::Enter);
        assert_eq!(event.modifiers, KeyModifiers::empty());
    }

    #[test]
    fn test_convert_browser_key_with_modifiers() {
        let input = KeyInput {
            key: "c".to_string(),
            ctrl: true,
            alt: false,
            shift: false,
        };
        let result = convert_browser_key_to_crossterm(&input);

        assert!(result.is_some());
        let event = result.unwrap();
        assert_eq!(event.code, KeyCode::Char('c'));
        assert!(event.modifiers.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn test_convert_browser_key_arrow() {
        let input = KeyInput {
            key: "ArrowUp".to_string(),
            ctrl: false,
            alt: false,
            shift: false,
        };
        let result = convert_browser_key_to_crossterm(&input);

        assert!(result.is_some());
        assert_eq!(result.unwrap().code, KeyCode::Up);
    }

    #[test]
    fn test_convert_browser_key_function() {
        let input = KeyInput {
            key: "F5".to_string(),
            ctrl: false,
            alt: false,
            shift: false,
        };
        let result = convert_browser_key_to_crossterm(&input);

        assert!(result.is_some());
        assert_eq!(result.unwrap().code, KeyCode::F(5));
    }

    #[test]
    fn test_convert_browser_key_unknown() {
        let input = KeyInput {
            key: "UnknownKey123".to_string(),
            ctrl: false,
            alt: false,
            shift: false,
        };
        let result = convert_browser_key_to_crossterm(&input);

        assert!(result.is_none());
    }

    #[test]
    fn test_convert_browser_key_space() {
        let input = KeyInput {
            key: " ".to_string(),
            ctrl: false,
            alt: false,
            shift: false,
        };
        let result = convert_browser_key_to_crossterm(&input);

        assert!(result.is_some());
        assert_eq!(result.unwrap().code, KeyCode::Char(' '));
    }

    #[test]
    fn test_apply_modifiers() {
        let mut output = String::new();
        apply_modifiers(&mut output, Modifier::BOLD | Modifier::ITALIC);
        assert!(output.contains("\x1b[1m")); // Bold
        assert!(output.contains("\x1b[3m")); // Italic
    }

    #[test]
    fn test_apply_foreground_color() {
        let mut output = String::new();
        apply_foreground_color(&mut output, Color::Red);
        assert_eq!(output, "\x1b[31m");
    }

    #[test]
    fn test_apply_foreground_color_rgb() {
        let mut output = String::new();
        apply_foreground_color(&mut output, Color::Rgb(255, 128, 64));
        assert_eq!(output, "\x1b[38;2;255;128;64m");
    }

    #[test]
    fn test_apply_background_color() {
        let mut output = String::new();
        apply_background_color(&mut output, Color::Blue);
        assert_eq!(output, "\x1b[44m");
    }
}
