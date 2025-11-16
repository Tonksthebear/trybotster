use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use serde_json;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Manages git worktrees for agent sessions
pub struct WorktreeManager {
    base_dir: PathBuf,
}

impl WorktreeManager {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Reads .botster_copy file and returns patterns
    fn read_botster_copy_patterns(repo_path: &Path) -> Result<Vec<String>> {
        let botster_copy_path = repo_path.join(".botster_copy");

        if !botster_copy_path.exists() {
            return Ok(Vec::new());
        }

        let content =
            fs::read_to_string(&botster_copy_path).context("Failed to read .botster_copy")?;

        let patterns: Vec<String> = content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.to_string())
            .collect();

        Ok(patterns)
    }

    /// Reads .botster_init file and returns commands to run in the shell
    pub fn read_botster_init_commands(repo_path: &Path) -> Result<Vec<String>> {
        let botster_init_path = repo_path.join(".botster_init");

        if !botster_init_path.exists() {
            return Ok(Vec::new());
        }

        let content =
            fs::read_to_string(&botster_init_path).context("Failed to read .botster_init")?;

        let commands: Vec<String> = content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.to_string())
            .collect();

        log::info!("Read {} init command(s) from .botster_init", commands.len());
        Ok(commands)
    }

    /// Reads .botster_teardown file and returns commands to run before deletion
    pub fn read_botster_teardown_commands(repo_path: &Path) -> Result<Vec<String>> {
        let botster_teardown_path = repo_path.join(".botster_teardown");

        if !botster_teardown_path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&botster_teardown_path)
            .context("Failed to read .botster_teardown")?;

        let commands: Vec<String> = content
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.to_string())
            .collect();

        log::info!(
            "Read {} teardown command(s) from .botster_teardown",
            commands.len()
        );
        Ok(commands)
    }

    /// Copies files matching .botster_copy patterns from source to destination
    fn copy_botster_files(source_repo: &Path, dest_worktree: &Path) -> Result<()> {
        let patterns = Self::read_botster_copy_patterns(source_repo)?;

        if patterns.is_empty() {
            log::debug!("No .botster_copy patterns found, skipping file copy");
            return Ok(());
        }

        // Build globset from patterns
        let mut builder = GlobSetBuilder::new();
        for pattern in &patterns {
            let glob = Glob::new(pattern)
                .with_context(|| format!("Invalid pattern in .botster_copy: {}", pattern))?;
            builder.add(glob);
        }
        let globset = builder.build()?;

        // Walk the source repo and copy matching files
        Self::copy_matching_files(source_repo, dest_worktree, source_repo, &globset)?;

        log::info!("Copied {} pattern(s) from .botster_copy", patterns.len());
        Ok(())
    }

    /// Recursively copy files matching the globset
    fn copy_matching_files(
        source_root: &Path,
        dest_root: &Path,
        current_dir: &Path,
        globset: &globset::GlobSet,
    ) -> Result<()> {
        if !current_dir.is_dir() {
            return Ok(());
        }

        for entry in fs::read_dir(current_dir)? {
            let entry = entry?;
            let path = entry.path();

            // Skip .git directory
            if path.file_name().and_then(|n| n.to_str()) == Some(".git") {
                continue;
            }

            // Get relative path from source root
            let rel_path = path
                .strip_prefix(source_root)
                .context("Failed to get relative path")?;

            if path.is_dir() {
                // Recurse into directories
                Self::copy_matching_files(source_root, dest_root, &path, globset)?;
            } else if globset.is_match(&rel_path) {
                // Copy matching file
                let dest_path = dest_root.join(&rel_path);

                // Create parent directories if needed
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                fs::copy(&path, &dest_path).with_context(|| {
                    format!(
                        "Failed to copy {} to {}",
                        path.display(),
                        dest_path.display()
                    )
                })?;

                log::debug!("Copied: {}", rel_path.display());
            }
        }

        Ok(())
    }

    /// Detects the current git repository
    pub fn detect_current_repo() -> Result<(PathBuf, String)> {
        let current_dir = std::env::current_dir().context("Failed to get current directory")?;

        // Find the git repository root
        let repo = git2::Repository::discover(&current_dir).context("Not in a git repository")?;

        let repo_path = repo
            .path()
            .parent()
            .context("Failed to get repo path")?
            .to_path_buf();

        // Get the repo name from the remote URL or directory name
        let repo_name = if let Ok(remote) = repo.find_remote("origin") {
            if let Some(url) = remote.url() {
                // Extract owner/repo from URL like "https://github.com/owner/repo.git"
                url.trim_end_matches(".git")
                    .split('/')
                    .rev()
                    .take(2)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("/")
            } else {
                repo_path
                    .file_name()
                    .context("No repo name")?
                    .to_string_lossy()
                    .to_string()
            }
        } else {
            repo_path
                .file_name()
                .context("No repo name")?
                .to_string_lossy()
                .to_string()
        };

        Ok((repo_path, repo_name))
    }

    /// Creates a worktree from the current repository
    pub fn create_worktree_from_current(&self, issue_number: u32) -> Result<PathBuf> {
        let (repo_path, repo_name) = Self::detect_current_repo()?;

        let repo_safe = repo_name.replace('/', "-");
        let branch_name = format!("botster-issue-{}", issue_number);
        let worktree_path = self
            .base_dir
            .join(format!("{}-{}", repo_safe, issue_number));

        // Remove existing worktree if present
        self.cleanup_worktree(&repo_path, &worktree_path)?;

        let repo_obj = git2::Repository::open(&repo_path)?;

        // Check if branch exists
        let branch_exists = repo_obj
            .find_branch(&branch_name, git2::BranchType::Local)
            .is_ok();

        // Create worktree using git command
        let output = if branch_exists {
            log::info!("Using existing branch: {}", branch_name);
            std::process::Command::new("git")
                .args(&[
                    "worktree",
                    "add",
                    worktree_path.to_str().unwrap(),
                    &branch_name,
                ])
                .current_dir(&repo_path)
                .output()?
        } else {
            log::info!("Creating new branch: {}", branch_name);
            std::process::Command::new("git")
                .args(&[
                    "worktree",
                    "add",
                    "-b",
                    &branch_name,
                    worktree_path.to_str().unwrap(),
                ])
                .current_dir(&repo_path)
                .output()?
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to create worktree: {}", stderr);
        }

        // Mark as trusted for Claude
        let claude_dir = worktree_path.join(".claude");
        fs::create_dir_all(&claude_dir)?;

        // Create settings.local.json to pre-authorize the directory
        let settings = serde_json::json!({
            "allowedDirectories": [worktree_path.to_str().unwrap()],
            "permissionMode": "acceptEdits"
        });
        fs::write(
            claude_dir.join("settings.local.json"),
            serde_json::to_string_pretty(&settings)?,
        )?;

        // Copy files matching .botster_copy patterns
        Self::copy_botster_files(&repo_path, &worktree_path)?;

        Ok(worktree_path)
    }

    /// Creates or reuses a git worktree for the given repo and issue (clone from GitHub)
    pub fn create_worktree(&self, repo: &str, issue_number: u32) -> Result<PathBuf> {
        let repo_safe = repo.replace('/', "-");
        fs::create_dir_all(&self.base_dir)?;

        let clone_dir = self.base_dir.join(&repo_safe);

        // Clone if needed
        if !clone_dir.exists() {
            log::info!("Cloning {}...", repo);
            let url = format!("https://github.com/{}.git", repo);
            git2::Repository::clone(&url, &clone_dir).context("Failed to clone repository")?;
        }

        let branch_name = format!("botster-{}-{}", repo_safe, issue_number);
        let worktree_path = self
            .base_dir
            .join(format!("{}-{}", repo_safe, issue_number));

        let repo_obj = git2::Repository::open(&clone_dir)?;

        // Remove existing worktree if present - use git command as git2 API is unreliable
        self.cleanup_worktree(&clone_dir, &worktree_path)?;

        // Check if branch exists
        let branch_exists = repo_obj
            .find_branch(&branch_name, git2::BranchType::Local)
            .is_ok();

        // Create worktree using git command (git2 API doesn't handle existing branches properly)
        let output = if branch_exists {
            // Branch exists - checkout existing branch (no -b flag)
            log::info!("Using existing branch: {}", branch_name);
            std::process::Command::new("git")
                .args(&[
                    "worktree",
                    "add",
                    worktree_path.to_str().unwrap(),
                    &branch_name,
                ])
                .current_dir(&clone_dir)
                .output()?
        } else {
            // Branch doesn't exist - create new branch with -b flag
            log::info!("Creating new branch: {}", branch_name);
            std::process::Command::new("git")
                .args(&[
                    "worktree",
                    "add",
                    "-b",
                    &branch_name,
                    worktree_path.to_str().unwrap(),
                ])
                .current_dir(&clone_dir)
                .output()?
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to create worktree: {}", stderr);
        }

        // Mark as trusted for Claude
        let claude_dir = worktree_path.join(".claude");
        fs::create_dir_all(&claude_dir)?;

        // Create settings.local.json to pre-authorize the directory
        let settings = serde_json::json!({
            "allowedDirectories": [worktree_path.to_str().unwrap()],
            "permissionMode": "acceptEdits"
        });
        fs::write(
            claude_dir.join("settings.local.json"),
            serde_json::to_string_pretty(&settings)?,
        )?;

        Ok(worktree_path)
    }

    /// Cleans up a worktree using git command
    pub fn cleanup_worktree(&self, clone_dir: &PathBuf, worktree_path: &PathBuf) -> Result<()> {
        let worktree_name = worktree_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if !worktree_name.is_empty() {
            std::process::Command::new("git")
                .args(&["worktree", "remove", worktree_name, "--force"])
                .current_dir(clone_dir)
                .output()
                .ok();
        }
        Ok(())
    }

    /// Lists all existing worktrees for a repo
    pub fn list_worktrees(&self, repo: &str) -> Result<Vec<String>> {
        let repo_safe = repo.replace('/', "-");
        let clone_dir = self.base_dir.join(&repo_safe);

        if !clone_dir.exists() {
            return Ok(Vec::new());
        }

        let output = std::process::Command::new("git")
            .args(&["worktree", "list"])
            .current_dir(&clone_dir)
            .output()?;

        let list = String::from_utf8_lossy(&output.stdout);
        Ok(list.lines().map(|s| s.to_string()).collect())
    }

    /// Prunes all stale worktrees for a repo
    pub fn prune_stale_worktrees(&self, repo: &str) -> Result<()> {
        let repo_safe = repo.replace('/', "-");
        let clone_dir = self.base_dir.join(&repo_safe);

        if clone_dir.exists() {
            std::process::Command::new("git")
                .args(&["worktree", "prune"])
                .current_dir(&clone_dir)
                .output()?;
        }
        Ok(())
    }

    /// Deletes a worktree by issue number, running teardown scripts first
    pub fn delete_worktree_by_issue_number(&self, issue_number: u32) -> Result<()> {
        // Detect the current repo
        let (repo_path, repo_name) = Self::detect_current_repo()?;

        let repo_safe = repo_name.replace('/', "-");
        let branch_name = format!("botster-issue-{}", issue_number);
        let worktree_path = self
            .base_dir
            .join(format!("{}-{}", repo_safe, issue_number));

        if !worktree_path.exists() {
            anyhow::bail!(
                "Worktree for issue #{} does not exist at {}",
                issue_number,
                worktree_path.display()
            );
        }

        log::info!("Deleting worktree for issue #{}", issue_number);

        // Read and run teardown commands
        let teardown_commands = Self::read_botster_teardown_commands(&repo_path)?;

        if !teardown_commands.is_empty() {
            log::info!("Running {} teardown command(s)", teardown_commands.len());

            for cmd in teardown_commands {
                log::info!("Running teardown: {}", cmd);

                // Run the command in a shell with environment variables
                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .env("BOTSTER_REPO", &repo_name)
                    .env("BOTSTER_ISSUE_NUMBER", issue_number.to_string())
                    .env("BOTSTER_WORKTREE_PATH", worktree_path.to_str().unwrap())
                    .env(
                        "BOTSTER_HUB_BIN",
                        std::env::current_exe()
                            .ok()
                            .and_then(|p| p.to_str().map(|s| s.to_string()))
                            .unwrap_or_else(|| "botster-hub".to_string()),
                    )
                    .output()?;

                if !output.status.success() {
                    log::warn!(
                        "Teardown command failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                } else {
                    log::debug!(
                        "Teardown output: {}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                }
            }
        }

        // Remove the worktree using git
        log::info!("Removing worktree at {}", worktree_path.display());
        let output = std::process::Command::new("git")
            .args(&[
                "worktree",
                "remove",
                worktree_path.to_str().unwrap(),
                "--force",
            ])
            .current_dir(&repo_path)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to remove worktree: {}", stderr);
        }

        // Delete the branch
        log::info!("Deleting branch {}", branch_name);
        let output = std::process::Command::new("git")
            .args(&["branch", "-D", &branch_name])
            .current_dir(&repo_path)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!("Failed to delete branch {}: {}", branch_name, stderr);
        }

        log::info!("Successfully deleted worktree for issue #{}", issue_number);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_worktree_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let manager = WorktreeManager::new(temp_dir.path().to_path_buf());
        assert!(manager.base_dir.to_str().is_some());
    }

    #[test]
    fn test_cleanup_nonexistent_worktree() {
        let temp_dir = TempDir::new().unwrap();
        let manager = WorktreeManager::new(temp_dir.path().to_path_buf());
        let clone_dir = temp_dir.path().join("test-repo");
        let worktree = temp_dir.path().join("test-worktree");

        // Should not panic on non-existent worktree
        let result = manager.cleanup_worktree(&clone_dir, &worktree);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_worktrees_empty_repo() {
        let temp_dir = TempDir::new().unwrap();
        let manager = WorktreeManager::new(temp_dir.path().to_path_buf());

        // Non-existent repo should return empty list
        let result = manager.list_worktrees("nonexistent/repo");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 0);
    }
}
