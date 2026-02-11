# Testing Infrastructure Analysis

*Analysis performed 2026-01-11*

## Current State

### Rails Testing (`test/`)

- **Framework**: Minitest with parallel execution
- **~28 test files**, ~3,883 lines
- **Coverage**: Models (7), Controllers (2), Channels (3), System (1), Integration (1)

**Key Infrastructure:**
- `test/support/cli_test_helper.rb` (294 lines) - Spawns real CLI binaries for system tests
- WebMock enabled, fixtures for GitHub webhook payloads
- Capybara + Selenium for browser automation

**Notable Tests:**
- `test/system/terminal_relay_test.rb` - Full E2E: real browser + real CLI + Signal Protocol
- `test/controllers/github/webhooks_controller_test.rb` - Webhook signature validation, message routing
- `test/channels/terminal_channel_test.rb` - Action Cable relay, encryption envelope format

### Rust CLI Testing (`cli/tests/`)

- **Framework**: cargo test (single-threaded in CI to avoid env var races)
- **15+ integration tests**, ~4,383 lines
- **Dev deps**: `tempfile`, `wiremock`, `libc`, `portable-pty`

**Notable Tests:**
- `integration_tests.rs` - Deadlock prevention, real PTY spawning
- `signal_protocol_test.rs` - Message format/envelope validation
- `worktree_*_test.rs` - Git worktree regression tests

**Pattern**: Timeout-based deadlock detection, real shell process spawning

### CI Pipeline (`.github/workflows/ci.yml`)

| Job | Purpose |
|-----|---------|
| `test` | Rails tests with postgres |
| `system-test` | E2E with screenshot artifacts |
| `cli-test` | Rust tests (single-threaded) |
| `scan_ruby` | Brakeman, bundler-audit |
| `lint` | RuboCop |

---

## Strengths

- **Real E2E exists**: System tests spawn actual CLI binary, connect via Action Cable
- **Signal Protocol format tests**: JSON schema validation ensures Rails ↔ CLI compatibility
- **Deadlock testing**: Rust tests use channels + timeouts to catch concurrency bugs
- **Bug regression tests**: Specific tests for known issues (worktree, deadlocks)
- **CliTestHelper**: Sophisticated test harness with `BOTSTER_ENV=test` for unified test mode

---

## Gaps

### High Priority

| Gap | Impact |
|-----|--------|
| **API Endpoint Tests** | Only 2 controller tests. Hub CRUD, device auth, VPN registration untested |
| **CLI → Rails API** | No tests verifying CLI HTTP calls to Rails endpoints |
| **Message Encryption E2E** | System tests verify connection but don't decrypt/verify actual payload |
| **VPN/WireGuard** | Zero test coverage despite being in architecture |

### Medium Priority

- Error recovery (network failures, retries, partial states)
- MCP tools coverage (only 1 test file)
- Background job tests (Solid Queue)
- Request/route tests

### Nice to Have

- Performance benchmarks
- Load tests for concurrent connections
- Rust coverage reporting (`tarpaulin` or `llvm-cov`)
- Snapshot/approval testing

---

## Architecture Gap

The testing has two modes:
1. **Unit tests** - Mock everything
2. **System tests** - Spin up the whole world (browser + CLI + Rails)

**Missing middle layer**: Integration tests that verify Rails ↔ CLI API contract without needing browsers/PTYs.

---

## Recommended Next Steps

1. **API contract tests** - Request specs verifying JSON shape Rails returns matches CLI expectations
2. **CLI mock server tests** - Use `wiremock` to test CLI against fake Rails endpoints
3. **Message roundtrip verification** - Extend system tests to decrypt and assert on message content
4. **VPN integration tests** - Test WireGuard key exchange and connectivity

---

## Key File Locations

```
# Rails test infrastructure
test/test_helper.rb
test/support/cli_test_helper.rb
test/system/terminal_relay_test.rb
test/controllers/github/webhooks_controller_test.rb

# Rust test infrastructure
cli/tests/integration_tests.rs
cli/tests/signal_protocol_test.rs
cli/Cargo.toml (dev-dependencies)

# CI
.github/workflows/ci.yml
```
