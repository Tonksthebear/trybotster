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
- **🔐 E2E Encrypted**: Signal Protocol encryption - server cannot read terminal content
- **🖥️ Web GUI**: Remote view/control agents from any browser via QR code pairing

## 📦 Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                    GitHub (External)                          │
│  Someone mentions @trybotster in issue/PR                    │
└─────────────────────┬────────────────────────────────────────┘
                      │ Webhook
                      ↓
┌──────────────────────────────────────────────────────────────┐
│              Rails Server (Message Broker + Relay)            │
│                                                               │
│  • Receives GitHub webhooks                                  │
│  • Creates Integrations::Github::Message records              │
│  • Verifies repo access via GitHub API                       │
│  • Provides MCP tools for agents                             │
│  • Relays E2E encrypted terminal data (cannot decrypt)       │
│  • Auto-cleanup on issue/PR close                            │
│                                                               │
│  Event Types:                                                │
│  • github_mention - New @trybotster mention                  │
│  • agent_cleanup - Issue/PR closed, cleanup agent            │
└─────────────────────┬────────────────────────────────────────┘
                      │ HTTP Polling + ActionCable WebSocket
                      ↓
┌──────────────────────────────────────────────────────────────┐
│               Rust Daemon (botster-hub)                       │
│                                                               │
│  • Interactive TUI (ratatui)                                 │
│  • Polls Rails API for GitHub messages                       │
│  • Signal Protocol encryption for browser streaming          │
│  • Creates/deletes git worktrees                             │
│  • Spawns Claude in PTY for each agent                       │
│  • Routes keyboard input to selected agent                   │
│  • QR code pairing for browser connections                   │
│                                                               │
│  Agent Sessions:                                             │
│  • Key: "repo-safe-issue_number"                            │
│  • Worktree: ~/botster-sessions/org-repo-123/              │
│  • Full VT100 terminal emulation                            │
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
# Your account is now ready - no API key generation needed!
```

### 4. Daemon Setup

Build the daemon:

```bash
cd cli
cargo build --release
```

The binary will be at `target/release/botster-hub`.

**First-time setup** - The CLI uses device authorization (like signing into a TV):

```bash
./target/release/botster-hub start

# CLI will display:
#   To authorize this device, visit: https://trybotster.com/users/hubs/new
#   And enter code: ABCD-1234
#   [QR code displayed]
#
# Scan QR or visit URL, enter the code, and approve.
# Token is saved securely in your OS keychain.
```

**Optional environment overrides:**

```bash
# export BOTSTER_SERVER_URL="https://your-domain.com"  # default: https://trybotster.com
# export BOTSTER_WORKTREE_BASE="$HOME/my-worktrees"    # default: ~/botster-sessions
# export BOTSTER_POLL_INTERVAL="10"                    # default: 5 seconds
# export BOTSTER_TOKEN="btstr_..."                     # CI/CD: skip device flow
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

### Web GUI

View and control agents from any browser with E2E encryption.

**How it works:**

1. CLI displays QR code in the TUI containing its Signal Protocol public keys
2. Open https://trybotster.com/hubs in your browser
3. Scan the QR code with your phone or click "Connect" and scan with webcam
4. Browser and CLI establish encrypted Signal Protocol session
5. Terminal output streams in real-time - server only sees encrypted blobs

**Pairing is per-device:** Each browser/device scans and pairs independently. The server never has access to decryption keys.

## 🛠️ Configuration

### Environment Variables

**Authentication (optional - device flow is preferred):**

- `BOTSTER_TOKEN` - Skip device authorization flow (for CI/CD)

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
  "poll_interval": 5,
  "agent_timeout": 3600,
  "max_sessions": 20,
  "worktree_base": "/Users/you/botster-sessions"
}
```

**Note:** Tokens are stored in your OS keychain (macOS Keychain, Linux Secret Service), not in this config file. Environment variables override config file values.

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
│   │   ├── device_token.rb          # btstr_ auth tokens
│   │   ├── hub.rb                   # Hub records
│   │   ├── github/app.rb            # GitHub API wrapper
│   │   └── user.rb                  # User auth
│   │
│   ├── channels/
│   │   ├── hub_command_channel.rb     # Hub command channel (signaling)
│   │   └── preview_channel.rb         # Preview relay (legacy ActionCable)
│   │
│   ├── controllers/
│   │   ├── hubs_controller.rb              # Hub management
│   │   ├── hubs/messages_controller.rb     # Message polling
│   │   ├── hubs/codes_controller.rb        # Device authorization
│   │   └── github/webhooks_controller.rb   # Webhook receiver
│   │
│   ├── javascript/
│   │   ├── signal/                  # Signal Protocol WASM wrapper
│   │   ├── workers/                 # Web Worker for crypto isolation
│   │   └── controllers/             # Stimulus controllers
│   │
│   └── mcp/tools/                   # MCP tool implementations
│
├── cli/                             # Rust daemon
│   ├── src/
│   │   ├── main.rs                  # Entry point
│   │   ├── config.rs                # Configuration + keyring
│   │   ├── auth.rs                  # Device authorization flow
│   │   ├── git.rs                   # Worktree operations
│   │   ├── hub/                     # Hub management
│   │   ├── relay/                   # Browser relay + Signal Protocol
│   │   │   ├── signal.rs            # X3DH + Double Ratchet + Kyber
│   │   │   └── connection.rs        # ActionCable WebSocket
│   │   ├── agent/                   # Agent/PTY management
│   │   └── tui/                     # Terminal UI (ratatui)
│   └── Cargo.toml
│
└── README.md
```

