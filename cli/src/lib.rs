//! Botster Hub - Agent orchestration daemon.
//!
//! This crate provides the core functionality for the botster CLI,
//! managing agent lifecycles, TUI rendering, and remote client communication.
//!
//! # Architecture
//!
//! The crate follows a centralized state store pattern:
//!
//! - **Hub** - Central orchestrator, owns state, runs event loop
//! - **Agent** - Domain entity representing a working session (user-defined processes)
//! - **TUI** - Terminal view adapter (optional - headless mode works without it)
//! - **Server** - Rails API adapter
//! - **Relay** - Browser WebSocket adapter
//!
//! # Modules
//!
//! - [`agent`] - Agent and PTY session management
//! - [`app`] - TUI state types and input handling
//! - [`server`] - Rails API client
//! - [`config`] - Configuration loading/saving

// Library modules
pub mod agent;
pub mod app;
pub mod auth;
pub mod channel;
pub mod client;
pub mod commands;
pub mod hub;
pub mod lua;
pub mod mcp_serve;
pub mod relay;
pub mod socket;
pub mod tui;
pub mod ws;

pub mod compat;
pub mod config;
pub mod crypto;
pub mod notifications;
pub mod file_watcher;
pub mod constants;
pub mod device;
pub mod env;
pub mod git;
pub mod keyring;
pub mod process;
pub mod server;
pub mod terminal_widget;

// Re-export commonly used types
pub use agent::{Agent, PtyView};
pub use config::{Config, HubRegistry};
pub use git::WorktreeManager;
pub use relay::{AgentInfo, TerminalMessage, WorktreeInfo};
pub use terminal_widget::TerminalWidget;

// Re-export Hub
pub use hub::Hub;

