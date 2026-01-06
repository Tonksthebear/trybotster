// Library modules
pub mod agent;
pub mod agents;
pub mod app;
pub mod commands;
pub mod compat;
pub mod config;
pub mod constants;
pub mod device;
pub mod git;
pub mod notifications;
pub mod process;
pub mod prompt;
pub mod render;
pub mod server;
pub mod terminal;
pub mod terminal_relay;
pub mod terminal_widget;
pub mod tunnel;

// Re-export commonly used types
pub use agent::{Agent, AgentNotification, AgentStatus, PtyView, ScreenInfo};
pub use app::{dispatch_key_event, parse_terminal_input, InputAction, KeyInput};
pub use compat::{BrowserDimensions, BrowserMode, VpnStatus, WebAgentInfo, WebWorktreeInfo};
pub use config::Config;
pub use device::Device;
pub use git::WorktreeManager;
pub use notifications::{NotificationSender, NotificationType};
pub use process::{get_parent_pid, kill_orphaned_processes};
pub use prompt::PromptManager;
pub use render::render_agent_terminal;
pub use terminal::spawn_in_external_terminal;
pub use terminal_relay::{
    AgentInfo, BrowserEvent, BrowserResize, TerminalMessage, TerminalOutputSender, TerminalRelay,
    WorktreeInfo,
};
pub use terminal_widget::TerminalWidget;
pub use tunnel::{allocate_tunnel_port, TunnelManager, TunnelStatus};
