//! Smooth scroll trait for mouse-wheel acceleration.
//!
//! Provides natural-feeling scroll acceleration: the first event in a batch
//! scrolls 1 line, the second scrolls 2, the third scrolls 3, etc. This
//! gives precise single-notch control while ramping up for fast scrolling.
//!
//! Implementors provide `scroll_up` and `scroll_down` for their specific
//! content type; the trait handles acceleration and tick reset.

// Rust guideline compliant 2026-03

use super::raw_input::ScrollDirection;

/// Smooth mouse-wheel scrolling with per-tick acceleration.
///
/// Widgets that can scroll implement this trait to get consistent
/// acceleration behavior. The runner calls [`mouse_scroll`] per raw
/// event and [`reset_scroll_accel`] at the end of each tick.
pub trait SmoothScroll {
    /// Scroll up (into history) by `lines` lines.
    fn scroll_up(&mut self, lines: usize);

    /// Scroll down (toward live/present) by `lines` lines.
    fn scroll_down(&mut self, lines: usize);

    /// Current acceleration counter.
    fn scroll_accel(&self) -> u32;

    /// Set the acceleration counter.
    fn set_scroll_accel(&mut self, val: u32);

    /// Handle a mouse scroll event with acceleration.
    ///
    /// Consecutive events within the same tick scroll progressively more
    /// lines (1, 2, 3, ...), giving natural acceleration for fast scrolling
    /// without sacrificing single-notch precision.
    fn mouse_scroll(&mut self, direction: ScrollDirection) {
        let accel = self.scroll_accel() + 1;
        self.set_scroll_accel(accel);
        let lines = accel as usize;
        match direction {
            ScrollDirection::Up => self.scroll_up(lines),
            ScrollDirection::Down => self.scroll_down(lines),
        }
    }

    /// Reset scroll acceleration between event batches.
    fn reset_scroll_accel(&mut self) {
        self.set_scroll_accel(0);
    }
}
