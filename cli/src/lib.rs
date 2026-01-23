//! Botster Hub - Agent orchestration daemon.
//!
//! This crate provides the core functionality for the botster-hub CLI,
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
//! - [`tunnel`] - HTTP tunnel forwarding

// Library modules
pub mod agent;
pub mod agents;
pub mod app;
pub mod auth;
pub mod channel;
pub mod client;
pub mod commands;
pub mod hub;
pub mod relay;
pub mod tui;

// Re-export auth types for tests
pub use auth::{DeviceCodeResponse, ErrorResponse, TokenResponse};
pub mod compat;
pub mod config;
pub mod constants;
pub mod device;
pub mod env;
pub mod git;
pub mod keyring;
pub mod notifications;
pub mod process;
pub mod prompt;
pub mod render;
pub mod server;
pub mod terminal_widget;
pub mod tunnel;

// Re-export commonly used types
pub use agent::{Agent, AgentNotification, AgentStatus, PtyView, ScreenInfo};
pub use app::{dispatch_key_event, parse_terminal_input, InputAction, KeyEventContext, KeyInput};
pub use compat::{BrowserDimensions, BrowserMode, VpnStatus};
pub use config::Config;
pub use device::Device;
pub use git::WorktreeManager;
pub use notifications::{NotificationSender, NotificationType};
pub use process::{get_parent_pid, kill_orphaned_processes};
pub use prompt::PromptManager;
pub use relay::{AgentInfo, BrowserEvent, BrowserResize, HubSender, TerminalMessage, WorktreeInfo};
pub use terminal_widget::TerminalWidget;
pub use tunnel::{allocate_tunnel_port, TunnelManager, TunnelStatus};

// Re-export Channel types
pub use channel::{
    ActionCableChannel, Channel, ChannelConfig, ChannelError, ConnectionState, IncomingMessage,
    PeerId, SharedConnectionState,
};

// Re-export Hub types
pub use agents::AgentSpawnConfig;
pub use hub::{Hub, HubAction, HubState};

// Re-export Client types
pub use client::{
    BrowserClient, Client, ClientId, ClientRegistry, CreateAgentRequest, DeleteAgentRequest,
    Response, TuiClient,
};

// Re-export PTY event types (for pub/sub integration)
pub use agent::pty::{ConnectedClient, PtyEvent};

// Re-export Hub event/command types (for event-driven architecture)
pub use hub::{HubCommand, HubCommandSender, HubEvent};
