//! CLI subcommand implementations for botster-hub.
//!
//! This module contains the business logic for all CLI subcommands that don't
//! involve the interactive TUI. Commands are organized into submodules by domain:
//!
//! - [`json`] - JSON file manipulation (get, set, delete)
//! - [`prompt`] - Agent prompt retrieval
//! - [`update`] - Self-update functionality
//! - [`worktree`] - Git worktree management (list, delete)
//!
//! # Usage
//!
//! Commands are invoked from the main CLI dispatcher:
//!
//! ```ignore
//! use botster_hub::commands;
//!
//! commands::json::get(&file_path, &key_path)?;
//! commands::worktree::list()?;
//! commands::update::check()?;
//! ```

pub mod json;
pub mod prompt;
pub mod update;
pub mod worktree;

// Re-export commonly used functions for convenience
#[doc(inline)]
pub use json::{delete as json_delete, get as json_get, set as json_set};
#[doc(inline)]
pub use prompt::get as get_prompt;
#[doc(inline)]
pub use update::{check as update_check, install as update_install, VERSION};
#[doc(inline)]
pub use worktree::{delete as delete_worktree, list as list_worktrees};
