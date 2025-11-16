/// This test reproduces the exact bug: "reference already exists"
/// This happens when:
/// 1. A worktree was created before
/// 2. The worktree directory was removed BUT the git reference remains
/// 3. We try to create the same worktree again
use std::process::Command;
use tempfile::TempDir;

#[test]
fn test_reproduces_reference_already_exists_bug() {
    let temp_dir = TempDir::new().unwrap();
    let clone_dir = temp_dir.path().join("test-repo");

    // Setup: Create a real git repo
    std::fs::create_dir_all(&clone_dir).unwrap();

    Command::new("git")
        .args(&["init"])
        .current_dir(&clone_dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["config", "user.email", "test@test.com"])
        .current_dir(&clone_dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["config", "user.name", "Test"])
        .current_dir(&clone_dir)
        .output()
        .unwrap();

    std::fs::write(clone_dir.join("README.md"), "test").unwrap();
    Command::new("git")
        .args(&["add", "."])
        .current_dir(&clone_dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["commit", "-m", "init"])
        .current_dir(&clone_dir)
        .output()
        .unwrap();

    let branch_name = "botster-Tonksthebear-trybotster-6";
    let worktree_path = temp_dir.path().join("worktree-6");

    // Step 1: Create worktree first time (should succeed)
    let output = Command::new("git")
        .args(&[
            "worktree",
            "add",
            "-b",
            branch_name,
            worktree_path.to_str().unwrap(),
        ])
        .current_dir(&clone_dir)
        .output()
        .unwrap();

    assert!(output.status.success(), "First worktree creation failed");
    assert!(worktree_path.exists(), "Worktree directory should exist");

    // Step 2: Remove the worktree using git (simulates what happens in production)
    let output = Command::new("git")
        .args(&[
            "worktree",
            "remove",
            worktree_path.file_name().unwrap().to_str().unwrap(),
            "--force",
        ])
        .current_dir(&clone_dir)
        .output()
        .unwrap();

    assert!(output.status.success(), "Worktree removal should succeed");

    // Step 3: The branch reference STILL EXISTS even though worktree is gone
    let output = Command::new("git")
        .args(&["branch"])
        .current_dir(&clone_dir)
        .output()
        .unwrap();

    let branches = String::from_utf8_lossy(&output.stdout);
    assert!(
        branches.contains(branch_name),
        "Branch reference should still exist after worktree removal"
    );

    // Step 4: Try to create worktree again with SAME branch name
    // THIS IS THE BUG - it fails with "reference already exists"
    let output = Command::new("git")
        .args(&[
            "worktree",
            "add",
            "-b",
            branch_name,
            worktree_path.to_str().unwrap(),
        ])
        .current_dir(&clone_dir)
        .output()
        .unwrap();

    // This is what currently happens (the bug):
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("Git output: {}", stderr);

    // The bug: git fails because branch already exists
    assert!(
        stderr.contains("already exists") || !output.status.success(),
        "BUG REPRODUCED: Git fails when branch already exists"
    );
}

#[test]
fn test_worktree_creation_should_reuse_existing_branch() {
    let temp_dir = TempDir::new().unwrap();
    let clone_dir = temp_dir.path().join("test-repo");

    // Setup git repo
    std::fs::create_dir_all(&clone_dir).unwrap();
    Command::new("git")
        .args(&["init"])
        .current_dir(&clone_dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["config", "user.email", "test@test.com"])
        .current_dir(&clone_dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["config", "user.name", "Test"])
        .current_dir(&clone_dir)
        .output()
        .unwrap();
    std::fs::write(clone_dir.join("README.md"), "test").unwrap();
    Command::new("git")
        .args(&["add", "."])
        .current_dir(&clone_dir)
        .output()
        .unwrap();
    Command::new("git")
        .args(&["commit", "-m", "init"])
        .current_dir(&clone_dir)
        .output()
        .unwrap();

    let branch_name = "botster-test-1";
    let worktree_path = temp_dir.path().join("worktree-1");

    // Create worktree first time
    Command::new("git")
        .args(&[
            "worktree",
            "add",
            "-b",
            branch_name,
            worktree_path.to_str().unwrap(),
        ])
        .current_dir(&clone_dir)
        .output()
        .unwrap();

    // Remove worktree
    Command::new("git")
        .args(&[
            "worktree",
            "remove",
            worktree_path.file_name().unwrap().to_str().unwrap(),
            "--force",
        ])
        .current_dir(&clone_dir)
        .output()
        .unwrap();

    // THE FIX: When branch exists, don't use -b flag, just checkout existing branch
    let output = Command::new("git")
        .args(&[
            "worktree",
            "add",
            worktree_path.to_str().unwrap(),
            branch_name,
        ]) // No -b flag!
        .current_dir(&clone_dir)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("Git stderr: {}", stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("Git stdout: {}", stdout);

    // This should succeed!
    assert!(
        output.status.success(),
        "Should be able to recreate worktree using existing branch. Error: {}",
        stderr
    );
    assert!(worktree_path.exists(), "Worktree should be created");
}
