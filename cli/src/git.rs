//! Git worktree management.
//!
//! Provides functionality for creating, managing, and deleting git worktrees
//! for agent sessions. Each agent runs in an isolated worktree to prevent
//! conflicts between concurrent tasks.

use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Returns the path for debug logging.
/// In test mode, writes to project tmp/ to avoid leaking outside the project.
fn debug_log_path() -> PathBuf {
    if crate::env::is_any_test() {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.join("tmp/botster_debug.log"))
            .unwrap_or_else(|| PathBuf::from("/tmp/botster_debug.log"))
    } else {
        PathBuf::from("/tmp/botster_debug.log")
    }
}

/// Manages git worktrees for agent sessions.
#[derive(Debug)]
pub struct WorktreeManager {
    /// Base directory for worktree storage.
    base_dir: PathBuf,
}

impl WorktreeManager {
    /// Creates a new worktree manager with the specified base directory.
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Read workspace teardown commands from `.botster/shared/workspace_teardown`.
    ///
    /// Returns non-empty, non-comment lines from the teardown file.
    /// Returns an empty vector if the file does not exist.
    pub fn read_teardown_commands(repo_path: &Path) -> Result<Vec<String>> {
        let teardown_path = repo_path.join(".botster/shared/workspace_teardown");

        if !teardown_path.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&teardown_path)
            .context("Failed to read .botster/shared/workspace_teardown")?;

        let commands: Vec<String> = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(std::string::ToString::to_string)
            .collect();

