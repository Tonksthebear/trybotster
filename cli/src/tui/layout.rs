//! Layout calculations for the TUI.
//!
//! Provides functions to calculate terminal widget dimensions that match
//! the actual rendering layout. This ensures PTY dimensions match the
//! visible area.

// Rust guideline compliant 2025-01

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::{Block, Borders};

/// Calculate the inner area of the terminal widget for given terminal dimensions.
///
/// The layout is:
/// - 30% for agent list (left panel)
/// - 70% for terminal widget (right panel)
/// - Terminal widget has a 1-char border all around
///
/// This calculation must match the layout in `render.rs` to ensure PTY
/// dimensions match the visible rendering area. We use `Block::inner()` directly
/// to guarantee the calculation matches what ratatui does during rendering.
///
/// # Arguments
///
/// * `cols` - Total terminal width
/// * `rows` - Total terminal height
///
/// # Returns
///
/// A tuple of (rows, cols) representing the inner terminal widget area.
#[must_use]
pub fn terminal_widget_inner_area(cols: u16, rows: u16) -> (u16, u16) {
    let area = Rect::new(0, 0, cols, rows);

    // Split 30/70 horizontally (matches render.rs)
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
        .split(area);

    // Terminal widget is in chunks[1]
    let terminal_chunk = chunks[1];

    // Use Block::inner() to calculate exactly what render.rs does
    // This ensures we match ratatui's border calculation precisely
    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(terminal_chunk);

    // Ensure minimum viable dimensions
    let final_cols = inner.width.max(10);
    let final_rows = inner.height.max(5);

    (final_rows, final_cols)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_widget_inner_area_standard_terminal() {
        // Standard 80x24 terminal
        let (rows, cols) = terminal_widget_inner_area(80, 24);

        // 70% of 80 = 56, minus 2 for borders = 54
        assert_eq!(cols, 54);
        // 24 minus 2 for borders = 22
        assert_eq!(rows, 22);
    }

    #[test]
    fn test_terminal_widget_inner_area_large_terminal() {
        // Large 200x50 terminal
        let (rows, cols) = terminal_widget_inner_area(200, 50);

        // 70% of 200 = 140, minus 2 for borders = 138
        assert_eq!(cols, 138);
        // 50 minus 2 for borders = 48
        assert_eq!(rows, 48);
    }

    #[test]
    fn test_terminal_widget_inner_area_minimum_dimensions() {
        // Very small terminal
        let (rows, cols) = terminal_widget_inner_area(20, 10);

        // Should clamp to minimums
        assert!(cols >= 10);
        assert!(rows >= 5);
    }

    #[test]
    fn test_terminal_widget_inner_area_odd_dimensions() {
        // Odd dimensions (rounding test)
        let (rows, cols) = terminal_widget_inner_area(101, 25);

        // 70% of 101 ≈ 70, minus 2 = 68
        // (actual may vary based on ratatui's layout algorithm)
        assert!(cols > 50);
        assert!(rows == 23); // 25 - 2
    }

    #[test]
    fn test_debug_layout_chunks() {
        // Debug test to verify chunk calculations
        let area = Rect::new(0, 0, 191, 54);

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(area);

        eprintln!("Total area: {:?}", area);
        eprintln!("Left chunk (30%): {:?}", chunks[0]);
        eprintln!("Right chunk (70%): {:?}", chunks[1]);
        eprintln!(
            "Left + Right widths: {} + {} = {}",
            chunks[0].width,
            chunks[1].width,
            chunks[0].width + chunks[1].width
        );

        let block = Block::default().borders(Borders::ALL);
        let inner = block.inner(chunks[1]);
        eprintln!("Inner area after borders: {:?}", inner);
        eprintln!("PTY should be: {}cols x {}rows", inner.width, inner.height);

        // The chunks should fill the entire width
        assert_eq!(
            chunks[0].width + chunks[1].width,
            area.width,
            "Chunks should fill total width"
        );
    }
}
