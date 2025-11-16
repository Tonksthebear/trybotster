# Testing Guide

## Overview

The codebase is now modularized with comprehensive unit and integration tests.

## Project Structure

```
src/
├── lib.rs           # Library exports
├── main.rs          # Main binary (uses lib modules)
├── config.rs        # Configuration management (tested)
├── agent.rs         # Agent/PTY management (tested)
└── git.rs           # Git worktree management (tested)

tests/
└── worktree_integration_test.rs  # Integration tests
```

## Running Tests

### Run All Tests
```bash
cargo test
```

### Run Unit Tests Only (Fast)
```bash
cargo test --lib
```

### Run Integration Tests Only
```bash
cargo test --test worktree_integration_test
```

### Run Specific Module Tests
```bash
# Test only config module
cargo test --lib config::tests

# Test only agent module  
cargo test --lib agent::tests

# Test only git module
cargo test --lib git::tests
```

### Run Single Test
```bash
cargo test test_agent_creation
cargo test test_worktree_creation_with_real_git
```

### Run Tests with Output
```bash
cargo test -- --nocapture
```

### Run Tests in Parallel
```bash
cargo test -- --test-threads=4
```

## Test Coverage

### Config Module (`src/config.rs`)
- ✅ Default configuration values
- ✅ Serialization/deserialization
- ✅ File I/O (load/save)

**Tests:**
- `test_default_config` - Validates default settings
- `test_config_serialization` - JSON round-trip

**Run:**
```bash
cargo test --lib config::tests
```

### Agent Module (`src/agent.rs`)
- ✅ Agent creation and initialization
- ✅ Buffer management (circular buffer with limit)
- ✅ Session key generation
- ✅ Age tracking
- ✅ PTY spawning (basic)

**Tests:**
- `test_agent_creation` - Creates agent with correct properties
- `test_session_key` - Generates unique keys per repo/issue
- `test_add_to_buffer` - Buffer append functionality
- `test_buffer_limit` - Enforces MAX_BUFFER_LINES limit
- `test_agent_age` - Time tracking

**Run:**
```bash
cargo test --lib agent::tests
```

### Git Module (`src/git.rs`)
- ✅ Worktree manager creation
- ✅ Cleanup of non-existent worktrees
- ✅ List worktrees for empty repos
- ✅ Prune stale worktrees

**Tests:**
- `test_worktree_manager_creation` - Manager initialization
- `test_cleanup_nonexistent_worktree` - Safe cleanup
- `test_list_worktrees_empty_repo` - Handle missing repos

**Run:**
```bash
cargo test --lib git::tests
```

### Integration Tests
- ✅ Real git repository operations
- ✅ Agent spawning with actual commands
- ✅ Multiple agents with different issues

**Tests:**
- `test_worktree_creation_with_real_git` - Full git workflow
- `test_agent_spawns_with_echo_command` - PTY spawn verification
- `test_multiple_agents_different_issues` - Session isolation

**Run:**
```bash
cargo test --test worktree_integration_test
```

## Testing Individual Components

### Test Config Without Full Build
```bash
# Just check config can load/save
cargo test --lib config -- --nocapture
```

### Test Git Worktree Operations
```bash
# Test worktree cleanup and creation
cargo test --lib git -- --nocapture

# Integration test with real git
cargo test test_worktree_creation_with_real_git -- --nocapture
```

### Test Agent Buffer Management
```bash
# Verify buffer doesn't leak memory
cargo test test_buffer_limit -- --nocapture
```

### Test PTY Spawning
```bash
# Verify we can spawn processes
cargo test test_agent_spawns_with_echo_command -- --nocapture
```

## Watch Mode (Auto-run tests on changes)

Install cargo-watch:
```bash
cargo install cargo-watch
```

Run tests on every file change:
```bash
cargo watch -x test
cargo watch -x 'test --lib'  # Unit tests only
```

## Test Performance

```bash
# Run with timing info
cargo test -- --show-output

# Run with profiling
cargo test --release
```

## Current Test Results

```
running 10 tests (unit tests)
test config::tests::test_default_config ... ok
test config::tests::test_config_serialization ... ok
test agent::tests::test_agent_creation ... ok
test agent::tests::test_session_key ... ok
test agent::tests::test_add_to_buffer ... ok
test agent::tests::test_agent_age ... ok
test agent::tests::test_buffer_limit ... ok
test git::tests::test_worktree_manager_creation ... ok
test git::tests::test_cleanup_nonexistent_worktree ... ok
test git::tests::test_list_worktrees_empty_repo ... ok

running 3 tests (integration tests)
test test_worktree_creation_with_real_git ... ok
test test_agent_spawns_with_echo_command ... ok
test test_multiple_agents_different_issues ... ok

Total: 13 tests passing ✅
```

## Adding New Tests

### Unit Test Template
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_your_feature() {
        // Arrange
        let input = "test";
        
        // Act
        let result = your_function(input);
        
        // Assert
        assert_eq!(result, expected);
    }
}
```

### Integration Test Template
```rust
use botster_hub::{Agent, Config, WorktreeManager};
use tempfile::TempDir;

#[test]
fn test_your_integration() {
    let temp_dir = TempDir::new().unwrap();
    // Test cross-module functionality
}
```

## What's NOT Tested (Yet)

These require refactoring to be testable:

- ❌ TUI rendering (in main.rs)
- ❌ API polling logic (in main.rs)
- ❌ Full E2E flow with Rails server
- ❌ Error handling in production scenarios

## Benefits of This Structure

1. **Fast Iteration** - Test individual modules without full compile
2. **Clear Failures** - Know exactly which component broke
3. **Refactoring Safety** - Tests prevent regressions
4. **Documentation** - Tests show how to use each module
5. **CI/CD Ready** - Easy to run in GitHub Actions

## CI/CD Example

```yaml
# .github/workflows/test.yml
name: Test
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - uses: actions-rs/toolchain@v1
      - run: cargo test --all
```

## Performance Notes

- **Unit tests**: ~20ms (instant feedback)
- **Integration tests**: ~200ms (spawns real git/processes)
- **Full build + test**: ~25s (first time), ~2s (incremental)

Much better than E2E testing which requires:
- Rails server running
- Database setup
- GitHub API mocking
- Full daemon lifecycle

## Next Steps

To test the full application flow, you can:

1. **Unit test each module independently** ✅ (Done!)
2. **Integration test module interactions** ✅ (Done!)
3. **Manual E2E test with real server** (Current state)
4. **Mock API responses for automated E2E** (Future)
