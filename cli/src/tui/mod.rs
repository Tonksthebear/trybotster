//! TUI - Terminal User Interface.
//!
//! This module provides the terminal rendering, input handling, and event loop
//! for the botster-hub TUI. The TUI runs independently from the Hub, communicating
//! via channels.
//!
//! # Architecture
//!
//! ```text
//! TuiRunner (TUI thread)
//! ├── owns: mode, menu_selected, input_buffer, vt100_parser
//! ├── sends: JSON messages via request_tx (to Hub → Lua client.lua)
//! └── receives: TuiOutput via output_rx (PTY output and Lua events from Hub)
//! ```
//!
//! The TuiRunner owns all TUI state and runs its own event loop. It converts
//! keyboard/mouse input into either local TUI actions or JSON messages, and
//! communicates with Hub exclusively through the Lua client protocol.
//!
//! # Modules
//!
//! - [`actions`] - TUI-local action types (`TuiAction`, `InputResult`)
//! - [`events`] - TUI-specific event types (creation stages)
//! - [`guard`] - Terminal state RAII guard for cleanup
//! - [`input`] - Event to action/input conversion
//! - [`layout`] - Layout calculations
//! - [`qr`] - QR code generation for browser connection
//! - [`render`] - Main rendering function
//! - [`runner`] - TuiRunner struct, event loop, and `run_with_hub()`
//! - [`runner_agent`] - Agent navigation methods for TuiRunner
//! - [`runner_handlers`] - Action and event handlers for TuiRunner
//! - [`runner_input`] - Input submission handlers for TuiRunner
//! - [`menu`] - Context-aware menu system
//! - [`view`] - View state types

// Rust guideline compliant 2026-01

pub mod actions;
pub mod events;
pub mod guard;
pub mod input;
pub mod layout;
pub mod menu;
pub mod qr;
pub mod render;
pub mod runner;
mod runner_agent;
mod runner_handlers;
mod runner_input;
pub mod screen;
pub mod scroll;
pub mod view;

#[doc(inline)]
pub use actions::{InputResult, TuiAction};
#[doc(inline)]
pub use events::CreationStage;
#[doc(inline)]
pub use guard::TerminalGuard;
#[doc(inline)]
pub use input::{process_event, InputContext};
#[doc(inline)]
pub use layout::terminal_widget_inner_area;
#[doc(inline)]
pub use menu::{build_menu, MenuAction, MenuContext, MenuItem};
#[doc(inline)]
pub use qr::{build_kitty_escape_from_png, generate_qr_code_lines, generate_qr_png, ConnectionCodeData};
#[doc(inline)]
pub use render::{render, AgentRenderInfo, RenderContext, RenderResult};
#[doc(inline)]
pub use runner::{run_with_hub, TuiRunner};
#[doc(inline)]
pub use screen::ScreenInfo;
#[doc(inline)]
pub use view::{AgentDisplayInfo, ViewContext, ViewState};
