# Botster

Local-first PTY workspace platform for agents, accessories, Lua automation, and encrypted clients.

## Architecture

```
Client / plugin command → Rust daemon
                              ↓
              Hub coordinates control-plane policy
                              ↓
       Lua plugins create workspace/worktree/session intent
                              ↓
 SessionIo/ClientWorker stream terminal state to clients
```

**Rails server** (trybotster.com): Hub registration, user auth, template catalog, plugin event channels, and encrypted relay/signaling it cannot decrypt.

**Rust daemon** (botster): Hub control plane/orchestrator, Lua runtime, PTY/session infrastructure, and equal client transports for TUI, browser via WebRTC (E2E encrypted), and socket clients.

**Lua plugin system** (Neovim-inspired): Hot-reloadable plugins, ~20 Rust primitives exposed to Lua.

Provider-specific agent support lives in plugins/templates, not the core product boundary.

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
