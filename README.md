# Botster

**Your agents. Your machine. From anywhere.**

Run autonomous AI agents on your machine. Monitor them from any device through P2P encrypted connections the server can never read.

## Features

- **Zero-Knowledge Architecture** — Keys exchanged offline via QR code. The server relays encrypted blobs it can never read.
- **Multi-Agent Management** — Run 20+ agents simultaneously, each in isolated git worktrees. Manage from the TUI or your browser.
- **Plugin System** — Extensible Lua plugin architecture with a template catalog for one-click install from the browser.
- **Local-First Execution** — Agents run on your hardware. Keys live in your OS keychain. No cloud execution, no vendor lock-in.
- **Profiles & Sessions** — Configure multiple named profiles per repo or device. Each agent can run multiple sessions (e.g., agent + dev server) with optional port forwarding over encrypted WebRTC.

## Privacy Model

The server sees nothing. Privacy by architecture, not policy.

- **Offline key exchange** — Encryption keys shared via QR code. They never touch the server.
- **Encrypted signaling** — The server only negotiates handshakes, and even those are encrypted blobs it relays without any ability to read.
- **P2P data channel** — After handshake, connections are direct P2P. Data is encrypted with [vodozemac](https://matrix.org/blog/2022/05/16/independent-public-audit-of-vodozemac-a-native-rust-reference-implementation-of-matrix-end-to-end-encryption/) (the Matrix Foundation's audited Olm implementation) on top of WebRTC encryption.
- **No trust required** — The server *cannot* decrypt your data, even if compromised.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Tonksthebear/trybotster/main/install.sh | sh
```

Detects your platform, downloads the latest release, verifies the checksum, and installs to `/usr/local/bin`.

### Manual download

| Platform | Binary |
|----------|--------|
| macOS (Apple Silicon) | `botster-macos-arm64` |
| macOS (Intel) | `botster-macos-x86_64` |
| Linux (x86_64) | `botster-linux-x86_64` |
| Linux (ARM64) | `botster-linux-arm64` |

Download from [GitHub Releases](https://github.com/Tonksthebear/trybotster/releases/latest), `chmod +x`, and move to a directory in your PATH.

### Build from source

Requires Rust toolchain:

```bash
git clone https://github.com/Tonksthebear/trybotster.git
cd trybotster/cli
cargo build --release
# Binary at: target/release/botster
```

### Update

The CLI checks for updates on launch. You can also update manually:

```bash
botster update         # Download and install latest
botster update --check # Check without installing
```

## Getting Started

### 1. Start the daemon

Run `botster start` from the root of the project you want agents to work in:

```bash
cd ~/projects/my-app
botster start
```

On first run, the CLI will prompt you to name your hub (defaults to the repo name) and walk you through device authorization:

```
  Setting up a new Botster hub.

  Name this hub (Enter for "my-app"):

  To authorize, visit:

    https://trybotster.com/hubs/codes/WDJB-MJHT

  Code: WDJB-MJHT

  Press Enter to open browser (or visit the URL above)...
```

Visit the URL, sign in with GitHub, and approve. Token is saved securely in your OS keychain.

### 2. Configure your repository

Set up a `.botster/` directory in your repo (see [Repository Setup](#repository-setup)) or use the browser Settings page to configure sessions and install templates.

### 3. Connect your browser

Open [trybotster.com/hubs](https://trybotster.com/hubs) and scan the QR code displayed in the TUI. This establishes the E2E encrypted P2P connection — the server never sees your data.

### 4. Create agents

Create agents from the browser or from the TUI menu (`Ctrl+P`). Terminal output streams in real-time over the encrypted connection.

## Architecture

```
Browser / TUI
      |
  Create agent (manual or plugin-triggered)
      |
  Rust daemon receives command
      |
  Creates git worktree
      |
  Spawns session PTYs (agent, server, etc.)
      |
  Agent works in isolation
      |
  Monitor via E2E encrypted WebRTC
```

**Rails server** ([trybotster.com](https://trybotster.com)) — User auth, hub management, template catalog, plugin event channels. Relays E2E encrypted data it cannot decrypt.

**Rust daemon** (botster) — Interactive TUI with ratatui, event-driven Lua runtime for agent lifecycle, creates isolated git worktrees, spawns sessions in PTYs, streams terminal over encrypted WebRTC.

**Lua runtime** — Hot-reloadable event-driven plugin system. Core handlers manage agent lifecycle, WebRTC signaling, hub commands, and TUI keybindings. User-extensible via plugins in `.botster/`.

## TUI Controls

The TUI has two primary modes:

**Normal mode** — Command mode, no PTY forwarding:

```
i           - Enter insert mode (forward keys to PTY)
Ctrl+P      - Open menu
Ctrl+J      - Next agent
Ctrl+K      - Previous agent
Ctrl+]      - Toggle PTY session
Ctrl+R      - Refresh agents
Ctrl+Q      - Quit daemon
```

**Insert mode** — Keys forward to the active PTY. Modifier combos still work:

```
Ctrl+P          - Open menu
Ctrl+J / Ctrl+K - Switch agents
Ctrl+]          - Toggle PTY session
Shift+PageUp    - Scroll half page up
Shift+PageDown  - Scroll half page down
Shift+Home      - Scroll to top
Shift+End       - Scroll to bottom
Ctrl+Q          - Quit daemon
```

## Repository Setup

Each repository that uses Botster needs a `.botster/` configuration directory. You can set this up manually or use the browser Settings page (Settings > Config tab) to create and edit files over E2E encrypted connections. Session and plugin templates are available for one-click install from the Settings > Templates tab.

### Directory structure

```
.botster/
  shared/                          # merged into EVERY profile
    workspace_include              # glob patterns for files to copy into worktrees
    workspace_teardown             # script run before worktree deletion
    sessions/
      agent/                       # REQUIRED — the primary agent session
        initialization             # startup script
    plugins/
      {name}/init.lua              # Lua plugins
  profiles/
    {profile-name}/                # named profile (e.g., "web", "api")
      sessions/
        {session-name}/
          initialization           # startup script for this session
          port_forward             # sentinel file — session gets $PORT
      plugins/
        {name}/init.lua
