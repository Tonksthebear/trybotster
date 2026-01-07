//! Screen rendering utilities for terminal output.
//!
//! This module provides functions for converting VT100 screen state into
//! various output formats suitable for streaming to remote terminals or
//! computing change detection hashes.
//!
//! # Performance
//!
//! These functions are on the hot path for terminal streaming. Key optimizations:
//! - Pre-allocated string buffers with capacity hints
//! - Attribute change detection to minimize escape sequence output
//! - Hash-based change detection to skip unchanged frames

// Rust guideline compliant 2025-01

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write;
use std::hash::{Hash, Hasher};

/// Render a VT100 screen as ANSI escape sequences.
///
/// Produces output suitable for replaying on a remote terminal. Includes:
/// - Cursor positioning sequences
/// - Color and attribute sequences (only when changed)
/// - Screen clear and cursor show/hide sequences
///
/// # Arguments
///
/// * `screen` - Reference to the VT100 screen to render
///
/// # Returns
///
/// A string containing ANSI escape sequences that reproduce the screen.
///
/// # Performance
///
/// This function is optimized for minimal escape sequence output by tracking
/// attribute state and only emitting changes. String capacity is pre-allocated
/// based on screen dimensions.
#[must_use]
pub fn render_screen_as_ansi(screen: &vt100::Screen) -> String {
    let (rows, cols) = screen.size();

    // Estimate capacity: ~20 bytes per cell average for attributes + content
    let estimated_capacity = (rows as usize) * (cols as usize) * 20;
    let mut output = String::with_capacity(estimated_capacity);

    // Hide cursor during update to prevent flicker
    output.push_str("\x1b[?25l");

    // Reset attributes, clear screen and scrollback, move to home
    output.push_str("\x1b[0m\x1b[2J\x1b[3J\x1b[H");

    for row in 0..rows {
        let _ = write!(output, "\x1b[{};1H", row + 1);

        let mut last_fg = vt100::Color::Default;
        let mut last_bg = vt100::Color::Default;
        let mut last_bold = false;
        let mut last_italic = false;
        let mut last_underline = false;
        let mut last_inverse = false;

        let mut col = 0u16;
        while col < cols {
            let cell = screen.cell(row, col);
            if let Some(cell) = cell {
                let contents = cell.contents();

                if contents.is_empty() {
                    col += 1;
                    continue;
                }

                let _ = write!(output, "\x1b[{};{}H", row + 1, col + 1);

                let fg = cell.fgcolor();
                let bg = cell.bgcolor();
                let bold = cell.bold();
                let italic = cell.italic();
                let underline = cell.underline();
                let inverse = cell.inverse();

                let attrs_changed = fg != last_fg
                    || bg != last_bg
                    || bold != last_bold
                    || italic != last_italic
                    || underline != last_underline
                    || inverse != last_inverse;

                if attrs_changed {
                    output.push_str("\x1b[0m");
                    write_color_sequence(&mut output, fg, true);
                    write_color_sequence(&mut output, bg, false);

                    if bold {
                        output.push_str("\x1b[1m");
                    }
                    if italic {
                        output.push_str("\x1b[3m");
                    }
                    if underline {
                        output.push_str("\x1b[4m");
                    }
                    if inverse {
                        output.push_str("\x1b[7m");
                    }

                    last_fg = fg;
                    last_bg = bg;
                    last_bold = bold;
                    last_italic = italic;
                    last_underline = underline;
                    last_inverse = inverse;
                }

                output.push_str(contents);
            }
            col += 1;
        }
    }

    output.push_str("\x1b[0m");

    // Position cursor
    let cursor = screen.cursor_position();
    let _ = write!(output, "\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1);

    // Show cursor
    output.push_str("\x1b[?25h");

    output
}

/// Write a color escape sequence to the output.
///
/// # Arguments
///
/// * `output` - String buffer to write to
/// * `color` - The color to encode
/// * `is_foreground` - True for foreground (38), false for background (48)
fn write_color_sequence(output: &mut String, color: vt100::Color, is_foreground: bool) {
    let base = if is_foreground { 38 } else { 48 };

    match color {
        vt100::Color::Default => {}
        vt100::Color::Idx(i) => {
            let _ = write!(output, "\x1b[{base};5;{i}m");
        }
        vt100::Color::Rgb(r, g, b) => {
            let _ = write!(output, "\x1b[{base};2;{r};{g};{b}m");
        }
    }
}

/// Compute a hash of the screen content for change detection.
///
/// The hash includes:
/// - All screen contents (text and attributes)
/// - Cursor position
/// - Scrollback offset
///
/// # Arguments
///
/// * `screen` - Reference to the VT100 screen to hash
///
/// # Returns
///
/// A 64-bit hash suitable for equality comparison.
///
/// # Usage
///
/// Compare consecutive hashes to detect screen changes and avoid
/// sending unchanged frames to remote terminals.
#[must_use]
pub fn compute_screen_hash(screen: &vt100::Screen) -> u64 {
    let mut hasher = DefaultHasher::new();
    screen.contents().hash(&mut hasher);
    screen.cursor_position().hash(&mut hasher);
    screen.scrollback().hash(&mut hasher);
    hasher.finish()
}

/// Information about screen dimensions.
///
/// Used for debugging and reporting screen state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenInfo {
    /// Terminal height in rows.
    pub rows: u16,
    /// Terminal width in columns.
    pub cols: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_screen() -> vt100::Parser {
        vt100::Parser::new(24, 80, 1000)
    }

    #[test]
    fn test_render_empty_screen() {
        let parser = create_test_screen();
        let output = render_screen_as_ansi(parser.screen());

        // Should contain cursor hide/show sequences
        assert!(output.contains("\x1b[?25l")); // Hide cursor
        assert!(output.contains("\x1b[?25h")); // Show cursor
    }

    #[test]
    fn test_render_screen_with_content() {
        let mut parser = create_test_screen();
        parser.process(b"Hello, World!\r\n");

        let output = render_screen_as_ansi(parser.screen());

        // Strip ANSI escape sequences to verify content
        fn strip_ansi(s: &str) -> String {
            let mut result = String::new();
            let mut chars = s.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    // Skip until we hit a letter (end of escape sequence)
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next.is_ascii_alphabetic() || next == 'h' || next == 'l' {
                            break;
                        }
                    }
                } else {
                    result.push(c);
                }
            }
            result
        }

        let text = strip_ansi(&output);
        assert!(text.contains("Hello, World!"));
    }

    #[test]
    fn test_compute_screen_hash_changes_with_content() {
        let mut parser = create_test_screen();
        let hash1 = compute_screen_hash(parser.screen());

        parser.process(b"Some new content\r\n");
        let hash2 = compute_screen_hash(parser.screen());

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_screen_hash_stable_for_same_content() {
        let mut parser1 = create_test_screen();
        let mut parser2 = create_test_screen();

        parser1.process(b"Same content\r\n");
        parser2.process(b"Same content\r\n");

        let hash1 = compute_screen_hash(parser1.screen());
        let hash2 = compute_screen_hash(parser2.screen());

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_screen_info() {
        let info = ScreenInfo { rows: 24, cols: 80 };
        assert_eq!(info.rows, 24);
        assert_eq!(info.cols, 80);
    }
}
