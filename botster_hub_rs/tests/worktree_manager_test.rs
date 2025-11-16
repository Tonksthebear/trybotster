/// Integration test for WorktreeManager that reproduces and validates the bug fix
use botster_hub::WorktreeManager;
use std::process::Command;
use tempfile::TempDir;

fn setup_test_repo(path: &std::path::Path) {
    Command::new("git")
        .args(&["init"])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["config", "user.email", "test@test.com"])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["config", "user.name", "Test"])
        .current_dir(path)
        .output()
        .unwrap();
    std::fs::write(path.join("README.md"), "test").unwrap();
    Command::new("git")
        .args(&["add", "."])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["commit", "-m", "init"])
        .current_dir(path)
        .output()
        .unwrap();
}

#[test]
fn test_worktree_manager_handles_existing_branch() {
    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path().to_path_buf();

    // Setup a fake "cloned" repo
    let repo_dir = base_dir.join("owner-repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    setup_test_repo(&repo_dir);

    let manager = WorktreeManager::new(base_dir.clone());

    // Manually create the branch first (simulates previous run)
    let branch_name = "botster-owner-repo-1";
    Command::new("git")
        .args(&["branch", branch_name])
        .current_dir(&repo_dir)
        .output()
        .unwrap();

    // Now try to create worktree - should reuse existing branch
    // This would fail with "reference already exists" before the fix
    let result = manager.create_worktree("owner/repo", 1);

    assert!(
        result.is_ok(),
        "Should handle existing branch: {:?}",
        result.err()
    );

    let worktree_path = result.unwrap();
    assert!(worktree_path.exists(), "Worktree should be created");
    assert!(
        worktree_path.join(".claude/trusted").exists(),
        "Should be marked as trusted"
    );
}

#[test]
fn test_worktree_manager_creates_new_branch_when_none_exists() {
    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path().to_path_buf();

    let repo_dir = base_dir.join("test-repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    setup_test_repo(&repo_dir);

    let manager = WorktreeManager::new(base_dir.clone());

    // Create worktree - branch doesn't exist yet
    let result = manager.create_worktree("test/repo", 42);

    assert!(
        result.is_ok(),
        "Should create new branch: {:?}",
        result.err()
    );

    let worktree_path = result.unwrap();
    assert!(worktree_path.exists());

    // Verify branch was created
    let output = Command::new("git")
        .args(&["branch"])
        .current_dir(&repo_dir)
        .output()
        .unwrap();

    let branches = String::from_utf8_lossy(&output.stdout);
    assert!(branches.contains("botster-test-repo-42"));
}

#[test]
fn test_worktree_manager_recreates_after_cleanup() {
    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path().to_path_buf();

    let repo_dir = base_dir.join("my-repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    setup_test_repo(&repo_dir);

    let manager = WorktreeManager::new(base_dir.clone());

    // Create worktree first time
    let worktree1 = manager.create_worktree("my/repo", 5).unwrap();
    assert!(worktree1.exists());

    // Clean it up
    manager.cleanup_worktree(&repo_dir, &worktree1).unwrap();

    // Create it again - THIS IS THE BUG SCENARIO
    // Before fix: fails with "reference already exists"
    // After fix: should succeed
    let result = manager.create_worktree("my/repo", 5);

    assert!(
        result.is_ok(),
        "Should recreate worktree after cleanup. Error: {:?}",
        result.err()
    );

    let worktree2 = result.unwrap();
    assert!(worktree2.exists(), "Worktree should exist after recreation");
}

#[test]
fn test_cleanup_removes_worktree_from_git() {
    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path().to_path_buf();

    let repo_dir = base_dir.join("cleanup-test");
    std::fs::create_dir_all(&repo_dir).unwrap();
    setup_test_repo(&repo_dir);

    let manager = WorktreeManager::new(base_dir.clone());

    // Create worktree
    let worktree_path = manager.create_worktree("cleanup/test", 1).unwrap();

    // Verify it's in git worktree list
    let output = Command::new("git")
        .args(&["worktree", "list"])
        .current_dir(&repo_dir)
        .output()
        .unwrap();

    let worktrees = String::from_utf8_lossy(&output.stdout);
    assert!(worktrees.contains("cleanup-test-1"));

    // Clean it up
    manager.cleanup_worktree(&repo_dir, &worktree_path).unwrap();

    // Verify it's removed from git
    let output = Command::new("git")
        .args(&["worktree", "list"])
        .current_dir(&repo_dir)
        .output()
        .unwrap();

    let worktrees = String::from_utf8_lossy(&output.stdout);
    assert!(
        !worktrees.contains("cleanup-test-1"),
        "Worktree should be removed from git"
    );
}
