# Trybotster

**GitHub Mention → Autonomous AI Agent**

When someone mentions `@trybotster` in a GitHub issue or PR, an autonomous AI agent spawns in an isolated git worktree to investigate and resolve the issue.

## 🎯 What It Does

```
GitHub Issue/PR Comment
  "@trybotster can you fix this bug?"
        ↓
  Rails webhook receives mention
        ↓
  Creates message in queue
        ↓
  Rust daemon polls and detects message
        ↓
  Verifies user has repo access
        ↓
  Creates git worktree
        ↓
  Spawns Claude agent in PTY
        ↓
  Agent investigates and fixes issue
        ↓
  Creates PR or comments on issue
        ↓
  Issue/PR closed → automatic cleanup
```

### Key Features

- **🤖 Autonomous**: Agents work independently without human intervention
- **🔒 Local-first**: Your code never leaves your machine
- **⚡ Interactive TUI**: Real-time view of all running agents
- **🎨 Isolated Worktrees**: Each agent works in a separate git worktree
- **🧹 Auto-cleanup**: Closes agents and deletes worktrees when issues are closed
- **🔄 Smart Deduplication**: Multiple mentions to the same issue ping the existing agent
- **📡 MCP Integration**: Agents interact with GitHub via Model Context Protocol
- **🔐 WireGuard VPN**: Direct network access to agent dev servers
- **🖥️ Web GUI**: Remote control agents via P2P WebRTC connection

## 📦 Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                    GitHub (External)                          │
│  Someone mentions @trybotster in issue/PR                    │
└─────────────────────┬────────────────────────────────────────┘
                      │ Webhook
                      ↓
┌──────────────────────────────────────────────────────────────┐
│              Rails Server (Message Broker)                    │
│                                                               │
│  • Receives GitHub webhooks                                  │
│  • Creates Bot::Message records                              │
│  • Verifies repo access via GitHub API                       │
│  • Provides MCP tools for agents                             │
│  • WireGuard VPN coordination (key exchange, IP allocation)  │
│  • Auto-cleanup on issue/PR close                            │
│                                                               │
│  Event Types:                                                │
│  • github_mention - New @trybotster mention                  │
│  • agent_cleanup - Issue/PR closed, cleanup agent            │
└─────────────────────┬────────────────────────────────────────┘
                      │ HTTP Polling
                      ↓
┌──────────────────────────────────────────────────────────────┐
│               Rust Daemon (botster-hub)                       │
│                                                               │
│  • Interactive TUI (ratatui)                                 │
│  • Polls Rails API every 5 seconds                           │
│  • Manages agents in HashMap by session key                  │
│  • Creates/deletes git worktrees                             │
│  • Spawns Claude in PTY for each agent                       │
│  • Routes keyboard input to selected agent                   │
│  • Handles cleanup on issue/PR close                         │
│  • Pings existing agents on duplicate mentions               │
│                                                               │
│  Agent Sessions:                                             │
│  • Key: "repo-safe-issue_number"                            │
│  • Worktree: ~/botster-sessions/org-repo-123/              │
│  • Full VT100 terminal emulation                            │
│  • Environment variables for context                         │
└──────────────────────────────────────────────────────────────┘
```

## 🚀 Quick Start

### Prerequisites

**Server:**

- Ruby 3.3+
- PostgreSQL
- GitHub App (for webhooks and bot actions)

**Client:**

- Rust (for building the daemon)
- Claude Code CLI
- Git
- **Supported Terminals:**
  - Ghostty (recommended)
  - iTerm2
  - Other terminals that support OSC 9 notifications
  - Note: macOS Terminal.app does not support agent notifications

### 1. Server Setup

```bash
# Clone and install
git clone https://github.com/yourusername/trybotster.git
cd trybotster
bundle install

# Setup database
rails db:create db:migrate

# Configure GitHub App
# See "GitHub App Setup" section below
```

### 2. GitHub App Setup

1. **Create a GitHub App** at https://github.com/settings/apps/new

2. **Configure webhook:**
   - Webhook URL: `https://your-domain.com/github/webhooks`
   - Webhook secret: Generate a random string
   - Subscribe to events:
     - ✅ Issues (opened, edited, closed)
     - ✅ Pull requests (opened, edited, closed)
     - ✅ Issue comments
     - ✅ Pull request review comments

3. **Set permissions:**
   - Issues: Read & Write
   - Pull requests: Read & Write
   - Contents: Read & Write

4. **Set environment variables:**

```bash
GITHUB_APP_ID=your_app_id
GITHUB_APP_CLIENT_ID=your_client_id
GITHUB_APP_CLIENT_SECRET=your_client_secret
GITHUB_APP_PRIVATE_KEY="-----BEGIN RSA PRIVATE KEY-----\n..."
GITHUB_WEBHOOK_SECRET=your_webhook_secret
```

