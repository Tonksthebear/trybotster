//! Git worktree management commands.
//!
//! Provides CLI utilities for listing and deleting git worktrees managed by
//! botster-hub. Worktrees are used to isolate agent work on separate branches.
//!
//! # Examples
//!
//! ```bash
//! # List all worktrees for the current repository
//! botster-hub list-worktrees
//!
//! # Delete a worktree by issue number
//! botster-hub delete-worktree 42
//! ```

use crate::{Config, WorktreeManager};
use anyhow::Result;
use std::process::Command;

/// Deletes a git worktree by issue number.
///
/// Removes the worktree directory and cleans up the git worktree reference.
/// Also runs any teardown scripts defined in the worktree.
///
/// # Errors
///
/// Returns an error if:
/// - Configuration cannot be loaded
/// - The worktree doesn't exist
/// - Git operations fail
///
/// # Examples
///
/// ```ignore
/// // Delete the worktree for issue #42
/// worktree::delete(42)?;
/// ```
pub fn delete(issue_number: u32) -> Result<()> {
    let config = Config::load()?;
    let git_manager = WorktreeManager::new(config.worktree_base);

    git_manager.delete_worktree_by_issue_number(issue_number)?;

    println!("Successfully deleted worktree for issue #{}", issue_number);
    Ok(())
}

/// Lists all git worktrees for the current repository.
///
/// Displays a formatted table of worktree paths and their associated branches.
/// Worktrees following the botster naming convention (issue-N) are highlighted.
///
/// # Output Format
///
/// ```text
/// Worktrees for repository: owner/repo
///
/// Path                                     Branch
/// ----------------------------------------------------------------------
/// /path/to/worktree-1                      issue-42
/// /path/to/worktree-2                      feature-branch
/// ```
///
/// # Errors
///
/// Returns an error if:
/// - Not in a git repository
/// - Git commands fail
///
/// # Examples
///
/// ```ignore
/// worktree::list()?;
/// ```
pub fn list() -> Result<()> {
    // Detect current repository
    let (repo_path, repo_name) = WorktreeManager::detect_current_repo()?;

    println!("Worktrees for repository: {}", repo_name);
    println!();

    // Run `git worktree list --porcelain` for machine-readable output
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repo_path)
        .output()?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to list worktrees: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let worktree_output = String::from_utf8_lossy(&output.stdout);
    let worktrees = parse_porcelain_output(&worktree_output);

    // Display worktrees in a formatted way
    if worktrees.is_empty() {
        println!("No worktrees found");
    } else {
        print_worktree_table(&worktrees);
    }

    Ok(())
}

/// Parsed worktree information.
#[derive(Debug, Clone)]
struct WorktreeInfo {
    path: String,
    branch: String,
}

/// Parses git worktree list --porcelain output.
///
/// Format:
/// ```text
/// worktree <path>
/// HEAD <sha>
/// branch <ref>
/// <blank line>
/// ```
fn parse_porcelain_output(output: &str) -> Vec<WorktreeInfo> {
    let mut worktrees = Vec::new();
    let mut current_path = String::new();
    let mut current_branch = String::new();

    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = path.to_string();
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            current_branch = branch.to_string();
        } else if line.is_empty() && !current_path.is_empty() {
            // End of worktree entry
            worktrees.push(WorktreeInfo {
                path: current_path.clone(),
                branch: current_branch.clone(),
            });
            current_path.clear();
            current_branch.clear();
        }
    }

    // Handle last entry if file doesn't end with blank line
    if !current_path.is_empty() {
        worktrees.push(WorktreeInfo {
            path: current_path,
            branch: current_branch,
        });
    }

    worktrees
}

/// Column width for path display.
const PATH_COLUMN_WIDTH: usize = 40;

/// Total table width including separator.
const TABLE_WIDTH: usize = 70;

/// Prints worktree information as a formatted table.
fn print_worktree_table(worktrees: &[WorktreeInfo]) {
    println!("{:<PATH_COLUMN_WIDTH$} {}", "Path", "Branch");
    println!("{}", "-".repeat(TABLE_WIDTH));

    for wt in worktrees {
        let display_branch = format_branch_name(&wt.branch);
        println!("{:<PATH_COLUMN_WIDTH$} {}", wt.path, display_branch);
    }
}

/// Formats branch name for display.
///
/// Returns "(detached)" for empty branch names, otherwise returns the branch as-is.
fn format_branch_name(branch: &str) -> String {
    if branch.is_empty() {
        "(detached)".to_string()
    } else {
        branch.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_porcelain_output_single_worktree() {
        let output = "worktree /path/to/main\nHEAD abc123\nbranch refs/heads/main\n\n";
        let worktrees = parse_porcelain_output(output);

        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].path, "/path/to/main");
        assert_eq!(worktrees[0].branch, "main");
    }

    #[test]
    fn test_parse_porcelain_output_multiple_worktrees() {
        let output = "\
worktree /path/to/main
HEAD abc123
branch refs/heads/main

worktree /path/to/feature
HEAD def456
branch refs/heads/feature-branch

";
        let worktrees = parse_porcelain_output(output);

        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].branch, "main");
        assert_eq!(worktrees[1].branch, "feature-branch");
    }

    #[test]
    fn test_parse_porcelain_output_detached_head() {
        let output = "worktree /path/to/detached\nHEAD abc123\n\n";
        let worktrees = parse_porcelain_output(output);

        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].branch, "");
    }

    #[test]
    fn test_parse_porcelain_output_no_trailing_newline() {
        let output = "worktree /path/to/main\nHEAD abc123\nbranch refs/heads/main";
        let worktrees = parse_porcelain_output(output);

        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].path, "/path/to/main");
    }

    #[test]
    fn test_format_branch_name_normal() {
        assert_eq!(format_branch_name("main"), "main");
        assert_eq!(format_branch_name("feature-123"), "feature-123");
    }

    #[test]
    fn test_format_branch_name_empty() {
        assert_eq!(format_branch_name(""), "(detached)");
    }
}
