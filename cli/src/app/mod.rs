//! Application state and event handling for the botster-hub TUI.
//!
//! This module contains the core application types that define the TUI's state
//! and behavior. The main components are:
//!
//! - [`AppMode`] - The current mode/state of the TUI
//! - State management utilities (in submodules)
//!
//! # Application Flow
//!
//! The TUI operates in a mode-based state machine:
//!
//! ```text
//! Normal -> Menu -> NewAgentSelectWorktree -> NewAgentCreateWorktree
//!    ^        |                                       |
//!    |        v                                       v
//!    +--- CloseAgentConfirm                   NewAgentPrompt
//! ```

pub mod state;
pub mod ui;

pub use state::AppMode;

// Re-export commonly used types
pub use state::WorktreeSelection;
pub use ui::{buffer_to_ansi, centered_rect};
