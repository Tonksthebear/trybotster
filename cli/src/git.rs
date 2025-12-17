use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use serde_json;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Extension trait for safe path-to-string conversion
trait PathExt {
    fn to_str_safe(&self) -> Result<&str>;
}

impl PathExt for Path {
    fn to_str_safe(&self) -> Result<&str> {
        self.to_str()
            .ok_or_else(|| anyhow::anyhow!("Path contains invalid UTF-8: {:?}", self))
    }
}

impl PathExt for PathBuf {
    fn to_str_safe(&self) -> Result<&str> {
        self.to_str()
            .ok_or_else(|| anyhow::anyhow!("Path contains invalid UTF-8: {:?}", self))
    }
}

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

        log::info!(
            "Found {} patterns in .botster_copy: {:?}",
            patterns.len(),
            patterns
        );
        log::info!("Source repo: {}", source_repo.display());
        log::info!("Dest worktree: {}", dest_worktree.display());

        // Write debug info to file for troubleshooting
        let debug_log = format!(
            "[copy_botster_files] patterns={:?}, source={}, dest={}\n",
            patterns,
            source_repo.display(),
            dest_worktree.display()
        );
        std::fs::write("/tmp/botster_debug.log", &debug_log).ok();

        // Build globset from patterns
        let mut builder = GlobSetBuilder::new();
        for pattern in &patterns {
            match Glob::new(pattern) {
                Ok(glob) => {
                    builder.add(glob);
                }
                Err(e) => {
                    log::warn!("Invalid pattern in .botster_copy: '{}' - {}", pattern, e);
                    continue;
                }
            }
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

        let read_result = fs::read_dir(current_dir);
        if let Err(e) = &read_result {
            log::warn!("Failed to read directory {}: {}", current_dir.display(), e);
            return Ok(()); // Continue despite errors
        }

        for entry in read_result.unwrap() {
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
                if globset.is_match(&rel_path) {
                    // Copy matching file
                    let dest_path = dest_root.join(&rel_path);

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
                                .open("/tmp/botster_debug.log")
                            {
                                file.write_all(debug_msg.as_bytes()).ok();
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

        let repo_obj = git2::Repository::open(&repo_path)?;

        // Check if branch exists
        let branch_exists = repo_obj
            .find_branch(branch_name, git2::BranchType::Local)
            .is_ok();

        // Create worktree using git command
        let output = if branch_exists {
            log::info!("Using existing branch: {}", branch_name);
            std::process::Command::new("git")
                .args(&[
                    "worktree",
                    "add",
                    worktree_path.to_str().unwrap(),
                    branch_name,
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
                    branch_name,
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
        if worktree_path.exists() {
            log::info!("Removing existing worktree at {}", worktree_path.display());

            // Try to remove with git worktree remove
            let remove_result = std::process::Command::new("git")
                .args(&[
                    "worktree",
                    "remove",
                    worktree_path.to_str().unwrap(),
                    "--force",
                ])
                .current_dir(clone_dir)
                .output();

            // If git command fails, try to prune and remove directory manually
            if remove_result.is_err() || !remove_result.as_ref().unwrap().status.success() {
                log::warn!("Git worktree remove failed, trying prune...");
                std::process::Command::new("git")
                    .args(&["worktree", "prune"])
                    .current_dir(clone_dir)
                    .output()
                    .ok();

                // Manually remove the directory if it still exists
                if worktree_path.exists() {
                    std::fs::remove_dir_all(worktree_path).ok();
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
            .args(&["worktree", "list"])
            .current_dir(&clone_dir)
            .output()?;

        let list = String::from_utf8_lossy(&output.stdout);
        Ok(list.lines().map(|s| s.to_string()).collect())
    }

    /// Finds an existing worktree for a given issue number
    /// Returns the worktree path and branch name if found
    pub fn find_existing_worktree_for_issue(&self, issue_number: u32) -> Result<Option<(PathBuf, String)>> {
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
            log::warn!("Directory exists but is not a git worktree: {}", worktree_path.display());
            return Ok(None);
        }

        // Verify the worktree is valid by checking if git recognizes it
        let output = std::process::Command::new("git")
            .args(&["worktree", "list", "--porcelain"])
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
                    log::info!("Found existing worktree for issue #{} at {}", issue_number, worktree_path.display());
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
                .args(&["worktree", "prune"])
                .current_dir(&clone_dir)
                .output()?;
        }
        Ok(())
    }

    /// Deletes a worktree by path, running teardown scripts first
    ///
    /// # Safety
    /// This function has multiple defense-in-depth checks to prevent accidental
    /// deletion of the main repository or other important directories.
    pub fn delete_worktree_by_path(
        &self,
        worktree_path: &std::path::Path,
        branch_name: &str,
    ) -> Result<()> {
        // DEFENSE-IN-DEPTH CHECK 1: Verify path is within managed base directory
        let canonical_worktree = worktree_path.canonicalize()
            .context("Failed to canonicalize worktree path")?;
        let canonical_base = self.base_dir.canonicalize()
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

        // Find the main repository from the worktree
        // Worktrees have a .git file (not directory) that points to the main repo
        let repo_obj = git2::Repository::open(worktree_path)
            .context("Failed to open worktree as git repository")?;

        log::debug!("worktree_path = {}", worktree_path.display());
        log::debug!("is_worktree() = {}", repo_obj.is_worktree());

        // DEFENSE-IN-DEPTH CHECK 4: Git's is_worktree check
        if !repo_obj.is_worktree() {
            log::error!(
                "CRITICAL: Refusing to delete main repository at {}. This is not a worktree!",
                worktree_path.display()
            );
            anyhow::bail!(
                "Cannot delete main repository at {}. Only worktrees can be deleted.",
                worktree_path.display()
            );
        }

        let repo_path = if repo_obj.is_worktree() {
            // This is a worktree - find the main repository
            // Use commondir() which returns the path to the main repo's .git directory
            let common_dir = repo_obj.commondir();
            let result = common_dir
                .parent()
                .context("Failed to find main repository from commondir")?
                .to_path_buf();
            log::info!(
                "DEBUG: Calculated repo_path from worktree = {}",
                result.display()
            );
            result
        } else {
            // This is the main repository
            let result = repo_obj
                .path()
                .parent()
                .context("Failed to get repo path")?
                .to_path_buf();
            log::info!(
                "DEBUG: Calculated repo_path from main repo = {}",
                result.display()
            );
            result
        };

        // Get repo name from the remote URL or directory name
        let repo_name = if let Ok(remote) = repo_obj.find_remote("origin") {
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

        if !worktree_path.exists() {
            log::warn!(
                "Worktree at {} does not exist, skipping deletion",
                worktree_path.display()
            );
            return Ok(());
        }

        log::info!("Deleting worktree at {}", worktree_path.display());

        // Read and run teardown commands
        let teardown_commands = Self::read_botster_teardown_commands(&repo_path)?;

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
            .args(&[
                "worktree",
                "remove",
                worktree_path.to_str().unwrap(),
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
            .args(&["branch", "-D", branch_name])
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
                    .env("BOTSTER_BRANCH_NAME", &branch_name)
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
