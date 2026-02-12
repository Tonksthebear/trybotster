/// Debug test to verify worktree path detection using git CLI.
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn setup_test_repo(path: &std::path::Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .output()
        .unwrap();
    std::fs::write(path.join("README.md"), "test").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
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
        .args([
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

    // Worktrees have a .git *file*, main repos have a .git *directory*
    let git_path = worktree_path.join(".git");
    let is_worktree = git_path.is_file();
    println!("\n=== Worktree detection ===");
    println!(".git is file (worktree): {}", is_worktree);
    assert!(is_worktree, "Worktree should have .git as a file");

    // Main repo should have .git as a directory
    let main_git = main_repo.join(".git");
    assert!(main_git.is_dir(), "Main repo should have .git as a directory");

    // Resolve common dir via git CLI
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(&worktree_path)
        .output()
        .unwrap();
    assert!(output.status.success());

    let git_common = String::from_utf8_lossy(&output.stdout).trim().to_string();
    println!("git-common-dir: {}", git_common);

    let git_common_path = PathBuf::from(&git_common);
    let absolute = if git_common_path.is_absolute() {
        git_common_path
    } else {
        worktree_path.join(&git_common_path)
    };

    let calculated_repo_path = absolute
        .canonicalize()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    println!("\n=== Calculated ===");
    println!("Calculated repo_path: {}", calculated_repo_path.display());

    // Canonicalize both paths to handle /var vs /private/var symlink on macOS
    let expected_canonical = std::fs::canonicalize(&main_repo).unwrap();

    assert_eq!(
        calculated_repo_path, expected_canonical,
        "Should calculate correct main repo path"
    );
}
