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
- Hub registration (one hub = one device, identified by Ed25519 fingerprint)
- `HubToken` for CLI auth, `BrowserKey` for browser E2E key exchange
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
app/models/hub.rb                          # Hub = device identity (fingerprint, tokens)
app/models/hub_token.rb                    # CLI auth token (btstr_ prefix)
app/models/browser_key.rb                  # Browser E2E public key registration
app/models/hub_authorization.rb            # OAuth device flow (RFC 8628)
app/models/integrations/github/message.rb  # GitHub webhook messages
app/models/hub_command.rb                  # Hub platform commands
app/controllers/github/webhooks_controller.rb
app/templates/plugins/github.lua           # GitHub plugin template

# Rust CLI
cli/src/main.rs             # TUI, daemon logic
cli/src/agent/mod.rs        # Agent PTY management (Rust struct)
cli/src/hub/mod.rs          # Hub orchestrator
cli/src/hub/handle_cache.rs # Thread-safe agent PTY handle cache
cli/src/session/mod.rs      # Per-session PTY process (replaces broker)
cli/src/session/protocol.rs # Session process wire protocol
cli/src/session/connection.rs # Hub-side session connection + reader thread
cli/src/device.rs           # Local device identity (Ed25519 keypair, fingerprint)
cli/src/relay/              # E2E encrypted browser communication
cli/src/wireguard.rs        # WireGuard VPN client
cli/src/git.rs              # Worktree operations

# Lua (agent lifecycle + TUI)
cli/lua/lib/agent.lua          # Agent class, metadata store
cli/lua/handlers/agents.lua    # Agent creation, matching, lifecycle
cli/lua/handlers/session_recovery.lua # Session recovery on hub restart
cli/lua/handlers/commands.lua  # Hub command dispatch
cli/lua/ui/layout.lua          # TUI layout composition
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

## Per-Session Process Architecture

Each PTY gets its own process (`botster session`) with its own Unix socket.
No broker, no multiplexing, no demux thread.

```
Hub spawns session process → session creates PTY + binds socket
Hub connects to socket → handshake → sends spawn config
Session reader thread → broadcasts PtyEvent::Output + structured events
Hub routes events to clients → snapshots via RPC to session process
```

**Session process owns:** PTY fd, ghostty parser (state tracking + 6 vendored callbacks), socket, tee/logging, resize, dual-screen snapshot generation, event frame emission
**Hub owns:** client routing, event broadcasting, snapshot RPC dispatch
**Socket-as-lease:** session process exits if its socket file is deleted
**setsid:** session processes survive hub restart (own process group)
**Recovery:** hub scans `/tmp/botster-{uid}/sessions/*.sock`, connects to live ones

Key paths:
- `cli/src/session/mod.rs` — session process entry point
- `cli/src/session/connection.rs` — hub-side connection + reader thread
- `cli/src/session/protocol.rs` — wire protocol (22 frame types incl. structured events 0x10-0x15)
- `cli/lua/handlers/session_recovery.lua` — recovery handler

## Patterns

See `.claude/skills/rails-backend-guidelines/` - fat models, no service objects, POROs.

**NEVER PRECOMPILE ASSETS IN RAILS**
