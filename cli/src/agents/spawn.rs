//! Agent spawning configuration and utilities.
//!
//! This module provides the configuration types and helper utilities
//! needed for spawning new agents.

use std::path::PathBuf;

/// Configuration for spawning a new agent.
///
/// Contains all the information needed to create and initialize a new agent
/// instance, including the worktree location, prompt, and identification info.
#[derive(Debug, Clone)]
pub struct AgentSpawnConfig {
    /// Issue number this agent is working on (if any).
    pub issue_number: Option<u32>,

    /// Branch name for the agent's worktree.
    pub branch_name: String,

    /// Path to the git worktree directory.
    pub worktree_path: PathBuf,

    /// Path to the main repository.
    pub repo_path: PathBuf,

    /// Repository name in "owner/repo" format.
    pub repo_name: String,

    /// Initial prompt to send to the agent.
    pub prompt: String,

    /// Server message ID that triggered this spawn (if any).
    pub message_id: Option<i64>,

    /// Invocation URL for tracking this agent instance.
    pub invocation_url: Option<String>,

    /// Terminal dimensions (rows, cols) for PTY sizing.
    pub dims: (u16, u16),
}

impl AgentSpawnConfig {
    /// Creates a new spawn configuration with required fields.
    ///
    /// Optional fields (message_id, invocation_url) are set to None.
    /// Dims default to (24, 80).
    pub fn new(
        issue_number: Option<u32>,
        branch_name: String,
        worktree_path: PathBuf,
        repo_path: PathBuf,
        repo_name: String,
        prompt: String,
    ) -> Self {
        Self {
            issue_number,
            branch_name,
            worktree_path,
            repo_path,
            repo_name,
            prompt,
            message_id: None,
            invocation_url: None,
            dims: (24, 80),
        }
    }

    /// Sets the message ID for server-triggered spawns.
    pub fn with_message_id(mut self, message_id: i64) -> Self {
        self.message_id = Some(message_id);
        self
    }

    /// Sets the invocation URL for tracking.
    pub fn with_invocation_url(mut self, url: String) -> Self {
        self.invocation_url = Some(url);
        self
    }

    /// Generates the agent ID for this agent.
    ///
    /// The agent ID uniquely identifies this agent in the system.
    pub fn agent_id(&self) -> String {
        SessionKeyGenerator::generate(&self.repo_name, self.issue_number, &self.branch_name)
    }
}

/// Generates unique session keys for agents.
///
/// Session keys are used to identify agents across the system.
/// Format: `{repo-safe}-{identifier}` where identifier is either
/// the issue number or a sanitized branch name.
#[derive(Debug)]
pub struct SessionKeyGenerator;

impl SessionKeyGenerator {
    /// Generates a session key from the given parameters.
    ///
    /// # Arguments
    ///
    /// * `repo_name` - Repository in "owner/repo" format
    /// * `issue_number` - Optional issue number
    /// * `branch_name` - Branch name (used if no issue number)
    ///
    /// # Examples
    ///
    /// ```
    /// use botster_hub::agents::SessionKeyGenerator;
    ///
    /// // With issue number
    /// let key = SessionKeyGenerator::generate("owner/repo", Some(42), "issue-42");
    /// assert_eq!(key, "owner-repo-42");
    ///
    /// // Without issue number
    /// let key = SessionKeyGenerator::generate("owner/repo", None, "feature-branch");
    /// assert_eq!(key, "owner-repo-feature-branch");
    /// ```
    pub fn generate(repo_name: &str, issue_number: Option<u32>, branch_name: &str) -> String {
        let repo_safe = Self::sanitize_repo_name(repo_name);

        if let Some(num) = issue_number {
            format!("{}-{}", repo_safe, num)
        } else {
            format!("{}-{}", repo_safe, Self::sanitize_branch_name(branch_name))
        }
    }

