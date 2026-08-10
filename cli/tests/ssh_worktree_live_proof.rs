//! End-to-end proof that WorktreeManager worktree paths stay free of `:` for SSH-style origins.
//!
//! Cargo/macOS DYLD path lists treat `:` as a separator, so SSH remotes such as
//! `git@github.com:owner/repo` must never become directory components unchanged.
use botster::WorktreeManager;
use std::process::Command;
use tempfile::TempDir;

fn setup_repo_with_ssh_origin(path: &std::path::Path) {
    let run = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["init"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    std::fs::write(path.join("README.md"), "ssh path proof").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "init"]);
    run(&[
        "remote",
        "add",
        "origin",
        "git@github.com:trybotster/botster-tui.git",
    ]);
}

#[test]
fn worktree_manager_ssh_origin_path_has_no_colon() {
    let temp = TempDir::new().unwrap();
    let repo_dir = temp.path().join("src-repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    setup_repo_with_ssh_origin(&repo_dir);

    let sessions = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let manager = WorktreeManager::new(sessions);
    let branch = "project-pipelines-ticket_ssh_e2e_proof";
    let worktree_path = manager
        .create_worktree_for_repo_root(&repo_dir, branch)
        .expect("create worktree");

    let path = worktree_path.to_string_lossy();
    assert!(worktree_path.exists(), "worktree missing at {path}");
    assert!(
        !path.contains(':'),
        "worktree path must not contain ':' (DYLD): {path}"
    );
    assert!(
        !path.contains('@'),
        "worktree path must not contain '@': {path}"
    );
    let name = worktree_path
        .file_name()
        .and_then(|n| n.to_str())
        .expect("utf-8 basename");
    assert!(
        name.contains("git-github.com-trybotster-botster-tui"),
        "basename should include sanitized SSH identity: {name}"
    );
}
