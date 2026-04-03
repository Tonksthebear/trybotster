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
pub mod mcp_gateway;
pub mod relay;
pub mod session;
pub mod socket;
pub mod tui;
pub mod ws;

pub mod compat;
pub mod config;
pub mod constants;
pub mod crypto;
pub mod device;
pub mod env;
pub mod file_watcher;
#[allow(missing_docs, missing_debug_implementations)]
pub mod ghostty_vt;
pub mod git;
pub mod keyring;
pub mod notifications;
pub mod process;
pub mod server;
pub mod spawn_targets;
pub mod terminal;
pub mod terminal_widget;
pub mod terminfo;

// Re-export commonly used types
pub use agent::Agent;
pub use config::Config;
pub use git::WorktreeManager;
pub use relay::{AgentInfo, TerminalMessage, WorktreeInfo};
pub use spawn_targets::{SpawnTarget, SpawnTargetInspection, SpawnTargetRegistry};
pub use terminal_widget::TerminalWidget;

// Re-export Hub
pub use hub::Hub;
