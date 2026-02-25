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

**Rails server** (trybotster.com):

- Receives GitHub webhooks, creates `Integrations::Github::Message` records
- VPN coordination via `WireguardCoordinator` (key exchange, IP allocation)
- MCP tools for agents (GitHub operations)
- User auth via GitHub OAuth

**Rust daemon** (botster):

- TUI with ratatui, browser client via WebRTC (E2E encrypted)
- Lua plugin system manages agent lifecycle, TUI layout, and hub commands
- Rust provides PTY infrastructure, crypto, transport primitives
- Creates/deletes git worktrees per branch
- WireGuard VPN client (`cli/src/wireguard.rs`)

**Lua plugin system** (Neovim-inspired):

- `cli/lua/lib/agent.lua` — Agent class with generic `metadata` key-value store
- `cli/lua/handlers/` — Event handlers (agents, commands, connections, templates)
- `app/templates/plugins/` — Plugin templates (e.g., GitHub integration)
- Hot-reloadable, ~20 Rust primitives exposed to Lua

## Key Paths

```
# Rails
app/models/integrations/github/message.rb  # GitHub webhook messages
app/models/hub_command.rb                  # Hub platform commands
app/models/vpn_node.rb                     # VPN node records
app/services/wireguard_coordinator.rb      # VPN key exchange
app/controllers/github/webhooks_controller.rb
app/templates/plugins/github.lua           # GitHub plugin template

# Rust CLI
cli/src/main.rs             # TUI, daemon logic
cli/src/agent/mod.rs        # Agent PTY management (Rust struct)
cli/src/hub/mod.rs          # Hub orchestrator
cli/src/hub/handle_cache.rs # Thread-safe agent PTY handle cache
cli/src/relay/              # E2E encrypted browser communication
cli/src/wireguard.rs        # WireGuard VPN client
cli/src/git.rs              # Worktree operations

# Lua (agent lifecycle + TUI)
cli/lua/lib/agent.lua          # Agent class, metadata store, context.json
cli/lua/handlers/agents.lua    # Agent creation, matching, lifecycle
cli/lua/handlers/commands.lua  # Hub command dispatch
cli/lua/ui/layout.lua          # TUI layout composition
```

## PTY Communication

### Writing to PTY Sessions

Two paths exist for writing to agent PTYs:

**Direct input** (human keystrokes): Clients write raw bytes via `PtyHandle::write_input_direct()`. This stamps `last_human_input_ms` on `SharedPtyState` for activity tracking. Focus events (`\x1b[I`/`\x1b[O`) are filtered out — they don't count as human activity.

**Message delivery** (programmatic): Lua calls `session:send_message(text)` to inject text into a PTY. This uses a probe-based delivery gate in `cli/src/agent/message_delivery.rs`:

```
Lua send_message("fix the bug")
  → MessageDeliveryState.enqueue()
  → Delivery task wakes up
  → Checks human activity cooldown (2s)
  → Injects "zx" probe into PTY
  → Watches for echo in PTY output (200ms timeout)
    → Echo detected: PTY accepts free-text input
      → Erase probe (backspaces), inject message + Enter
    → No echo: PTY is in modal state (permission prompt, etc.)
      → Wait for next output event, retry
```

The delivery task selects the correct Enter key based on the kitty keyboard protocol state: `\r` (legacy) or `\x1b[13u` (kitty). It runs as a tokio task per PTY session, spawned lazily on first `send_message()` call.

### Key Files

```
cli/src/agent/message_delivery.rs  # Probe-based message delivery system
cli/src/agent/pty/mod.rs           # SharedPtyState (last_human_input_ms)
cli/src/hub/agent_handle.rs        # PtyHandle, write_input_direct()
cli/src/lua/primitives/pty.rs      # PtySessionHandle (Lua send_message binding)
cli/src/hub/events.rs              # HubEvent::MessageDelivered
```

### Lua API

```lua
-- Send a message to an agent's PTY (queued, probe-gated)
session:send_message("your text here")

-- Direct write (immediate, no probe gate)
session:write("raw bytes\n")
```

## VPN Architecture

WireGuard VPN replaces WebSocket tunnels:

1. CLI generates WireGuard keypair locally
2. Registers with Rails (`POST /api/vpn/register`)
3. Rails allocates VPN IP (10.100.x.x), returns server config
4. CLI configures WireGuard interface
5. Direct connectivity via VPN

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