        log::info!(
            "Read {} teardown command(s) from workspace_teardown",
            commands.len()
        );
        Ok(commands)
    }

    /// Copy files from `source_repo` to `dest` matching glob patterns in `patterns_file`.
    ///
    /// Reads one glob pattern per line from `patterns_file` (ignoring blanks and
    /// `#`-comments), then recursively walks `source_repo` and copies every
    /// matching file into `dest`, preserving relative paths.
    pub fn copy_from_patterns(
        source_repo: &Path,
        dest: &Path,
        patterns_file: &Path,
    ) -> Result<()> {
        let content =
            fs::read_to_string(patterns_file).context("Failed to read patterns file")?;

        let patterns: Vec<String> = content
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(std::string::ToString::to_string)
            .collect();

        if patterns.is_empty() {
            log::debug!("No patterns in {}, skipping file copy", patterns_file.display());
            return Ok(());
        }

        log::info!(
            "Copying {} pattern(s) from {} into {}",
            patterns.len(),
            patterns_file.display(),
            dest.display(),
        );

        let mut builder = GlobSetBuilder::new();
        for pattern in &patterns {
            match Glob::new(pattern) {
                Ok(glob) => {
                    builder.add(glob);
                }
                Err(e) => {
                    log::warn!("Invalid glob pattern '{}': {}", pattern, e);
                    continue;
                }
            }
        }
        let globset = builder.build()?;

        Self::copy_matching_files(source_repo, dest, source_repo, &globset)?;

        log::info!("Copied {} pattern(s) into {}", patterns.len(), dest.display());
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

        let read_result = fs::read_dir(current_dir);
        if let Err(e) = &read_result {
            log::warn!("Failed to read directory {}: {}", current_dir.display(), e);
            return Ok(()); // Continue despite errors
        }

        for entry in read_result.expect("checked is_ok() above") {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    log::warn!("Failed to read entry: {}", err);
                    continue;
                }
            };
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
            } else {
                log::debug!(
                    "Checking file: {} (rel_path: {})",
                    path.display(),
                    rel_path.display()
                );
                if globset.is_match(rel_path) {
                    // Copy matching file
                    let dest_path = dest_root.join(rel_path);

                    // Create parent directories if needed
                    if let Some(parent) = dest_path.parent() {
                        if let Err(e) = fs::create_dir_all(parent) {
                            log::warn!(
                                "Failed to create parent directory for {}: {}",
                                dest_path.display(),
                                e
                            );
                            continue;
                        }
                    }

                    // Copy file, but continue on error
                    match fs::copy(&path, &dest_path) {
                        Ok(_) => {
                            log::info!("Copied: {} to {}", rel_path.display(), dest_path.display());

                            // Also append to debug file
                            let debug_msg = format!(
                                "COPIED: {} -> {}\n",
                                rel_path.display(),
                                dest_path.display()
                            );
                            use std::io::Write;
                            if let Ok(mut file) = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(debug_log_path())
                            {
                                let _ = file.write_all(debug_msg.as_bytes());
                            }
                        }
                        Err(e) => {
                            log::warn!(
                                "Failed to copy {} to {}: {} - continuing with remaining files",
                                path.display(),
                                dest_path.display(),
                                e
                            );
                        }
                    }
                } else {
                    log::debug!("Skipping (no match): {}", rel_path.display());
                }
            }
        }

        Ok(())
    }

    /// Detects the current git repository
    ///
    /// Repo name is determined from (in order):
    /// 1. BOTSTER_REPO env var (for tests and explicit override)
    /// 2. Origin remote URL
    /// 3. Directory name
    pub fn detect_current_repo() -> Result<(PathBuf, String)> {
        let current_dir = std::env::current_dir().context("Failed to get current directory")?;

        // Find the git repository root via `git rev-parse --show-toplevel`
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&current_dir)
            .output()
            .context("Failed to run git rev-parse")?;

        if !output.status.success() {
            anyhow::bail!("Not in a git repository");
        }

        let repo_path = PathBuf::from(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        );

        // Get the repo name: env var > origin remote > directory name
        let repo_name = if let Ok(env_repo) = std::env::var("BOTSTER_REPO") {
            // Explicit override (used in tests)
            env_repo
        } else if let Ok(url) = git_remote_url(&repo_path) {
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
        };

        Ok((repo_path, repo_name))
    }

    /// Creates a worktree from the current repository with a custom branch name
    pub fn create_worktree_with_branch(&self, branch_name: &str) -> Result<PathBuf> {
        let (repo_path, repo_name) = Self::detect_current_repo()?;

        let repo_safe = repo_name.replace('/', "-");
        let sanitized_branch = branch_name.replace('/', "-");
        let worktree_path = self
            .base_dir
            .join(format!("{}-{}", repo_safe, sanitized_branch));

        // Remove existing worktree if present
        self.cleanup_worktree(&repo_path, &worktree_path)?;

        let branch_exists = git_branch_exists(&repo_path, branch_name);

        // Create worktree using git command
        let output = if branch_exists {
            log::info!("Using existing branch: {}", branch_name);
            std::process::Command::new("git")
                .args([
                    "worktree",
                    "add",
                    worktree_path.to_str().expect("path is valid UTF-8"),
                    branch_name,
                ])
                .current_dir(&repo_path)
                .output()?
        } else {
            log::info!("Creating new branch: {}", branch_name);
            std::process::Command::new("git")
                .args([
                    "worktree",
                    "add",
                    "-b",
                    branch_name,
                    worktree_path.to_str().expect("path is valid UTF-8"),
                ])
                .current_dir(&repo_path)
                .output()?
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to create worktree: {}", stderr);
        }

        // File copying is now Lua-driven via worktree.copy_from_patterns()
        // using the resolved workspace_include from .botster/ config.

        Ok(worktree_path)
    }

    /// Creates a worktree from the current repository
    pub fn create_worktree_from_current(&self, issue_number: u32) -> Result<PathBuf> {
        let branch_name = format!("botster-issue-{}", issue_number);
        self.create_worktree_with_branch(&branch_name)
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
            let output = std::process::Command::new("git")
                .args(["clone", &url, clone_dir.to_str().expect("path is valid UTF-8")])
                .output()
                .context("Failed to run git clone")?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("Failed to clone repository: {}", stderr);
            }
        }

        let branch_name = format!("botster-{}-{}", repo_safe, issue_number);
        let worktree_path = self
            .base_dir
            .join(format!("{}-{}", repo_safe, issue_number));

        // Remove existing worktree if present
        self.cleanup_worktree(&clone_dir, &worktree_path)?;

        let branch_exists = git_branch_exists(&clone_dir, &branch_name);

        // Create worktree using git command (git2 API doesn't handle existing branches properly)
        let output = if branch_exists {
            // Branch exists - checkout existing branch (no -b flag)
            log::info!("Using existing branch: {}", branch_name);
            std::process::Command::new("git")
                .args([
                    "worktree",
                    "add",
                    worktree_path.to_str().expect("path is valid UTF-8"),
                    &branch_name,
                ])
                .current_dir(&clone_dir)
                .output()?
        } else {
            // Branch doesn't exist - create new branch with -b flag
            log::info!("Creating new branch: {}", branch_name);
            std::process::Command::new("git")
                .args([
                    "worktree",
                    "add",
                    "-b",
                    &branch_name,
                    worktree_path.to_str().expect("path is valid UTF-8"),
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
            "allowedDirectories": [worktree_path.to_str().expect("path is valid UTF-8")],
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
        if worktree_path.exists() {
            log::info!("Removing existing worktree at {}", worktree_path.display());

            // Try to remove with git worktree remove
            let remove_result = std::process::Command::new("git")
                .args([
                    "worktree",
                    "remove",
                    worktree_path.to_str().expect("path is valid UTF-8"),
                    "--force",
                ])
                .current_dir(clone_dir)
                .output();

            // If git command fails, try to prune and remove directory manually
            if remove_result.as_ref().is_err()
                || remove_result.as_ref().is_ok_and(|r| !r.status.success())
            {
                log::warn!("Git worktree remove failed, trying prune...");
                let _ = std::process::Command::new("git")
                    .args(["worktree", "prune"])
                    .current_dir(clone_dir)
                    .output();

                // Manually remove the directory if it still exists
                if worktree_path.exists() {
                    let _ = std::fs::remove_dir_all(worktree_path);
                }
            }
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
            .args(["worktree", "list"])
            .current_dir(&clone_dir)
            .output()?;

        let list = String::from_utf8_lossy(&output.stdout);
        Ok(list.lines().map(std::string::ToString::to_string).collect())
    }

    /// Finds an existing worktree for a given issue number
    /// Returns the worktree path and branch name if found
    pub fn find_existing_worktree_for_issue(
        &self,
        issue_number: u32,
    ) -> Result<Option<(PathBuf, String)>> {
        let (repo_path, repo_name) = Self::detect_current_repo()?;
        let repo_safe = repo_name.replace('/', "-");
        let branch_name = format!("botster-issue-{}", issue_number);
        let worktree_path = self.base_dir.join(format!("{}-{}", repo_safe, branch_name));

        // Check if the worktree directory exists
        if !worktree_path.exists() {
            log::debug!("No worktree found at {}", worktree_path.display());
            return Ok(None);
        }

        // Verify it's actually a git worktree
        let git_file = worktree_path.join(".git");
        if !git_file.exists() {
            log::warn!(
                "Directory exists but is not a git worktree: {}",
                worktree_path.display()
            );
            return Ok(None);
        }

        // Verify the worktree is valid by checking if git recognizes it
        let output = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repo_path)
            .output()?;

        if !output.status.success() {
            log::warn!("Failed to list worktrees");
            return Ok(None);
        }

        let worktree_output = String::from_utf8_lossy(&output.stdout);
        let worktree_path_str = worktree_path.to_str().unwrap_or("");

        // Check if our worktree path is in the list
        for line in worktree_output.lines() {
            if line.starts_with("worktree ") {
                let path = line.strip_prefix("worktree ").unwrap_or("");
                if path == worktree_path_str {
                    log::info!(
                        "Found existing worktree for issue #{} at {}",
                        issue_number,
                        worktree_path.display()
                    );
                    return Ok(Some((worktree_path, branch_name)));
                }
            }
        }

        log::debug!("Worktree directory exists but not registered with git");
        Ok(None)
    }

    /// Prunes all stale worktrees for a repo
    pub fn prune_stale_worktrees(&self, repo: &str) -> Result<()> {
        let repo_safe = repo.replace('/', "-");
        let clone_dir = self.base_dir.join(&repo_safe);

        if clone_dir.exists() {
            std::process::Command::new("git")
                .args(["worktree", "prune"])
                .current_dir(&clone_dir)
                .output()?;
        }
        Ok(())
    }

    /// Deletes a worktree by path, running teardown scripts first.
    ///
    /// # Note
    ///
    /// This function has multiple defense-in-depth checks to prevent accidental
    /// deletion of the main repository or other important directories.
    pub fn delete_worktree_by_path(
        &self,
        worktree_path: &std::path::Path,
        branch_name: &str,
    ) -> Result<()> {
        // DEFENSE-IN-DEPTH CHECK 1: Verify path is within managed base directory
        let canonical_worktree = worktree_path
            .canonicalize()
            .context("Failed to canonicalize worktree path")?;
        let canonical_base = self
            .base_dir
            .canonicalize()
            .unwrap_or_else(|_| self.base_dir.clone());

        if !canonical_worktree.starts_with(&canonical_base) {
            log::error!(
                "SECURITY: Refusing to delete path outside managed directory. Path: {}, Base: {}",
                canonical_worktree.display(),
                canonical_base.display()
            );
            anyhow::bail!(
                "Worktree path {} is outside managed base directory {}",
                worktree_path.display(),
                self.base_dir.display()
            );
        }

        // DEFENSE-IN-DEPTH CHECK 2: Verify branch name follows botster convention
        if !branch_name.starts_with("botster-") {
            log::warn!(
                "Branch name '{}' doesn't follow botster convention (should start with 'botster-')",
                branch_name
            );
            // Don't bail - just warn, as this might be intentional
        }

        // DEFENSE-IN-DEPTH CHECK 3: Check for Claude settings marker file
        let marker_file = worktree_path.join(".claude/settings.local.json");
        if !marker_file.exists() {
            log::warn!(
                "Missing botster marker file at {} - this may not be a managed worktree",
                marker_file.display()
            );
            // Don't bail - just warn
        }

        log::debug!("worktree_path = {}", worktree_path.display());

        // DEFENSE-IN-DEPTH CHECK 4: Worktrees have a .git *file* (not directory)
        // pointing to the main repo. A main repo has a .git *directory*.
        let is_worktree = git_is_worktree(worktree_path);
        log::debug!("is_worktree() = {}", is_worktree);

        if !is_worktree {
            log::error!(
                "CRITICAL: Refusing to delete main repository at {}. This is not a worktree!",
                worktree_path.display()
            );
            anyhow::bail!(
                "Cannot delete main repository at {}. Only worktrees can be deleted.",
                worktree_path.display()
            );
        }

        // Find the main repository via `git rev-parse --git-common-dir`
        let repo_path = git_common_dir(worktree_path)
            .context("Failed to find main repository from worktree")?;
        log::info!(
            "DEBUG: Calculated repo_path from worktree = {}",
            repo_path.display()
        );

        // Get repo name from the remote URL or directory name
        let repo_name = if let Ok(url) = git_remote_url(worktree_path) {
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
        };

        if !worktree_path.exists() {
            log::warn!(
                "Worktree at {} does not exist, skipping deletion",
                worktree_path.display()
            );
            return Ok(());
        }

        log::info!("Deleting worktree at {}", worktree_path.display());

        // Read and run teardown commands
        let teardown_commands = Self::read_teardown_commands(&repo_path)?;

        if !teardown_commands.is_empty() {
            log::info!("Running {} teardown command(s)", teardown_commands.len());

            for cmd in teardown_commands {
                log::info!("Running teardown: {}", cmd);

                // Parse issue number from branch name if it's an issue-based branch
                let issue_number = if branch_name.starts_with("botster-issue-") {
                    branch_name
                        .strip_prefix("botster-issue-")
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0)
                } else {
                    0
                };

                // Run the command in a shell with environment variables
                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .env("BOTSTER_REPO", &repo_name)
                    .env("BOTSTER_ISSUE_NUMBER", issue_number.to_string())
                    .env("BOTSTER_BRANCH_NAME", branch_name)
                    .env(
                        "BOTSTER_WORKTREE_PATH",
                        worktree_path.to_str().expect("path is valid UTF-8"),
                    )
                    .env(
                        "BOTSTER_BIN",
                        std::env::current_exe()
                            .ok()
                            .and_then(|p| p.to_str().map(std::string::ToString::to_string))
                            .unwrap_or_else(|| "botster".to_string()),
                    )
                    .output()?;

                if output.status.success() {
                    log::debug!(
                        "Teardown output: {}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                } else {
                    log::warn!(
                        "Teardown command failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }

        // Remove the worktree using git
        log::info!("DEBUG: About to run git worktree remove");
        log::info!(
            "DEBUG: worktree_path argument = {}",
            worktree_path.display()
        );
        log::info!("DEBUG: current_dir (repo_path) = {}", repo_path.display());
        log::info!(
            "DEBUG: Command: git worktree remove {} --force",
            worktree_path.display()
        );

        let output = std::process::Command::new("git")
            .args([
                "worktree",
                "remove",
                worktree_path.to_str().expect("path is valid UTF-8"),
                "--force",
            ])
            .current_dir(&repo_path)
            .output()?;

        log::info!("DEBUG: git command exit status = {}", output.status);
        log::info!(
            "DEBUG: git stdout = {}",
            String::from_utf8_lossy(&output.stdout)
        );
        log::info!(
            "DEBUG: git stderr = {}",
            String::from_utf8_lossy(&output.stderr)
        );

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to remove worktree: {}", stderr);
        }

        // Delete the branch
        log::info!("Deleting branch {}", branch_name);
        let output = std::process::Command::new("git")
            .args(["branch", "-D", branch_name])
            .current_dir(&repo_path)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            log::warn!("Failed to delete branch {}: {}", branch_name, stderr);
        }

        log::info!(
            "Successfully deleted worktree at {}",
            worktree_path.display()
        );
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
            log::warn!(
                "Worktree for issue #{} does not exist at {}, skipping deletion",
                issue_number,
                worktree_path.display()
            );
            return Ok(());
        }

        log::info!("Deleting worktree for issue #{}", issue_number);

        // Read and run teardown commands
        let teardown_commands = Self::read_teardown_commands(&repo_path)?;

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
                    .env("BOTSTER_BRANCH_NAME", &branch_name)
                    .env(
                        "BOTSTER_WORKTREE_PATH",
                        worktree_path.to_str().expect("path is valid UTF-8"),
                    )
                    .env(
                        "BOTSTER_BIN",
                        std::env::current_exe()
                            .ok()
                            .and_then(|p| p.to_str().map(std::string::ToString::to_string))
                            .unwrap_or_else(|| "botster".to_string()),
                    )
                    .output()?;

                if output.status.success() {
                    log::debug!(
                        "Teardown output: {}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                } else {
                    log::warn!(
                        "Teardown command failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }

        // Remove the worktree using git
        log::info!("Removing worktree at {}", worktree_path.display());
        let output = std::process::Command::new("git")
            .args([
                "worktree",
                "remove",
                worktree_path.to_str().expect("path is valid UTF-8"),
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
            .args(["branch", "-D", &branch_name])
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

/// Checks whether a path is a git worktree (has a `.git` file, not directory).
fn git_is_worktree(path: &Path) -> bool {
    let git_path = path.join(".git");
    // Worktrees have a .git *file* pointing to the main repo's worktree directory.
    // Main repos have a .git *directory*.
    git_path.is_file()
}

/// Returns the origin remote URL for the repo at `path`.
fn git_remote_url(path: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(path)
        .output()
        .context("Failed to run git remote get-url")?;

    if !output.status.success() {
        anyhow::bail!("No origin remote configured");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Checks whether a local branch exists in the repo at `path`.
fn git_branch_exists(path: &Path, branch_name: &str) -> bool {
    std::process::Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch_name}")])
        .current_dir(path)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Returns the path to the main repository from a worktree via `git-common-dir`.
///
/// For worktrees, `git rev-parse --git-common-dir` returns the main repo's `.git`
/// directory. This function returns its parent (the repo root).
fn git_common_dir(path: &Path) -> Result<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(path)
        .output()
        .context("Failed to run git rev-parse --git-common-dir")?;

    if !output.status.success() {
        anyhow::bail!("Not in a git repository");
    }

    let git_common = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());

    // `--git-common-dir` returns the .git directory; we want its parent (repo root)
    // The path may be relative to `path`, so canonicalize from there
    let absolute = if git_common.is_absolute() {
        git_common
    } else {
        path.join(&git_common)
    };

    absolute
        .canonicalize()
        .context("Failed to canonicalize git common dir")?
        .parent()
        .context("Failed to get parent of .git directory")?
        .canonicalize()
        .context("Failed to canonicalize repo root")
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
