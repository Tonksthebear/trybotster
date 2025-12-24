// Library modules
pub mod agent;
pub mod config;
pub mod git;
pub mod prompt;
pub mod terminal;
pub mod tunnel;
pub mod webrtc_handler;

// Re-export commonly used types
pub use agent::{Agent, AgentNotification, AgentStatus};
pub use config::Config;
pub use git::WorktreeManager;
pub use prompt::PromptManager;
pub use terminal::spawn_in_external_terminal;
pub use tunnel::{allocate_tunnel_port, TunnelManager};
pub use webrtc_handler::{
    AgentInfo, BrowserCommand, BrowserDimensions, BrowserMode, IceServerConfig, KeyInput,
    WebAgentInfo, WebRTCHandler, WebWorktreeInfo,
};
