/// Debug test to understand what paths git2 returns for worktrees
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
fn debug_worktree_path_detection() {
    let temp_dir = TempDir::new().unwrap();
    let main_repo = temp_dir.path().join("main_repo");
    std::fs::create_dir_all(&main_repo).unwrap();
    setup_test_repo(&main_repo);

    let worktree_path = temp_dir.path().join("my_worktree");

    let output = Command::new("git")
        .args(&[
            "worktree",
            "add",
            "-b",
            "test-branch",
            worktree_path.to_str().unwrap(),
        ])
        .current_dir(&main_repo)
        .output()
        .unwrap();

    assert!(output.status.success());

    println!("\n=== Paths ===");
    println!("Main repo: {}", main_repo.display());
    println!("Worktree: {}", worktree_path.display());

    // Open worktree with git2
    let repo = git2::Repository::open(&worktree_path).unwrap();

    println!("\n=== git2 Info ===");
    println!("is_worktree(): {}", repo.is_worktree());
    println!("path(): {:?}", repo.path());

    println!("commondir(): {:?}", repo.commondir());

    println!("\n=== Path navigation ===");
    let path = repo.path();
    println!("repo.path() = {:?}", path);

    if let Some(parent1) = path.parent() {
        println!("repo.path().parent() = {:?}", parent1);

        if let Some(parent2) = parent1.parent() {
            println!("repo.path().parent().parent() = {:?}", parent2);
        }
    }

    println!("\n=== Expected ===");
    println!("Should find main repo at: {}", main_repo.display());

    // What does the actual implementation calculate?
    let calculated_repo_path = if repo.is_worktree() {
        let common_dir = repo.commondir();
        common_dir.parent().unwrap().to_path_buf()
    } else {
        repo.path().parent().unwrap().to_path_buf()
    };

    println!("\n=== Calculated by current code ===");
    println!("Calculated repo_path: {}", calculated_repo_path.display());
    println!(
        "Is this the main repo? {}",
        calculated_repo_path == main_repo
    );

    // Canonicalize both paths to handle /var vs /private/var symlink on macOS
    let calculated_canonical = std::fs::canonicalize(&calculated_repo_path).unwrap();
    let expected_canonical = std::fs::canonicalize(&main_repo).unwrap();

    assert_eq!(
        calculated_canonical, expected_canonical,
        "Should calculate correct main repo path"
    );
}