### 3. User Setup

```bash
# Start Rails server
rails server

# Visit http://localhost:3000 and login with GitHub
# Generate your API key in Rails console:

rails console
user = User.find_by(username: "your_github_username")
user.regenerate_api_key!
puts user.api_key  # Save this!
```

### 4. Daemon Setup

Build the daemon:

```bash
cd cli
cargo build --release
```

The binary will be at `target/release/botster-hub`.

Configure via environment variables:

```bash
export BOTSTER_API_KEY="your_api_key_from_step_3"
# Optional overrides:
# export BOTSTER_SERVER_URL="https://your-domain.com"  # default: https://trybotster.com
# export BOTSTER_WORKTREE_BASE="$HOME/my-worktrees"    # default: ~/botster-sessions
# export BOTSTER_POLL_INTERVAL="10"                    # default: 5 seconds
```

Start the daemon:

```bash
./target/release/botster-hub start
```

### 5. Repository Setup

In each repository where you want to use Trybotster, create these files:

**`.botster_init`** - Runs when agent starts:

```bash
#!/bin/bash
# Trust worktree in Claude config
"$BOTSTER_HUB_BIN" json-set ~/.claude.json "projects.$BOTSTER_WORKTREE_PATH.hasTrustDialogAccepted" "true"

# Register trybotster MCP server
claude mcp add trybotster --transport http https://trybotster.com --header "Authorization: Bearer $BOTSTER_TOKEN"

# Start Claude with prompt
claude --permission-mode acceptEdits "$BOTSTER_PROMPT"
```

**`.botster_teardown`** - Runs before worktree deletion:

```bash
#!/bin/bash
# Remove worktree from Claude's trusted projects
"$BOTSTER_HUB_BIN" json-delete ~/.claude.json "projects.$BOTSTER_WORKTREE_PATH"
```

**`.botster_server`** - Background dev server for tunnel preview (optional):

```bash
#!/bin/bash
# Runs when agent spawns with BOTSTER_TUNNEL_PORT set
# Customize for your project (Rails, Node, Python, etc.)

# For Rails with bin/dev (foreman/overmind):
PORT=$BOTSTER_TUNNEL_PORT bin/dev

# Or for Rails server only:
# bin/rails server -p "$BOTSTER_TUNNEL_PORT" -b 127.0.0.1

# Or for Node:
# npm run dev -- --port $BOTSTER_TUNNEL_PORT
```

**`.botster_copy`** - Files to copy to each worktree:

```
.env
config/credentials/*.key
.bundle
mise.toml
```

## 📖 Usage

### Mentioning the Bot

Simply mention `@trybotster` in any GitHub issue or PR comment:

```
@trybotster can you investigate this memory leak in the worker process?
```

The bot will:

1. Create a git worktree for that issue
2. Spawn a Claude agent
3. Investigate and work on the issue
4. Create a PR or comment with findings

### TUI Controls

When the daemon is running, you see an interactive TUI:

```
Ctrl+P  - Open menu
Ctrl+J  - Next agent
Ctrl+K  - Previous agent
Ctrl+X  - Kill selected agent
Ctrl+Q  - Quit daemon

Menu options:
  - Toggle Polling (pause/resume message polling)
  - New Agent (manually create agent)
  - Close Agent (close selected agent)
```

### Agent Lifecycle

**Creation:**

- Daemon detects new `github_mention` message
- Checks if agent already exists for that issue
- If exists: pings existing agent with new message
- If not: creates new agent in fresh worktree

**Running:**

- Agent appears in TUI with label like `owner/repo#123`
- Terminal output shown in right panel
- Keyboard input routed to selected agent

**Cleanup:**

- When issue/PR is closed, Rails sends `agent_cleanup` message
- Daemon kills agent, deletes worktree, runs teardown scripts
- Agent removed from TUI

### Environment Variables (in agents)

Each spawned agent has access to:

```bash
BOTSTER_REPO=owner/repo
BOTSTER_ISSUE_NUMBER=123
BOTSTER_BRANCH_NAME=botster-issue-123
BOTSTER_WORKTREE_PATH=/path/to/worktree
BOTSTER_PROMPT="User's request text"
BOTSTER_MESSAGE_ID=42
BOTSTER_HUB_BIN=/path/to/botster-hub
BOTSTER_TOKEN=your_api_key  # For MCP server auth
BOTSTER_TUNNEL_PORT=4001    # Port for HTTP tunnel (if available)
```

### WireGuard VPN

Agents connect via WireGuard VPN for direct network access to dev servers.

**How it works:**

1. CLI generates WireGuard keypair locally (stored in `~/.config/botster/wireguard.key`)
2. CLI registers with Rails (`POST /api/vpn/register`), sends public key
3. Rails allocates VPN IP (10.100.x.x), returns server config
4. CLI configures WireGuard interface (`botster0`)
5. Direct connectivity to agent dev servers via VPN

