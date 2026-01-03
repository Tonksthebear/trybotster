// Library modules
pub mod agent;
pub mod agents;
pub mod app;
pub mod commands;
pub mod config;
pub mod constants;
pub mod git;
pub mod notifications;
pub mod process;
pub mod prompt;
pub mod render;
pub mod server;
pub mod terminal;
pub mod terminal_widget;
pub mod tunnel;
pub mod webrtc_handler;

// Re-export commonly used types
pub use agent::{Agent, AgentNotification, AgentStatus, PtyView};
pub use notifications::{NotificationSender, NotificationType};
pub use config::Config;
pub use git::WorktreeManager;
pub use prompt::PromptManager;
pub use render::render_agent_terminal;
pub use terminal::spawn_in_external_terminal;
pub use terminal_widget::TerminalWidget;
pub use tunnel::{allocate_tunnel_port, TunnelManager, TunnelStatus};
pub use webrtc_handler::{
    AgentInfo, BrowserCommand, BrowserDimensions, BrowserMode, IceServerConfig, KeyInput,
    WebAgentInfo, WebRTCHandler, WebWorktreeInfo,
};
pub use process::{get_parent_pid, kill_orphaned_processes};
