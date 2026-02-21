//! TUI - Terminal User Interface.
//!
//! This module provides the terminal rendering, input handling, and event loop
//! for the botster TUI. The TUI runs independently from the Hub, communicating
//! via channels.
//!
//! # Architecture
//!
//! ```text
//! TuiRunner (TUI thread)
//! ├── owns: mode, widget_states (WidgetStateStore), panels (TerminalPanel)
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
//! - [`actions`] - TUI-local action types (`TuiAction`)
//! - [`guard`] - Terminal state RAII guard for cleanup
//! - [`hot_reload`] - Lua source loading, bootstrapping, and hot-reload
//! - [`layout`] - Layout calculations
//! - [`layout_lua`] - Lua state for layout, keybindings, and action dispatch
//! - [`lua_ops`] - Typed Lua operation enum (`LuaOp`)
//! - [`panel_pool`] - Terminal panel pool with focus state and subscriptions
//! - [`qr`] - QR code generation for browser connection
//! - [`raw_input`] - Raw stdin reader and byte-to-descriptor parser
//! - [`render`] - Main rendering function
//! - [`runner`] - TuiRunner struct, event loop, and `run_with_hub()`
//! - [`runner_handlers`] - Scroll and quit action dispatch
//! - [`terminal_modes`] - Terminal mode mirroring (DECCKM, bracketed paste, kitty)

// Rust guideline compliant 2026-02

pub mod actions;
pub mod guard;
pub mod hot_reload;
pub mod layout;
pub mod layout_lua;
pub mod lua_ops;
pub mod panel_pool;
pub mod qr;
pub mod raw_input;
pub mod render;
pub mod render_tree;
pub mod runner;
mod runner_handlers;
pub mod screen;
pub mod terminal_modes;
pub mod terminal_panel;
pub mod widget_state;

#[doc(inline)]
pub use actions::TuiAction;
#[doc(inline)]
pub use guard::TerminalGuard;
#[doc(inline)]
pub use raw_input::{InputEvent, RawInputReader};
#[doc(inline)]
pub use layout::terminal_widget_inner_area;
#[doc(inline)]
pub use qr::{generate_qr_code_lines, ConnectionCodeData};
#[doc(inline)]
pub use render::{render, RenderContext, RenderResult};
#[doc(inline)]
pub use runner::{run_with_hub, TuiRunner};
#[doc(inline)]
pub use screen::ScreenInfo;
