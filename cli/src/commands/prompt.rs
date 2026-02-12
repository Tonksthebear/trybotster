//! Agent prompt retrieval command.
//!
//! Provides CLI utility for retrieving the system prompt that would be used
//! for an agent in a given worktree. This is useful for debugging and
//! understanding what instructions an agent will receive.
//!
//! # Examples
//!
//! ```bash
//! # Get the prompt for a specific worktree
//! botster get-prompt /path/to/worktree
//! ```

use crate::PromptManager;
use anyhow::Result;
use std::path::PathBuf;

/// Retrieves and prints the system prompt for a worktree.
///
/// Loads the prompt configuration from the worktree and prints it to stdout.
/// The prompt includes system instructions, repository context, and any
/// custom configuration from `.botster_prompt`.
///
/// # Output
///
/// The prompt is printed to stdout without a trailing newline, making it
/// suitable for capture in shell scripts.
///
/// # Errors
///
/// Returns an error if:
/// - The worktree path doesn't exist
/// - The prompt configuration cannot be loaded
///
/// # Examples
///
/// ```ignore
/// // Get the prompt for a worktree
/// prompt::get("/path/to/worktree")?;
/// ```
pub fn get(worktree_path: &str) -> Result<()> {
    let path = PathBuf::from(worktree_path);
    let prompt = PromptManager::get_prompt(&path)?;

    // Print to stdout without trailing newline for shell capture
    print!("{}", prompt);

    Ok(())
}

#[cfg(test)]
mod tests {
    // Integration tests would require a real worktree setup.
    // Unit tests for PromptManager are in the prompt module.
}
