# Rust Testing Quick Reference

## Run Tests

```bash
# All tests
cargo test

# Specific test
cargo test test_name

# With output
cargo test -- --nocapture

# Sequential (for git tests)
cargo test -- --test-threads=1

# Integration test file
cargo test --test agent_test
```

## Write Tests

### Unit Test (in src/file.rs)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_something() {
        let result = my_function(42);
        assert_eq!(result, "expected");
    }
}
```

### Integration Test (tests/file.rs)

```rust
use botster::Agent;
use tempfile::TempDir;

#[test]
fn test_component() {
    let temp_dir = TempDir::new().unwrap();
    // Test code
}
```

## Best Practices

✅ **DO:**
- Use `tempfile::TempDir` for file system tests
- Use descriptive test names
- Test error cases
- Clean up resources
- Run git tests sequentially: `-- --test-threads=1`

❌ **DON'T:**
- Test private implementation details
- Leave test files/directories around
- Forget to test error paths
- Use hardcoded paths

## Current Test Status

- ✅ Agent session keys
- ✅ Worktree creation
- ✅ Agent spawning
- ⚠️ Need: CLI command tests
- ⚠️ Need: Config module tests
- ⚠️ Need: Prompt generation tests
- ⚠️ Need: Error handling tests

## Adding assert_cmd (Recommended)

```toml
# Add to Cargo.toml [dev-dependencies]
assert_cmd = "2.0"
predicates = "3.0"
```

```rust
// tests/cli_test.rs
use assert_cmd::Command;

#[test]
fn test_status_command() {
    Command::cargo_bin("botster").unwrap()
        .arg("status")
        .assert()
        .success();
}
```
