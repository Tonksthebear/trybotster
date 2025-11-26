// Library modules
pub mod agent;
pub mod config;
pub mod git;
pub mod prompt;
pub mod terminal;

// Re-export commonly used types
pub use agent::{Agent, AgentStatus};
pub use config::Config;
pub use git::WorktreeManager;
pub use prompt::PromptManager;
pub use terminal::spawn_in_external_terminal;