```

### Config resolution

Configuration is resolved across 4 layers (most specific wins):

1. **Device shared** (`~/.botster/shared/`) — Global defaults for all repos
2. **Device profile** (`~/.botster/profiles/{profile}/`) — Global profile overrides
3. **Repo shared** (`{repo}/.botster/shared/`) — Repo-specific defaults
4. **Repo profile** (`{repo}/.botster/profiles/{profile}/`) — Repo + profile overrides

Profile files win on collision. The `agent` session is required (in shared or profile) and always runs first.

### Example: `shared/workspace_include`

```
# Glob patterns for files to copy into worktrees
config/credentials/*.key
.claude/settings.local.json
mise.toml
```

### Example: `shared/workspace_teardown`

```bash
# Remove the worktree from Claude's trusted projects
"$BOTSTER_BIN" json-delete ~/.claude.json "projects.$BOTSTER_WORKTREE_PATH"
```

### Example: profile with dev server

A profile called `web` that adds a dev server session with port forwarding:

```
.botster/profiles/web/sessions/server/initialization   # contains: bin/dev
.botster/profiles/web/sessions/server/port_forward      # empty sentinel file
```

The `port_forward` sentinel file tells Botster to assign a `$PORT` and tunnel it over encrypted WebRTC for browser preview.

## Plugins

Plugins are Lua scripts that extend Botster's behavior. They live in `.botster/{shared,profiles}/plugins/{name}/init.lua` and are resolved across all 4 config layers like sessions.

### Installing plugins

The easiest way to install plugins is from the browser: go to your hub's **Settings > Templates** tab, which shows a catalog of available plugins. One click installs to either device or repo scope.

You can also create plugins manually by adding an `init.lua` to the appropriate plugins directory.

### GitHub plugin

The GitHub plugin subscribes to webhook events for your repository and automatically creates agents when `@botster` is mentioned in issues or PRs. Install it from the Templates catalog.

Once installed, mentioning `@botster` in a GitHub issue or PR will:

1. Create a git worktree for the issue
2. Spawn an agent with the mention context
3. Agent investigates and creates a PR or comments with findings
4. Issue/PR closed triggers automatic cleanup

The plugin also:

- Fetches a scoped MCP token so agents can use GitHub tools (showing as `@trybotster[bot]`)
- Routes new mentions to existing agents instead of spawning duplicates
- Posts notifications back to GitHub when agents ask questions

### Writing custom plugins

Plugins have access to the full Lua runtime API: `events`, `hooks`, `action_cable`, `http`, `json`, `secrets`, `log`, and more. See the GitHub plugin source for a comprehensive example.

## Configuration

**Environment variables:**

| Variable | Default | Description |
|----------|---------|-------------|
| `BOTSTER_SERVER_URL` | `https://trybotster.com` | Rails backend URL |
| `BOTSTER_WORKTREE_BASE` | `~/botster-sessions` | Where to create worktrees |
| `BOTSTER_POLL_INTERVAL` | `5` | Seconds between polls |
| `BOTSTER_MAX_SESSIONS` | `20` | Max concurrent agents |
| `BOTSTER_AGENT_TIMEOUT` | `3600` | Agent timeout in seconds |
| `BOTSTER_TOKEN` | — | Skip device flow (for CI/CD) |

**Supported terminals:** Ghostty (recommended), iTerm2, or any terminal supporting OSC 9 notifications. macOS Terminal.app does not support agent notifications.

## Agent Environment Variables

Each spawned agent has access to:

```bash
BOTSTER_REPO=owner/repo
BOTSTER_ISSUE_NUMBER=123
BOTSTER_BRANCH_NAME=botster-issue-123
BOTSTER_WORKTREE_PATH=/path/to/worktree
BOTSTER_PROMPT="User's request text"
BOTSTER_MESSAGE_ID=42
BOTSTER_BIN=/path/to/botster
BOTSTER_TOKEN=your_api_key
BOTSTER_TUNNEL_PORT=4001
```

Additional env vars may be injected by plugins (e.g., the GitHub plugin adds `BOTSTER_MCP_TOKEN` and `BOTSTER_MCP_URL`).

## Templates

The Settings > Templates tab in the browser provides a catalog of installable templates:

- **Sessions** — Pre-configured session initialization scripts (e.g., Claude agent session)
- **Plugins** — Lua plugins (e.g., GitHub integration)
- **Initialization** — User init.lua for custom hooks and commands

Templates can be installed to either device scope (`~/.botster/`) or repo scope (`{repo}/.botster/`) and are transferred over E2E encrypted connections.

## Testing

**Rust CLI:** Always use the test script:

```bash
cd cli
./test.sh              # Run all tests
./test.sh --unit       # Unit tests only
./test.sh -- scroll    # Tests matching 'scroll'
```

**Rails:** `rails test` or `rspec`.

## Contributing

Contributions welcome! See the [GitHub repository](https://github.com/Tonksthebear/trybotster).

## License

[O'Saasy License](LICENSE) — Free to use, modify, and distribute. Cannot be repackaged as a competing hosted/SaaS product.
