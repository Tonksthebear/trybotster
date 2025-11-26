# Rust Testing Guide for botster-hub

Complete guide to testing the botster-hub Rust CLI application.

## Table of Contents

- [Running Tests](#running-tests)
- [Test Organization](#test-organization)
- [Unit Tests](#unit-tests)
- [Integration Tests](#integration-tests)
- [Testing CLI Commands](#testing-cli-commands)
- [Testing with Git](#testing-with-git)
- [Best Practices](#best-practices)

---

## Running Tests

### Basic Commands

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run a specific test
cargo test test_name

# Run tests in a specific file
cargo test --test worktree_integration_test

# Run tests with logging
RUST_LOG=debug cargo test -- --nocapture

# Run tests in release mode (faster)
cargo test --release
```

### Parallel vs Sequential

```bash
# Run tests sequentially (useful for git operations)
cargo test -- --test-threads=1

# Run with specific number of threads
cargo test -- --test-threads=4
```

---

## Test Organization

### Project Structure

```
botster_hub/
├── src/
│   ├── main.rs          # CLI entry point
│   ├── lib.rs           # Library exports
│   ├── agent.rs         # Agent management
│   ├── config.rs        # Configuration
│   ├── git.rs           # Git operations
│   ├── prompt.rs        # Prompt generation
│   └── terminal.rs      # Terminal handling
├── tests/               # Integration tests
│   ├── agent_test.rs
│   ├── config_test.rs
│   ├── git_test.rs
│   ├── cli_test.rs
│   └── worktree_integration_test.rs
└── Cargo.toml
```

### Unit Tests vs Integration Tests

**Unit Tests** (in `src/` files):

- Test individual functions/methods
- Use `#[cfg(test)]` module
- Have access to private items
- Fast, isolated

**Integration Tests** (in `tests/` directory):

- Test public API as external user would
- Each file is a separate crate
- Can only use public exports from `lib.rs`
- More realistic, slower

---

## Unit Tests

### Inline Unit Tests

Add unit tests at the bottom of source files:

```rust
// src/agent.rs

impl Agent {
    pub fn new(id: Uuid, repo: String, issue_number: Option<u32>,
               branch_name: String, worktree_path: PathBuf) -> Self {
        // implementation
    }

    pub fn session_key(&self) -> String {
        if let Some(issue) = self.issue_number {
            format!("{}-{}", self.repo.replace("/", "-"), issue)
        } else {
            format!("{}-{}", self.repo.replace("/", "-"), self.branch_name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use std::path::PathBuf;

    #[test]
    fn test_session_key_with_issue() {
        let agent = Agent::new(
            Uuid::new_v4(),
            "owner/repo".to_string(),
            Some(123),
            "botster-issue-123".to_string(),
            PathBuf::from("/tmp/test"),
        );

        assert_eq!(agent.session_key(), "owner-repo-123");
    }

    #[test]
    fn test_session_key_without_issue() {
        let agent = Agent::new(
            Uuid::new_v4(),
            "owner/repo".to_string(),
            None,
            "feature-branch".to_string(),
            PathBuf::from("/tmp/test"),
        );

        assert_eq!(agent.session_key(), "owner-repo-feature-branch");
    }
}
```

### Running Unit Tests

```bash
# Run unit tests in a specific module
cargo test --lib agent::tests

# Run a specific unit test
cargo test test_session_key_with_issue
```

---

## Integration Tests

### Integration Test Structure

Create files in `tests/` directory:

```rust
// tests/agent_test.rs

use botster_hub::Agent;
use std::path::PathBuf;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn test_agent_creation() {
    let temp_dir = TempDir::new().unwrap();
    let worktree = temp_dir.path().to_path_buf();

    let agent = Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(1),
        "botster-issue-1".to_string(),
        worktree,
    );

    assert_eq!(agent.session_key(), "test-repo-1");
}

#[test]
fn test_agent_spawns_successfully() {
    let temp_dir = TempDir::new().unwrap();
    let worktree = temp_dir.path().to_path_buf();

    let mut agent = Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(1),
        "test-branch".to_string(),
        worktree,
    );

    // Use a simple command that won't fail
    let result = agent.spawn(
        "echo 'test'",
        "",
        vec![],
        std::collections::HashMap::new()
    );

    assert!(result.is_ok());
}
```

### Using Temporary Directories

Always use `tempfile::TempDir` for tests that need file system access:

```rust
use tempfile::TempDir;

#[test]
fn test_with_temp_dir() {
    let temp_dir = TempDir::new().unwrap();
    let path = temp_dir.path();

    // Create files, run git commands, etc.
    std::fs::write(path.join("test.txt"), "content").unwrap();

    assert!(path.join("test.txt").exists());

    // temp_dir is automatically cleaned up when dropped
}
```

---

## Testing CLI Commands

### Approach 1: Test Command Logic Separately

Extract command logic into testable functions:

```rust
// src/main.rs

fn handle_config(key: Option<String>, value: Option<String>) -> Result<()> {
    match (key, value) {
        (Some(k), Some(v)) => Config::set(&k, &v),
        (Some(k), None) => {
            let val = Config::get(&k)?;
            println!("{}", val);
            Ok(())
        }
        (None, None) => Config::list(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_get_set() {
        // Test the logic without running the CLI
        let result = handle_config(
            Some("test_key".to_string()),
            Some("test_value".to_string())
        );
        assert!(result.is_ok());
    }
}
```

### Approach 2: Integration Test with Command

Use `std::process::Command` to test the actual binary:

```rust
// tests/cli_test.rs

use std::process::Command;

#[test]
fn test_status_command() {
    let output = Command::new("cargo")
        .args(&["run", "--", "status"])
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success());
}

#[test]
fn test_json_get_command() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let json_file = temp_dir.path().join("test.json");

    std::fs::write(&json_file, r#"{"key": "value"}"#).unwrap();

    let output = Command::new("cargo")
        .args(&["run", "--", "json-get", json_file.to_str().unwrap(), "key"])
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "\"value\"");
}
```

### Approach 3: Test with Assert_cmd (Recommended)

Add to `Cargo.toml`:

```toml
[dev-dependencies]
assert_cmd = "2.0"
predicates = "3.0"
```

Then write tests:

```rust
// tests/cli_test.rs

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_status_command() {
    let mut cmd = Command::cargo_bin("botster-hub").unwrap();
    cmd.arg("status")
        .assert()
        .success();
}

#[test]
fn test_config_set_and_get() {
    let mut cmd = Command::cargo_bin("botster-hub").unwrap();
    cmd.args(&["config", "test_key", "test_value"])
        .assert()
        .success();

    let mut cmd = Command::cargo_bin("botster-hub").unwrap();
    cmd.args(&["config", "test_key"])
        .assert()
        .success()
        .stdout(predicate::str::contains("test_value"));
}
```

---

## Testing with Git

### Setup Test Repository

```rust
use std::process::Command;
use tempfile::TempDir;

fn setup_test_repo() -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let repo_path = temp_dir.path().join("test-repo");
    std::fs::create_dir_all(&repo_path).unwrap();

    // Initialize git repo
    Command::new("git")
        .args(&["init"])
        .current_dir(&repo_path)
        .output()
        .unwrap();

    Command::new("git")
        .args(&["config", "user.email", "test@example.com"])
        .current_dir(&repo_path)
        .output()
        .unwrap();

    Command::new("git")
        .args(&["config", "user.name", "Test User"])
        .current_dir(&repo_path)
        .output()
        .unwrap();

    // Create initial commit
    std::fs::write(repo_path.join("README.md"), "# Test\n").unwrap();

    Command::new("git")
        .args(&["add", "."])
        .current_dir(&repo_path)
        .output()
        .unwrap();

    Command::new("git")
        .args(&["commit", "-m", "Initial commit"])
        .current_dir(&repo_path)
        .output()
        .unwrap();

    temp_dir
}

#[test]
fn test_worktree_operations() {
    let temp_dir = setup_test_repo();
    let repo_path = temp_dir.path().join("test-repo");

    // Test worktree creation
    let worktree_path = temp_dir.path().join("worktree-1");

    let output = Command::new("git")
        .args(&["worktree", "add", worktree_path.to_str().unwrap(), "-b", "test-branch"])
        .current_dir(&repo_path)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(worktree_path.exists());
}
```

### Testing WorktreeManager

```rust
// tests/git_test.rs

use botster_hub::WorktreeManager;
use std::path::PathBuf;

#[test]
fn test_worktree_manager_list() {
    let temp_dir = setup_test_repo();
    let repo_path = temp_dir.path().join("test-repo");

    let manager = WorktreeManager::new(repo_path);
    let worktrees = manager.list_worktrees().unwrap();

    // Should have at least the main worktree
    assert!(!worktrees.is_empty());
}

#[test]
fn test_worktree_manager_create_and_delete() {
    let temp_dir = setup_test_repo();
    let repo_path = temp_dir.path().join("test-repo");

    let manager = WorktreeManager::new(repo_path);

    // Create worktree
    let worktree_path = temp_dir.path().join("test-worktree");
    manager.create_worktree(&worktree_path, "test-branch").unwrap();

    assert!(worktree_path.exists());

    // Delete worktree
    manager.delete_worktree(&worktree_path).unwrap();

    assert!(!worktree_path.exists());
}
```

---

## Best Practices

### 1. Use Result<()> for Test Functions

```rust
#[test]
fn test_something() -> Result<()> {
    let value = some_function()?;
    assert_eq!(value, expected);
    Ok(())
}
```

### 2. Use tempfile for File System Tests

```rust
use tempfile::TempDir;

#[test]
fn test_file_operations() {
    let temp_dir = TempDir::new().unwrap();
    // Files automatically cleaned up
}
```

### 3. Test Error Cases

```rust
#[test]
fn test_invalid_input_returns_error() {
    let result = parse_config("invalid");
    assert!(result.is_err());
}

#[test]
#[should_panic(expected = "Invalid configuration")]
fn test_panic_on_invalid_config() {
    load_config("bad_path");
}
```

### 4. Use Descriptive Test Names

```rust
// Good
#[test]
fn test_agent_spawns_with_valid_command()

#[test]
fn test_session_key_with_issue_number()

// Bad
#[test]
fn test_1()

#[test]
fn test_agent()
```

### 5. Test Public API, Not Implementation

```rust
// Good - tests public behavior
#[test]
fn test_agent_session_key_format() {
    let agent = Agent::new(...);
    assert_eq!(agent.session_key(), "expected-format");
}

// Bad - tests internal implementation
#[test]
fn test_internal_hash_calculation() {
    let hash = agent.calculate_internal_hash();  // private method
}
```

### 6. Group Related Tests

```rust
#[cfg(test)]
mod agent_tests {
    use super::*;

    mod session_key {
        use super::*;

        #[test]
        fn with_issue_number() { ... }

        #[test]
        fn without_issue_number() { ... }
    }

    mod spawning {
        use super::*;

        #[test]
        fn with_valid_command() { ... }

        #[test]
        fn with_invalid_command() { ... }
    }
}
```

### 7. Use Test Fixtures

```rust
// tests/common/mod.rs
pub fn create_test_agent() -> Agent {
    Agent::new(
        Uuid::new_v4(),
        "test/repo".to_string(),
        Some(1),
        "test-branch".to_string(),
        PathBuf::from("/tmp/test"),
    )
}

// tests/agent_test.rs
mod common;

#[test]
fn test_with_fixture() {
    let agent = common::create_test_agent();
    // Test with standard agent
}
```

### 8. Clean Up Resources

```rust
#[test]
fn test_with_cleanup() {
    let temp_dir = TempDir::new().unwrap();

    // Test code

    // TempDir automatically cleans up on drop
    // But for manual cleanup:
    drop(temp_dir);
}
```

### 9. Mock External Dependencies

For HTTP requests, use `mockito`:

```toml
[dev-dependencies]
mockito = "1.0"
```

```rust
#[test]
fn test_api_call() {
    let _m = mockito::mock("GET", "/api/messages")
        .with_status(200)
        .with_body(r#"{"messages": []}"#)
        .create();

    let client = create_client(&mockito::server_url());
    let response = client.get_messages().unwrap();

    assert!(response.messages.is_empty());
}
```

### 10. Test Concurrency Issues

```rust
#[test]
fn test_concurrent_agent_access() {
    use std::sync::Arc;
    use std::thread;

    let agent = Arc::new(create_test_agent());
    let mut handles = vec![];

    for _ in 0..10 {
        let agent_clone = Arc::clone(&agent);
        let handle = thread::spawn(move || {
            agent_clone.session_key()
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }
}
```

---

## Component-Specific Testing

### Testing Agent Module

```rust
// tests/agent_test.rs

use botster_hub::Agent;

#[test]
fn test_agent_lifecycle() {
    let mut agent = create_test_agent();

    // Spawn
    agent.spawn("echo test", "", vec![], HashMap::new()).unwrap();

    // Check status
    assert_eq!(agent.status(), AgentStatus::Running);

    // Get output
    std::thread::sleep(Duration::from_millis(100));
    let snapshot = agent.get_buffer_snapshot();
    assert!(!snapshot.is_empty());

    // Cleanup
    agent.cleanup().unwrap();
}
```

### Testing Config Module

```rust
// tests/config_test.rs

use botster_hub::Config;

#[test]
fn test_config_set_and_get() {
    Config::set("test_key", "test_value").unwrap();
    let value = Config::get("test_key").unwrap();
    assert_eq!(value, "test_value");
}
```

### Testing Prompt Module

```rust
// tests/prompt_test.rs

use botster_hub::PromptManager;

#[test]
fn test_prompt_generation() {
    let temp_dir = TempDir::new().unwrap();
    let manager = PromptManager::new(temp_dir.path().to_path_buf());

    let prompt = manager.generate_system_prompt().unwrap();
    assert!(prompt.contains("You are an AI assistant"));
}
```

---

## Running Tests in CI

### GitHub Actions Example

```yaml
name: Rust Tests

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      - name: Run tests
        run: cargo test --verbose
      - name: Run clippy
        run: cargo clippy -- -D warnings
      - name: Check formatting
        run: cargo fmt -- --check
```

---

## Summary

### Test Strategy for botster-hub

1. **Unit Tests** (`#[cfg(test)]` in source files):
   - Pure functions
   - Utility methods
   - Data structure methods

2. **Integration Tests** (`tests/` directory):
   - Agent lifecycle
   - Git operations
   - Configuration management
   - CLI commands

3. **Components to Test**:
   - ✅ Agent creation and session keys
   - ✅ Worktree management
   - ⚠️ Config get/set operations
   - ⚠️ Prompt generation
   - ⚠️ Terminal spawning
   - ⚠️ CLI command handlers
   - ⚠️ JSON manipulation utilities

4. **Testing Tools**:
   - `cargo test` - Built-in test runner
   - `tempfile` - Temporary directories
   - `assert_cmd` - CLI testing (recommended to add)
   - `mockito` - HTTP mocking (if needed)

---

**Next Steps:**

1. Add more integration tests for CLI commands
2. Test error handling paths
3. Add tests for concurrent operations
4. Measure code coverage with `cargo-tarpaulin`
