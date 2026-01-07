//! Scroll management for terminal views.
//!
//! Provides scroll operations for VT100 parser screens. These functions
//! operate on a locked parser, handling scrollback buffer navigation.
//!
//! # Design
//!
//! Functions in this module take `&Agent` or `&mut Agent` rather than
//! the parser directly. This keeps the public API simple while allowing
//! the implementation to access the correct PTY based on active view.

// Rust guideline compliant 2025-01

use super::Agent;

/// Check if we're in scrollback mode (scrolled up from live view).
#[must_use]
pub fn is_scrolled(agent: &Agent) -> bool {
    let parser = agent.get_active_parser();
    let p = parser.lock().expect("parser lock poisoned");
    p.screen().scrollback() > 0
}

/// Get current scroll offset from vt100.
#[must_use]
pub fn get_offset(agent: &Agent) -> usize {
    let parser = agent.get_active_parser();
    let p = parser.lock().expect("parser lock poisoned");
    p.screen().scrollback()
}

/// Scroll up by the specified number of lines.
pub fn up(agent: &Agent, lines: usize) {
    let parser = agent.get_active_parser();
    let mut p = parser.lock().expect("parser lock poisoned");
    let current = p.screen().scrollback();
    p.screen_mut().set_scrollback(current.saturating_add(lines));
}

/// Scroll down by the specified number of lines.
pub fn down(agent: &Agent, lines: usize) {
    let parser = agent.get_active_parser();
    let mut p = parser.lock().expect("parser lock poisoned");
    let current = p.screen().scrollback();
    p.screen_mut().set_scrollback(current.saturating_sub(lines));
}

/// Scroll to the bottom (return to live view).
pub fn to_bottom(agent: &Agent) {
    let parser = agent.get_active_parser();
    let mut p = parser.lock().expect("parser lock poisoned");
    p.screen_mut().set_scrollback(0);
}

/// Scroll to the top of the scrollback buffer.
pub fn to_top(agent: &Agent) {
    let parser = agent.get_active_parser();
    let mut p = parser.lock().expect("parser lock poisoned");
    p.screen_mut().set_scrollback(usize::MAX);
}

#[cfg(test)]
mod tests {
    // Integration tests require a full Agent, tested in agent/mod.rs
}
