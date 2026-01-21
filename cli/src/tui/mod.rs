//! TUI - Terminal User Interface adapter.
//!
//! This module provides the terminal rendering and input handling for
//! the botster-hub. It acts as an adapter between the Hub (business logic)
//! and the local terminal.
//!
//! # Architecture
//!
//! The TUI is an optional component - the Hub can run in headless mode
//! without it. When present, the TUI:
//!
//! - Renders the Hub's state to the terminal
//! - Converts keyboard/mouse input into [`HubAction`]s
//! - Handles terminal setup/teardown via RAII guards
//!
//! # Modules
//!
//! - [`guard`] - Terminal state RAII guard for cleanup
//! - [`qr`] - QR code generation for browser connection
//! - [`render`] - Main rendering function for Hub state
//! - [`input`] - Event to HubAction conversion
//! - [`view`] - View state types
//!
//! [`HubAction`]: crate::hub::HubAction

pub mod guard;
pub mod input;
pub mod layout;
pub mod qr;
pub mod render;
pub mod view;

pub use guard::TerminalGuard;
pub use input::{event_to_hub_action, InputContext};
pub use layout::terminal_widget_inner_area;
pub use qr::generate_qr_code_lines;
pub use render::render;
pub use view::{AgentDisplayInfo, ViewContext, ViewState};
