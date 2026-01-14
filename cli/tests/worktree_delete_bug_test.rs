/// Test that reproduces the exact bug: "Failed to remove worktree: fatal: '/path' is a main working tree"
/// This happens when delete_worktree_by_path is called while cwd is the main repo
use botster_hub::WorktreeManager;
use std::env;
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

    // Add a remote to simulate a real repo
    Command::new("git")
        .args(&[
            "remote",
            "add",
            "origin",
            "https://github.com/test/repo.git",
        ])
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
fn test_delete_worktree_by_path_from_main_repo() {
    // This test reproduces the bug where delete_worktree_by_path fails
    // because it detects the main repo as the worktree to delete

    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path().join("worktrees");
    std::fs::create_dir_all(&base_dir).unwrap();

    // Create main repository
    let main_repo = temp_dir.path().join("main_repo");
    std::fs::create_dir_all(&main_repo).unwrap();
    setup_test_repo(&main_repo);

    // Save original cwd and change to main repo (this simulates the bug scenario)
    // Use ok() to handle case where cwd was deleted by another test
    let original_cwd = env::current_dir().ok();
    env::set_current_dir(&main_repo).expect("Failed to change directory to main repo");

    let manager = WorktreeManager::new(base_dir.clone());

    // Create a worktree with a custom branch
    let branch_name = "center-home-index";
    let worktree_path = base_dir.join(format!("test-repo-{}", branch_name));

    // Create worktree using git command directly
    let output = Command::new("git")
        .args(&[
            "worktree",
            "add",
            "-b",
            branch_name,
            worktree_path.to_str().unwrap(),
        ])
        .current_dir(&main_repo)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Failed to create worktree: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(worktree_path.exists(), "Worktree should exist");

    // Now try to delete the worktree while cwd is still the main repo
    // This is the BUG: delete_worktree_by_path calls detect_current_repo()
    // which returns the main repo path, then tries to delete it as if it were a worktree

    let result = manager.delete_worktree_by_path(&worktree_path, branch_name);

    // Restore original directory before asserting
    // Handle case where original was deleted by another test
    if let Some(ref cwd) = original_cwd {
        if cwd.exists() {
            env::set_current_dir(cwd).expect("Failed to restore directory");
        } else {
            env::set_current_dir(env::temp_dir()).expect("Failed to restore to temp");
        }
    } else {
        env::set_current_dir(env::temp_dir()).expect("Failed to restore to temp");
    }

    // The bug manifests as:
    // "Failed to remove worktree: fatal: '/path/to/main_repo' is a main working tree"
    // Because detect_current_repo() returns the main repo, and we try to delete that

    assert!(
        result.is_ok(),
        "Should delete worktree successfully. Error: {:?}",
        result.err()
    );

    // Verify worktree was actually deleted
    assert!(
        !worktree_path.exists(),
        "Worktree directory should be deleted"
    );

    // Verify branch was deleted
    let output = Command::new("git")
        .args(&["branch", "--list", branch_name])
        .current_dir(&main_repo)
        .output()
        .unwrap();

    let branches = String::from_utf8_lossy(&output.stdout);
    assert!(
        branches.trim().is_empty(),
        "Branch should be deleted, but found: {}",
        branches
    );
}

#[test]
fn test_delete_worktree_by_path_with_different_cwd() {
    // Test that deletion works even when cwd is completely different

    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path().join("worktrees");
    std::fs::create_dir_all(&base_dir).unwrap();

    // Create main repository
    let main_repo = temp_dir.path().join("project");
    std::fs::create_dir_all(&main_repo).unwrap();
    setup_test_repo(&main_repo);

    // Change to /tmp (completely unrelated directory)
    // Use ok() to handle case where cwd was deleted by another test
    let original_cwd = env::current_dir().ok();
    env::set_current_dir("/tmp").expect("Failed to change to /tmp");

    let manager = WorktreeManager::new(base_dir.clone());

    // Create a worktree
    let branch_name = "feature-test";
    let worktree_path = base_dir.join("project-feature-test");

    let output = Command::new("git")
        .args(&[
            "worktree",
            "add",
            "-b",
            branch_name,
            worktree_path.to_str().unwrap(),
        ])
        .current_dir(&main_repo)
        .output()
        .unwrap();

    assert!(output.status.success());

    // Try to delete from /tmp - the old code would fail here
    // because detect_current_repo() can't find a repo in /tmp
    let result = manager.delete_worktree_by_path(&worktree_path, branch_name);

    // Restore directory - handle case where original was deleted by another test
    if let Some(ref cwd) = original_cwd {
        if cwd.exists() {
            env::set_current_dir(cwd).expect("Failed to restore directory");
        } else {
            env::set_current_dir(env::temp_dir()).expect("Failed to restore to temp");
        }
    } else {
        env::set_current_dir(env::temp_dir()).expect("Failed to restore to temp");
    }

    assert!(
        result.is_ok(),
        "Should delete worktree even from unrelated directory. Error: {:?}",
        result.err()
    );

    assert!(!worktree_path.exists(), "Worktree should be deleted");
}

#[test]
fn test_delete_worktree_by_path_finds_correct_main_repo() {
    // Test that the fix correctly identifies the main repo from the worktree

    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path().join("worktrees");
    std::fs::create_dir_all(&base_dir).unwrap();

    // Create MULTIPLE main repositories to ensure we get the right one
    let repo1 = temp_dir.path().join("repo1");
    let repo2 = temp_dir.path().join("repo2");
    std::fs::create_dir_all(&repo1).unwrap();
    std::fs::create_dir_all(&repo2).unwrap();
    setup_test_repo(&repo1);
    setup_test_repo(&repo2);

    // Create worktree from repo2
    let branch_name = "test-branch";
    let worktree_path = base_dir.join("repo2-worktree");

    let output = Command::new("git")
        .args(&[
            "worktree",
            "add",
            "-b",
            branch_name,
            worktree_path.to_str().unwrap(),
        ])
        .current_dir(&repo2) // Note: from repo2, not repo1
        .output()
        .unwrap();

    assert!(output.status.success());

    // Change cwd to repo1 (wrong repo)
    // Use ok() to handle case where cwd was deleted by another test
    let original_cwd = env::current_dir().ok();
    env::set_current_dir(&repo1).expect("Failed to change to repo1");

    let manager = WorktreeManager::new(base_dir.clone());

    // Delete worktree - should use repo2, NOT repo1 (current directory)
    let result = manager.delete_worktree_by_path(&worktree_path, branch_name);

    // Restore directory - handle case where original was deleted by another test
    if let Some(ref cwd) = original_cwd {
        if cwd.exists() {
            env::set_current_dir(cwd).expect("Failed to restore directory");
        } else {
            env::set_current_dir(env::temp_dir()).expect("Failed to restore to temp");
        }
    } else {
        env::set_current_dir(env::temp_dir()).expect("Failed to restore to temp");
    }

    assert!(
        result.is_ok(),
        "Should find correct main repo from worktree. Error: {:?}",
        result.err()
    );

    // Verify the branch was deleted from repo2, not repo1
    let output = Command::new("git")
        .args(&["branch", "--list", branch_name])
        .current_dir(&repo2)
        .output()
        .unwrap();

    assert!(
        String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "Branch should be deleted from repo2"
    );

    // Verify repo1 wasn't affected
    let output = Command::new("git")
        .args(&["branch", "--list"])
        .current_dir(&repo1)
        .output()
        .unwrap();

    let branches = String::from_utf8_lossy(&output.stdout);
    assert!(
        !branches.contains(branch_name),
        "repo1 should not have the branch"
    );
}
