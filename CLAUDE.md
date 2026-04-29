# Botster

Local-first PTY workspace platform for agents, accessories, Lua automation, and encrypted clients.

## Architecture

```
Client / plugin command → Rust daemon
                              ↓
                      Lua runtime handles lifecycle
                              ↓
              Creates workspace/worktree, spawns session PTYs
                              ↓
              Streams terminal state to connected clients
```

**Rails server** (trybotster.com): Hub registration, user auth, template catalog, plugin event channels, and encrypted relay/signaling it cannot decrypt.

**Rust daemon** (botster): TUI, web client transport via WebRTC (E2E encrypted), Lua plugin system, PTY infrastructure, worktree/workspace management.

**Lua plugin system** (Neovim-inspired): Hot-reloadable plugins, ~20 Rust primitives exposed to Lua.

GitHub and Claude support lives in plugins/templates, not the core product boundary.

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

Rails follows the repo conventions: fat models, concerns/callbacks where useful, POROs over service-object sprawl, and minimal gems.

**NEVER PRECOMPILE ASSETS IN RAILS**
