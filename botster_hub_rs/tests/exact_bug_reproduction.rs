/// This test reproduces the EXACT bug the user is experiencing:
/// "Failed to remove worktree: fatal: '/Users/jasonconigliari/Rails/trybotster' is a main working tree"
///
/// The issue is that delete_worktree_by_path is receiving the MAIN repo path instead of the worktree path
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
fn test_exact_bug_main_repo_path_passed_to_delete() {
    // This reproduces the EXACT scenario where delete_worktree_by_path
    // receives the MAIN repository path (like /Users/jasonconigliari/Rails/trybotster)
    // instead of the worktree path

    let temp_dir = TempDir::new().unwrap();

    // Create main repository at a path like /Users/jasonconigliari/Rails/trybotster
    let main_repo = temp_dir.path().join("trybotster");
    std::fs::create_dir_all(&main_repo).unwrap();
    setup_test_repo(&main_repo);

    // Create worktree directory
    let worktree_base = temp_dir.path().join("worktrees");
    std::fs::create_dir_all(&worktree_base).unwrap();

    let manager = WorktreeManager::new(worktree_base.clone());

    // Create a worktree for branch "center-home-index"
    let branch_name = "center-home-index";
    let worktree_path = worktree_base.join(format!("trybotster-{}", branch_name));

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

    assert!(output.status.success(), "Failed to create worktree");
    assert!(worktree_path.exists(), "Worktree should exist");

    // NOW HERE'S THE BUG:
    // If delete_worktree_by_path uses detect_current_repo() while cwd is main_repo,
    // it will get main_repo as the repo_path, then try to delete main_repo instead of worktree_path

    // Change to main repo directory (this is what triggers the bug)
    let original_cwd = env::current_dir().unwrap();
    env::set_current_dir(&main_repo).expect("Failed to change to main repo");

    // This call should delete the WORKTREE at worktree_path
    // But if buggy, it will try to delete MAIN_REPO at main_repo
    let result = manager.delete_worktree_by_path(&worktree_path, branch_name);

    // Restore cwd
    env::set_current_dir(&original_cwd).expect("Failed to restore cwd");

    // The bug manifests as this error:
    // "Failed to remove worktree: fatal: '/path/to/main_repo' is a main working tree"
    //
    // This happens because the code does:
    // 1. detect_current_repo() -> returns main_repo path (because cwd is main_repo)
    // 2. git worktree remove <worktree_path> --force (from main_repo directory)
    // 3. But somehow it's trying to remove main_repo itself

    if let Err(e) = &result {
        let err_msg = format!("{}", e);
        if err_msg.contains("is a main working tree") {
            panic!("BUG REPRODUCED! Got the exact error: {}", err_msg);
        }
    }

    assert!(
        result.is_ok(),
        "Should delete worktree successfully. Error: {:?}",
        result.err()
    );

    assert!(!worktree_path.exists(), "Worktree should be deleted");
}

#[test]
fn test_bug_scenario_with_logging() {
    // This test adds more logging to understand what's happening

    let temp_dir = TempDir::new().unwrap();
    let main_repo = temp_dir.path().join("trybotster");
    std::fs::create_dir_all(&main_repo).unwrap();
    setup_test_repo(&main_repo);

    let worktree_base = temp_dir.path().join("worktrees");
    std::fs::create_dir_all(&worktree_base).unwrap();

    let manager = WorktreeManager::new(worktree_base.clone());

    let branch_name = "center-home-index";
    let worktree_path = worktree_base.join(format!("trybotster-{}", branch_name));

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

    println!("Main repo path: {}", main_repo.display());
    println!("Worktree path: {}", worktree_path.display());
    println!("Branch name: {}", branch_name);

    // List worktrees before deletion
    let output = Command::new("git")
        .args(&["worktree", "list", "--porcelain"])
        .current_dir(&main_repo)
        .output()
        .unwrap();

    println!(
        "Worktrees before deletion:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let original_cwd = env::current_dir().unwrap();
    println!("Original cwd: {}", original_cwd.display());

    env::set_current_dir(&main_repo).expect("Failed to change to main repo");
    println!("Changed cwd to: {}", env::current_dir().unwrap().display());

    let result = manager.delete_worktree_by_path(&worktree_path, branch_name);

    env::set_current_dir(&original_cwd).expect("Failed to restore cwd");

    if let Err(e) = &result {
        println!("ERROR: {}", e);
        let err_msg = format!("{}", e);
        if err_msg.contains("is a main working tree") {
            panic!("BUG REPRODUCED with logging! Error: {}", err_msg);
        }
    }

    assert!(result.is_ok(), "Error: {:?}", result.err());
}