    /// Sanitizes a repository name for use in a session key.
    ///
    /// Replaces "/" with "-" to create a safe identifier.
    pub fn sanitize_repo_name(repo_name: &str) -> String {
        repo_name.replace('/', "-")
    }

    /// Sanitizes a branch name for use in a session key.
    ///
    /// Replaces "/" with "-" to create a safe identifier.
    pub fn sanitize_branch_name(branch_name: &str) -> String {
        branch_name.replace('/', "-")
    }

    /// Extracts issue number from a session key if present.
    ///
    /// Returns None if the key doesn't end with a number.
    pub fn extract_issue_number(session_key: &str) -> Option<u32> {
        session_key.rsplit('-').next().and_then(|s| s.parse().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spawn_config_creation() {
        let config = AgentSpawnConfig::new(
            Some(42),
            "issue-42".to_string(),
            PathBuf::from("/tmp/worktree"),
            PathBuf::from("/tmp/repo"),
            "owner/repo".to_string(),
            "Fix the bug".to_string(),
        );

        assert_eq!(config.issue_number, Some(42));
        assert_eq!(config.branch_name, "issue-42");
        assert_eq!(config.repo_name, "owner/repo");
        assert!(config.message_id.is_none());
        assert!(config.invocation_url.is_none());
    }

    #[test]
    fn test_spawn_config_builder_methods() {
        let config = AgentSpawnConfig::new(
            Some(42),
            "issue-42".to_string(),
            PathBuf::from("/tmp/worktree"),
            PathBuf::from("/tmp/repo"),
            "owner/repo".to_string(),
            "Fix the bug".to_string(),
        )
        .with_message_id(123)
        .with_invocation_url("https://example.com".to_string());

        assert_eq!(config.message_id, Some(123));
        assert_eq!(
            config.invocation_url,
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn test_spawn_config_agent_id() {
        let config = AgentSpawnConfig::new(
            Some(42),
            "issue-42".to_string(),
            PathBuf::from("/tmp/worktree"),
            PathBuf::from("/tmp/repo"),
            "owner/repo".to_string(),
            "Fix the bug".to_string(),
        );

        assert_eq!(config.agent_id(), "owner-repo-42");
    }

    #[test]
    fn test_session_key_with_issue_number() {
        let key = SessionKeyGenerator::generate("owner/repo", Some(42), "issue-42");
        assert_eq!(key, "owner-repo-42");
    }

    #[test]
    fn test_session_key_without_issue_number() {
        let key = SessionKeyGenerator::generate("owner/repo", None, "feature-branch");
        assert_eq!(key, "owner-repo-feature-branch");
    }

    #[test]
    fn test_session_key_with_nested_branch() {
        let key = SessionKeyGenerator::generate("owner/repo", None, "feature/nested/branch");
        assert_eq!(key, "owner-repo-feature-nested-branch");
    }

    #[test]
    fn test_sanitize_repo_name() {
        assert_eq!(
            SessionKeyGenerator::sanitize_repo_name("owner/repo"),
            "owner-repo"
        );
        assert_eq!(
            SessionKeyGenerator::sanitize_repo_name("org/nested/repo"),
            "org-nested-repo"
        );
    }

    #[test]
    fn test_sanitize_branch_name() {
        assert_eq!(
            SessionKeyGenerator::sanitize_branch_name("feature/test"),
            "feature-test"
        );
        assert_eq!(
            SessionKeyGenerator::sanitize_branch_name("simple-branch"),
            "simple-branch"
        );
    }

    #[test]
    fn test_extract_issue_number() {
        assert_eq!(
            SessionKeyGenerator::extract_issue_number("owner-repo-42"),
            Some(42)
        );
        assert_eq!(
            SessionKeyGenerator::extract_issue_number("owner-repo-feature-branch"),
            None
        );
        assert_eq!(
            SessionKeyGenerator::extract_issue_number("owner-repo-123"),
            Some(123)
        );
    }
}
