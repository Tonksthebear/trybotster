# Botster

**Your agents. Your machine. From anywhere.**

Run autonomous AI agents on your machine. Monitor them from any device through P2P encrypted connections the server can never read.

## Features

- **Zero-Knowledge Architecture** — Keys exchanged offline via QR code. The server relays encrypted blobs it can never read.
- **Multi-Agent Management** — Run 20+ agents simultaneously, each in isolated git worktrees. Manage from the TUI or your browser.
- **Plugin System** — Extensible architecture. GitHub is a first-party plugin — `@botster` triggers agents automatically.
- **Local-First Execution** — Agents run on your hardware. Keys live in your OS keychain. No cloud execution, no vendor lock-in.
- **Agent Accessories** — Configure accessory processes per agent with port forwarding and web app previewing over encrypted WebRTC.

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
botster update       # Download and install latest
botster update-check # Check without installing
```

## Getting Started

### 1. Pair your device

```bash
botster start

# CLI will display:
#   To authorize this device, visit: https://trybotster.com/users/hubs/new
#   And enter code: ABCD-1234
#   [QR code displayed]
#
# Scan QR or visit URL, enter the code, and approve.
# Token is saved securely in your OS keychain.
```

### 2. Assign work

Create agents from any browser at [trybotster.com/hubs](https://trybotster.com/hubs), from the TUI, or by mentioning `@botster` in a GitHub issue or PR:

```
@botster can you investigate this memory leak in the worker process?
```

The agent will create a git worktree, spawn Claude, investigate, and create a PR or comment with findings.

### 3. Monitor from anywhere

Open [trybotster.com/hubs](https://trybotster.com/hubs) in your browser and scan the QR code displayed in the TUI. Terminal output streams in real-time over E2E encrypted P2P connections.

## How It Works

```
GitHub Issue/PR Comment
  "@botster can you fix this bug?"
        |
  Rails webhook receives mention
        |
  Creates message in queue
        |
  Rust daemon polls and detects message
        |
  Creates git worktree
        |
  Spawns Claude agent in PTY
        |
  Agent investigates and fixes issue
        |
  Creates PR or comments on issue
        |
  Issue/PR closed -> automatic cleanup
```

## Architecture

```
GitHub webhook -> Rails server -> Message queue -> Rust daemon polls
                                                        |
                                                Creates worktree
                                                        |
                                                Spawns Claude in PTY
```

**Rails server** ([trybotster.com](https://trybotster.com)) — Receives GitHub webhooks, creates message records, provides MCP tools for agents, relays E2E encrypted data (cannot decrypt).

**Rust daemon** (botster) — Interactive TUI with ratatui, polls for messages, manages agent lifecycle in isolated git worktrees, spawns Claude in PTY, routes keyboard input, streams terminal over encrypted WebRTC.

## TUI Controls

```
Ctrl+P  - Open menu
Ctrl+J  - Next agent
Ctrl+K  - Previous agent
Ctrl+X  - Kill selected agent
Ctrl+Q  - Quit daemon
```

## Repository Setup

In each repository where you want to use Botster, create these files:

**`.botster_init`** — Runs when agent starts:

```bash
#!/bin/bash
"$BOTSTER_BIN" json-set ~/.claude.json "projects.$BOTSTER_WORKTREE_PATH.hasTrustDialogAccepted" "true"
claude mcp add trybotster --transport http https://trybotster.com --header "Authorization: Bearer $BOTSTER_TOKEN"
claude --permission-mode acceptEdits "$BOTSTER_PROMPT"
```

**`.botster_teardown`** — Runs before worktree deletion:

```bash
#!/bin/bash
"$BOTSTER_BIN" json-delete ~/.claude.json "projects.$BOTSTER_WORKTREE_PATH"
```

**`.botster_copy`** — Files to copy to each worktree:

```
.env
config/credentials/*.key
.bundle
mise.toml
```

**`.botster_server`** — Background dev server for tunnel preview (optional):

```bash
#!/bin/bash
PORT=$BOTSTER_TUNNEL_PORT bin/dev
```

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

## MCP Tools

Agents have access to GitHub operations via the trybotster MCP server:

- `github_get_issue` / `github_get_pull_request` — Get issue/PR details
- `github_list_issues` / `github_list_repos` — List issues and repos
- `github_create_pull_request` — Create a PR
- `github_update_issue` — Update issue status/labels
- `github_comment_issue` — Comment on issue/PR

All operations use the GitHub App, showing as `@trybotster[bot]` on GitHub.

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

MIT License