## 🔒 Security

### End-to-End Encrypted Terminal Streaming

When you view agents through the Web GUI, terminal content is **end-to-end encrypted** using the Signal Protocol. The server acts as a pure relay and **cannot decrypt your terminal output**.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         Security Architecture                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   ┌──────────────┐                                    ┌──────────────┐      │
│   │   Browser    │                                    │     CLI      │      │
│   │              │                                    │              │      │
│   │  ┌────────┐  │      1. QR Code Scan (Visual)      │  ┌────────┐  │      │
│   │  │ Signal │  │◄──────────────────────────────────►│  │ Signal │  │      │
│   │  │Protocol│  │      (Keys exchanged locally)      │  │Protocol│  │      │
│   │  └────────┘  │                                    │  └────────┘  │      │
│   │      │       │                                    │      │       │      │
│   │      │       │   2. Encrypted Terminal Data       │      │       │      │
│   │      ▼       │         (ciphertext only)          │      ▼       │      │
│   │  [Decrypt]   │◄──────────────────────────────────►│  [Encrypt]   │      │
│   │      │       │                │                   │      ▲       │      │
│   │      ▼       │                │                   │      │       │      │
│   │  Terminal    │                │                   │  PTY Output  │      │
│   │   Display    │                │                   │              │      │
│   └──────────────┘                │                   └──────────────┘      │
│                                   │                                          │
│                    ┌──────────────▼──────────────┐                          │
│                    │      Rails Server           │                          │
│                    │      (Pure Relay)           │                          │
│                    │                             │                          │
│                    │  ✓ Sees: connection timing  │                          │
│                    │  ✓ Sees: message sizes      │                          │
│                    │  ✗ Cannot see: plaintext    │                          │
│                    │  ✗ Cannot see: keystrokes   │                          │
│                    │  ✗ Cannot decrypt anything  │                          │
│                    └─────────────────────────────┘                          │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Cryptographic Details:**

| Component | Algorithm | Purpose |
|-----------|-----------|---------|
| Key Exchange | X3DH (Extended Triple Diffie-Hellman) | Initial session establishment |
| Message Encryption | Double Ratchet + AES-256-GCM | Forward secrecy per message |
| Post-Quantum | Kyber1024 | Resistance to future quantum attacks |
| Signing | Ed25519 | Identity verification |

**How Pairing Works:**

1. CLI displays QR code containing its public keys
2. You scan QR with your browser (visual channel - hard to MITM)
3. Browser and CLI perform X3DH key exchange
4. All subsequent messages encrypted with Double Ratchet
5. Server only sees encrypted blobs it cannot decrypt

### Browser Security

- **Web Worker Isolation**: All cryptographic operations run in a Web Worker. Even if XSS compromises the main page, session keys remain isolated in the worker thread.
- **Non-Extractable Keys**: Browser encryption keys are marked non-extractable via Web Crypto API.
- **Session Encryption**: IndexedDB sessions encrypted with AES-256-GCM using keys derived from non-extractable CryptoKey.

### CLI Token Security

- **OS Keyring Storage**: API tokens stored in macOS Keychain or Linux Secret Service, not plaintext config files.
- **No Query Parameters**: Authentication via `Authorization: Bearer` header only. Tokens never appear in URLs or server logs.

### Server-Side Security

**Webhook Verification:** GitHub webhooks verified using HMAC-SHA256 signatures.

**API Authentication:** CLI authenticates using device tokens with `btstr_` prefix, validated per-request.

**Repository Access:** Users must have GitHub access to a repository to receive messages for it. Rails verifies access via GitHub API before delivering messages.

**Bot Attribution:** All GitHub actions show as `@trybotster[bot]` using GitHub App installation tokens.

### Trust Model

**You trust:**
- The CLI binary you run (verify source/build)
- Browser JavaScript served by trybotster.com
- That you scanned the correct QR code

**You don't need to trust:**
- The server with your terminal content (E2E encrypted)
- Network infrastructure (encrypted in transit)

**The server knows:**
- When you connect and disconnect
- How much data flows (message sizes)
- Which hub you're connected to

**The server cannot know:**
- What commands you run
- What output appears in your terminal
- Your keystrokes

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
Integrations::Github::Message.create!(
  event_type: "github_mention",
  repo: "owner/repo",
  issue_number: 999,
  payload: {
    comment_body: "@trybotster test this",
    comment_author: "testuser",
    issue_title: "Test Issue",
    issue_body: "Description",
    issue_url: "https://github.com/owner/repo/issues/999",
    is_pr: false,
    prompt: "Work on issue #999"
  }
)
```

## 🚧 Roadmap

- [x] Auto-cleanup on issue/PR close
- [x] Smart agent deduplication
- [x] Interactive TUI
- [x] Web GUI with E2E encrypted terminal streaming
- [x] Signal Protocol encryption (X3DH + Double Ratchet + Kyber)
- [x] Device authorization flow (no manual API keys)
- [x] OS keychain token storage
- [ ] Agent timeout handling
- [ ] Metrics and monitoring
- [ ] Multi-repo support in single daemon

## 🤝 Contributing

Contributions welcome! This project follows Rails conventions and uses Rust for the daemon.

## 📄 License

MIT License

---

**Questions?** Open an issue on GitHub.