**Requirements:**

- **Linux:** WireGuard kernel module
- **macOS:** `wireguard-go` installed (`brew install wireguard-go`)

**VPN Status in TUI:** ⬤ connected, ◐ connecting, ○ disconnected

## 🛠️ Configuration

### Environment Variables

**Required:**

- `BOTSTER_API_KEY` - Your API key from Rails

**Optional (with defaults):**

- `BOTSTER_SERVER_URL` - Rails backend URL (default: `https://trybotster.com`)
- `BOTSTER_WORKTREE_BASE` - Where to create worktrees (default: `~/botster-sessions`)
- `BOTSTER_POLL_INTERVAL` - Seconds between polls (default: `5`)
- `BOTSTER_MAX_SESSIONS` - Max concurrent agents (default: `20`)
- `BOTSTER_AGENT_TIMEOUT` - Agent timeout in seconds (default: `3600`)

### Config File (Optional)

Create `~/.botster_hub/config.json` to set defaults:

```json
{
  "server_url": "https://trybotster.com",
  "api_key": "your_key_here",
  "poll_interval": 5,
  "agent_timeout": 3600,
  "max_sessions": 20,
  "worktree_base": "/Users/you/botster-sessions"
}
```

Environment variables override config file values.

## 🔧 MCP Tools

Agents have access to these MCP tools via the trybotster server:

### GitHub Operations

- **`github_get_issue`** - Get issue/PR details
- **`github_list_issues`** - List repository issues
- **`github_create_pull_request`** - Create a PR
- **`github_update_issue`** - Update issue status/labels
- **`github_comment_issue`** - Comment on issue/PR
- **`github_get_pull_request`** - Get PR details and diff
- **`github_list_repos`** - List accessible repositories

All operations use the GitHub App, showing as `@trybotster[bot]` on GitHub.

## 🏗️ Project Structure

```
trybotster/
├── app/
│   ├── models/
│   │   ├── bot/message.rb           # Message queue
│   │   ├── vpn_node.rb              # VPN node records
│   │   ├── github/app.rb            # GitHub API wrapper
│   │   └── user.rb                  # User auth
│   │
│   ├── services/
│   │   └── wireguard_coordinator.rb # VPN key exchange
│   │
│   ├── controllers/
│   │   ├── bots/messages_controller.rb     # API for daemon
│   │   └── github/webhooks_controller.rb   # Webhook receiver
│   │
│   └── mcp/tools/                   # MCP tool implementations
│
├── cli/                             # Rust daemon (CLI)
│   ├── src/
│   │   ├── main.rs                  # TUI and daemon logic
│   │   ├── agent.rs                 # Agent PTY management
│   │   ├── git.rs                   # Worktree operations
│   │   ├── config.rs                # Configuration
│   │   ├── wireguard.rs             # WireGuard VPN client
│   │   └── webrtc_handler.rs        # P2P WebRTC for web GUI
│   └── Cargo.toml
│
└── README.md                        # This file
```

## 🔒 Security

### Webhook Verification

GitHub webhooks are verified using HMAC-SHA256 signatures.

### API Authentication

Daemon authenticates to Rails using `X-API-Key` header.

### Repository Access

Users must have GitHub access to a repository to receive messages for it. The Rails server verifies access via GitHub API before delivering messages.

### Bot Attribution

All GitHub actions show as `@trybotster[bot]` using GitHub App installation tokens.

## 🧪 Testing

### Manual Test

1. Start Rails server: `rails server`
2. Start daemon: `./botster-hub start`
3. Mention `@trybotster` in a GitHub issue
4. Watch agent spawn in TUI
5. Close the issue on GitHub
6. Watch agent cleanup automatically

### Test Without GitHub

Create a test message in Rails console:

```ruby
user = User.find_by(username: "your_username")
Bot::Message.create!(
  event_type: "github_mention",
  payload: {
    repo: "owner/repo",
    issue_number: 999,
    comment_body: "@trybotster test this",
    comment_author: "testuser",
    issue_title: "Test Issue",
    issue_body: "Description",
    issue_url: "https://github.com/owner/repo/issues/999",
    is_pr: false,
    context: "Work on issue #999"
  }
)
```

## 🚧 Roadmap

- [x] Auto-cleanup on issue/PR close
- [x] Smart agent deduplication
- [x] Interactive TUI
- [x] WireGuard VPN (direct network access to dev servers)
- [x] Web GUI with P2P WebRTC
- [ ] Agent timeout handling
- [ ] Metrics and monitoring
- [ ] Multi-repo support in single daemon
- [ ] Linux support (X11/Wayland terminals)

## 🤝 Contributing

Contributions welcome! This project follows Rails conventions and uses Rust for the daemon.

## 📄 License

MIT License

---

**Questions?** Open an issue on GitHub.
