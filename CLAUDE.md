# Botster

GitHub mention → autonomous Claude agent in isolated worktree.

## Architecture

```
GitHub webhook → Rails server → Message queue → Rust daemon polls
                                                      ↓
                                              Creates worktree
                                                      ↓
                                              Spawns Claude in PTY
```

**Rails server** (trybotster.com):
- Receives GitHub webhooks, creates `Bot::Message` records
- VPN coordination via `WireguardCoordinator` (key exchange, IP allocation)
- MCP tools for agents (GitHub operations)
- User auth via GitHub OAuth

**Rust daemon** (botster-hub):
- TUI with ratatui
- Polls Rails for messages, manages agent lifecycle
- Creates/deletes git worktrees per issue
- Spawns Claude in PTY, routes keyboard input
- WireGuard VPN client (`cli/src/wireguard.rs`)

## Key Paths

```
# Rails
app/models/bot/message.rb              # Message queue
app/models/vpn_node.rb                 # VPN node records
app/services/wireguard_coordinator.rb  # VPN key exchange
app/controllers/github/webhooks_controller.rb

# Rust CLI
cli/src/main.rs      # TUI, daemon logic
cli/src/agent.rs     # Agent PTY management
cli/src/wireguard.rs # WireGuard VPN client
cli/src/git.rs       # Worktree operations
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
