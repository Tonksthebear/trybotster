//! Application utilities for the botster TUI.
//!
//! This module contains UI utility functions used by the TUI rendering layer.

pub mod ui;

// Re-export commonly used types
pub use ui::{buffer_to_ansi, centered_rect};
