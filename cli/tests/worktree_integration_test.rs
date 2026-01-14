use botster_hub::Agent;
use std::process::Command;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn test_worktree_creation_with_real_git() {
    let temp_dir = TempDir::new().unwrap();

    // Initialize a test git repo
    let test_repo = temp_dir.path().join("test-repo");
    std::fs::create_dir_all(&test_repo).unwrap();

    Command::new("git")
        .args(&["init"])
        .current_dir(&test_repo)
        .output()
        .unwrap();

    Command::new("git")
        .args(&["config", "user.email", "test@example.com"])
        .current_dir(&test_repo)
        .output()
        .unwrap();

    Command::new("git")
        .args(&["config", "user.name", "Test User"])
        .current_dir(&test_repo)
        .output()
        .unwrap();

    // Create initial commit
    std::fs::write(test_repo.join("README.md"), "# Test\n").unwrap();

    Command::new("git")
        .args(&["add", "."])
        .current_dir(&test_repo)
        .output()
        .unwrap();

    Command::new("git")
        .args(&["commit", "-m", "Initial commit"])
        .current_dir(&test_repo)
        .output()
        .unwrap();

    // Now test listing worktrees
    let output = Command::new("git")
        .args(&["worktree", "list"])
        .current_dir(&test_repo)
        .output()
        .unwrap();

    let worktrees = String::from_utf8_lossy(&output.stdout);
    assert!(worktrees.contains("test-repo"));
}

#[test]
fn test_agent_spawns_with_echo_command() {
    let temp_dir = TempDir::new().unwrap();
    let worktree = temp_dir.path().to_path_buf();

    let id = Uuid::new_v4();
    let mut agent = Agent::new(
        id,
        "test/repo".to_string(),
        Some(1),
        "test-branch".to_string(),
        worktree,
    );

    // Spawn with echo command (simple, won't fail)
    let empty_env = std::collections::HashMap::new();
    let result = agent.spawn("echo test", "", vec![], &empty_env);

    // Should succeed in spawning
    assert!(result.is_ok());

    // Give it a moment to run
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Buffer should have spawn messages
    let snapshot = agent.get_buffer_snapshot();
    assert!(snapshot.iter().any(|line| line.contains("Spawning agent")));
}

#[test]
fn test_multiple_agents_different_issues() {
    let temp_dir = TempDir::new().unwrap();

    let agent1 = Agent::new(
        Uuid::new_v4(),
        "owner/repo".to_string(),
        Some(1),
        "botster-issue-1".to_string(),
        temp_dir.path().to_path_buf(),
    );

    let agent2 = Agent::new(
        Uuid::new_v4(),
        "owner/repo".to_string(),
        Some(2),
        "botster-issue-2".to_string(),
        temp_dir.path().to_path_buf(),
    );

    // Different issue numbers should have different session keys
    assert_ne!(agent1.session_key(), agent2.session_key());
    assert_eq!(agent1.session_key(), "owner-repo-1");
    assert_eq!(agent2.session_key(), "owner-repo-2");
}
