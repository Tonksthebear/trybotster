# Botster

GitHub mention → autonomous Claude agent in isolated worktree.

## Architecture

```
GitHub webhook → Rails server → Message queue → Rust daemon polls
                                                      ↓
                                              Lua plugin handles message
                                                      ↓
                                              Creates worktree, spawns Claude in PTY
```

**Rails server** (trybotster.com): Webhooks, hub registration, MCP tools, user auth via GitHub OAuth.

**Rust daemon** (botster): TUI, browser client via WebRTC (E2E encrypted), Lua plugin system, PTY infrastructure, worktree management.

**Lua plugin system** (Neovim-inspired): Hot-reloadable plugins, ~20 Rust primitives exposed to Lua.

## Running Tests

**Rust CLI:** Always use the test script, never `cargo test` directly:

```bash
cd cli
./test.sh              # Run all tests
./test.sh --unit       # Unit tests only
./test.sh -- scroll    # Tests matching 'scroll'
```

This ensures `BOTSTER_ENV=test` is set, preventing macOS keyring prompts.

**Rails:** Standard `rails test` or `rspec`.

## Patterns

See `.claude/skills/rails-backend-guidelines/` - fat models, no service objects, POROs.

**NEVER PRECOMPILE ASSETS IN RAILS**
