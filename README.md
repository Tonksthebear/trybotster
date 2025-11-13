# Botster Hub

**GitHub Mention → Local Agent Automation**

Botster Hub automates GitHub issue/PR mentions (e.g., `@trybotster fix this`) into local CLI agent sessions. When someone mentions `@trybotster` in a GitHub issue or PR, all authorized users' local daemons receive the notification and can spawn a terminal session with their chosen agent (Claude Code, Aider, Cursor, etc.) to handle the request.

## 🎯 What It Does

```
GitHub Issue Comment
  "@trybotster can you fix this bug?"
        ↓
  Rails webhook receives @trybotster mention
        ↓
  Creates pending messages for all active users
        ↓
  Local daemons poll and detect new message
        ↓
  Server verifies user has repo access (via GitHub API)
        ↓
  Authorized daemons receive the message
        ↓
  Daemon creates git worktree
        ↓
  Spawns terminal with your agent (Claude, Aider, etc.)
        ↓
  Agent analyzes issue and makes changes
        ↓
  Marks complete → automatic cleanup
```

### Key Features

- **🔒 Local-first**: Your code never leaves your machine
- **🤖 Agent-agnostic**: Works with Claude Code, Aider, Cursor CLI, or any CLI tool
- **⚡ Zero dependencies**: Pure bash daemon using only macOS built-ins
- **📡 Simple polling**: HTTP REST API (no WebSockets, no hanging connections)
- **🎨 RESTful Rails**: Clean, conventional routing
- **🔧 MCP Tools**: Agents can interact with GitHub via Model Context Protocol

## 📦 Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                    GitHub (External)                          │
│  Someone mentions @trybotster in issue/PR comment            │
└─────────────────────┬────────────────────────────────────────┘
                      │ Webhook
                      ↓
┌──────────────────────────────────────────────────────────────┐
│              Rails Server (Notification Broker)               │
│                                                               │
│  Models:                                                      │
│  • Bot::Message - Message queue with lifecycle tracking      │
│  • Github::App - GitHub API interactions                     │
│  • User - Authentication, GitHub App tokens, repo access     │
│                                                               │
│  Controllers:                                                 │
│  • Github::WebhooksController - Receives @trybotster webhook │
│  • Bots::MessagesController - REST API with repo auth check  │
│                                                               │
│  Authorization Flow:                                          │
│  1. Webhook creates messages for all active users            │
│  2. Daemon polls → controller checks repo access             │
│  3. Only authorized users receive messages                   │
│                                                               │
│  MCP Tools (for agents):                                     │
│  • github_get_issue                                          │
│  • github_create_pull_request                                │
│  • github_comment_issue                                      │
│  • github_update_issue                                       │
│  • + more...                                                 │
└─────────────────────┬────────────────────────────────────────┘
                      │ HTTP Polling (GET /bots/messages)
                      │ + Repo Access Check (via GitHub API)
                      ↓
┌──────────────────────────────────────────────────────────────┐
│                  Local Machine (macOS)                        │
│                                                               │
│  bin/botster_hub (Pure Bash Daemon)                          │
│  • Polls Rails API every 5 seconds                           │
│  • Manages session state (~/.botster_hub/sessions/)          │
│  • Creates git worktrees                                     │
│  • Spawns Terminal.app tabs                                  │
│  • Monitors completion markers                               │
│  • Cleans up on completion                                   │
│                                                               │
│  Terminal Sessions (one per mention)                         │
│  • Git worktree: ~/botster-sessions/org-repo-123/           │
│  • Runs: claude, aider, cursor, etc.                         │
│  • Has MCP access to GitHub via Rails                        │
└──────────────────────────────────────────────────────────────┘
```

## 🚀 Quick Start

### Prerequisites

**Server:**

- Ruby 3.3+
- PostgreSQL
- GitHub App (for bot interactions)

**Client (Local Daemon):**

- macOS (for Terminal.app automation)
- That's it! No gems, no dependencies, just bash

### 1. Server Setup

```bash
# Clone and install
git clone https://github.com/yourusername/trybotster.git
cd trybotster
bundle install

# Setup database
rails db:create db:migrate

# Configure GitHub App (see "GitHub App Setup" below)
# You'll need to create a .env file or set environment variables
```

### 2. GitHub App Setup

1. **Create a GitHub App** at https://github.com/settings/apps/new

2. **Configure webhook:**
   - Webhook URL: `https://your-domain.com/github/webhooks`
   - Webhook secret: Generate a random string
   - Subscribe to events:
     - ✅ Issue comment
     - ✅ Pull request review comment

3. **Set permissions:**
   - Issues: Read & Write
   - Pull requests: Read & Write
   - Metadata: Read-only

4. **Configure environment variables:**

```bash
# Required
GITHUB_APP_ID=your_app_id
GITHUB_APP_CLIENT_ID=your_client_id
GITHUB_APP_CLIENT_SECRET=your_client_secret
GITHUB_APP_PRIVATE_KEY=your_private_key
GITHUB_WEBHOOK_SECRET=your_webhook_secret

# Optional
APP_URL=http://localhost:3000  # For OAuth callbacks
```

5. **Install the app** on your repositories

### 3. User Setup

```bash
# Start Rails server
rails server

# 1. Visit http://localhost:3000
# 2. Click "Login with GitHub"
# 3. Authorize the GitHub App
# 4. Generate your API key in Rails console:

rails console
user = User.find_by(email: "you@example.com")
user.regenerate_api_key!
puts user.api_key  # Save this!
```

### 4. Local Daemon Setup

```bash
# Configure the daemon
bin/botster_hub config server_url "http://localhost:3000"
bin/botster_hub config api_key "your_api_key_from_step_3"
bin/botster_hub config agent_command "claude"  # or "aider", "cursor", etc.

# Optional: Adjust polling interval and session limits
bin/botster_hub config poll_interval 5      # seconds
bin/botster_hub config max_sessions 20      # concurrent sessions

# Start the daemon (stays in foreground)
bin/botster_hub start
```

## 📖 Usage

### Basic Workflow

1. **Someone mentions @trybotster in GitHub:**

   ```
   GitHub issue/PR comment:
   "@trybotster can you investigate this memory leak?"
   ```

2. **Daemon automatically responds:**

   ```
   [2025-11-12 10:30:45] Received 1 new message(s)
   [2025-11-12 10:30:45] Processing GitHub mention in myorg/myrepo#123
   [2025-11-12 10:30:46] ✓ Created worktree: ~/botster-sessions/myorg-myrepo-123
   [2025-11-12 10:30:47] ✓ Spawned terminal for myorg/myrepo#123
   [2025-11-12 10:30:47] ✓ Session created: myorg-myrepo-123
   ```

3. **Terminal opens with your agent:**
   - New Terminal.app tab titled "Botster: myorg/myrepo#123"
   - Working directory: `~/botster-sessions/myorg-myrepo-123/`
   - Agent (claude/aider) running with issue context

4. **Agent works on the issue:**
   - Has access to issue details via MCP tools
   - Can create PRs, comment on issues, etc.
   - Makes changes in isolated git worktree

5. **Mark as complete:**

   ```bash
   # In the spawned terminal, when done:
   echo "RESOLVED" > .botster_status
   # OR
   echo "DONE" > .botster_status
   ```

6. **Automatic cleanup:**
   - Daemon detects completion marker
   - Removes git worktree
   - Cleans up session state

### Daemon Commands

```bash
# Start daemon (stays in foreground - use tmux/screen for background)
bin/botster_hub start

# Check active sessions
bin/botster_hub status

# Kill a specific session
bin/botster_hub kill myorg-myrepo-123

# Clean up stale worktrees
bin/botster_hub cleanup

# Show/set configuration
bin/botster_hub config                          # Show all
bin/botster_hub config api_key                  # Show specific
bin/botster_hub config agent_command "aider"    # Set value

# Show version
bin/botster_hub version
```

### Configuration

Configuration is stored in `~/.botster_hub/config` as simple key=value pairs:

```bash
server_url=http://localhost:3000
api_key=your_api_key_here
agent_command=claude
completion_marker=.botster_status
max_sessions=20
poll_interval=5
worktree_base=/Users/you/botster-sessions
```

### Session State

Each active session has a file in `~/.botster_hub/sessions/<repo>-<issue>`:

```bash
message_id=42
repo=myorg/myrepo
issue_number=123
worktree_path=/Users/you/botster-sessions/myorg-myrepo-123
terminal_id=12345
started_at=2025-11-12T10:30:45Z
status=active
```

## 🛠️ MCP Tools for Agents

When your agent runs in a spawned session, it has access to these MCP tools via the Rails server:

### GitHub Operations

- **`github_get_issue`** - Get issue/PR details

  ```
  repo: "owner/repo"
  issue_number: 123
  ```

- **`github_list_issues`** - List issues in a repository

  ```
  repo: "owner/repo"
  state: "open" | "closed" | "all"
  ```

- **`github_create_pull_request`** - Create a PR

  ```
  repo: "owner/repo"
  title: "Fix memory leak"
  head: "botster-fix-123"
  base: "main"
  body: "Description..."
  ```

- **`github_update_issue`** - Update issue status/labels

  ```
  repo: "owner/repo"
  issue_number: 123
  state: "closed"
  labels: ["bug", "fixed"]
  ```

- **`github_comment_issue`** - Add comment to issue/PR

  ```
  repo: "owner/repo"
  issue_number: 123
  body: "Fixed in PR #124"
  ```

- **`github_get_pull_request`** - Get PR details including diff
  ```
  repo: "owner/repo"
  pull_number: 124
  ```

All GitHub operations show as **[bot]** attribution on GitHub.

### Configuring Agent MCP Access

**For Claude Code:**

```bash
# In spawned terminal, BOTSTER_API_KEY is already exported
claude --mcp-server "rails:http://localhost:3000/mcp"
```

**For other agents**, ensure they can access the MCP endpoint:

- Base URL: `http://localhost:3000/mcp`
- Authentication: `X-API-Key: $BOTSTER_API_KEY` (auto-exported)

## 🏗️ Project Structure

```
trybotster/
├── app/
│   ├── models/
│   │   ├── bot.rb                    # Namespace module
│   │   ├── bot/message.rb            # Message queue with lifecycle
│   │   ├── github/app.rb             # GitHub API interactions
│   │   └── user.rb                   # User auth & GitHub tokens
│   │
│   ├── controllers/
│   │   ├── bots/
│   │   │   └── messages_controller.rb    # REST API for daemon
│   │   └── github/
│   │       ├── webhooks_controller.rb    # Webhook receiver
│   │       ├── authorization_controller.rb
│   │       └── callbacks_controller.rb
│   │
│   └── mcp/
│       └── tools/
│           ├── application_mcp_tool.rb
│           ├── github_get_issue_tool.rb
│           ├── github_create_pull_request_tool.rb
│           ├── github_comment_issue_tool.rb
│           ├── github_update_issue_tool.rb
│           ├── github_get_pull_request_tool.rb
│           └── ...
│
├── bin/
│   └── botster_hub              # Pure bash daemon (590 lines!)
│
├── config/
│   └── routes.rb                # RESTful routes
│
├── db/
│   ├── migrate/
│   │   └── ..._create_bot_messages.rb
│   └── schema.rb
│
└── BOTSTER_HUB.md              # Detailed architecture doc
```

## 🔒 Security

### Webhook Signature Verification

GitHub webhooks are verified using HMAC-SHA256:

```ruby
# In production, set this environment variable:
GITHUB_WEBHOOK_SECRET=your_webhook_secret

# Automatically verified in Github::WebhooksController
```

### API Key Authentication

- Each user has a unique encrypted API key
- Used for daemon → Rails API communication
- Regenerate anytime: `user.regenerate_api_key!`

### Bot Attribution

All GitHub actions via MCP tools use GitHub App installation tokens, showing as `[bot]` on GitHub.

## 📊 Database Schema

### `bot_messages`

```ruby
create_table :bot_messages do |t|
  t.references :user, null: false, foreign_key: true
  t.string :event_type, null: false           # "github_mention"
  t.jsonb :payload, null: false, default: {}  # GitHub context
  t.datetime :sent_at
  t.datetime :acknowledged_at
  t.string :status, default: "pending"        # pending → sent → acknowledged → failed
  t.timestamps
end
```

**Lifecycle:**

1. `pending` - Created by webhook, waiting for daemon poll
2. `sent` - Returned in API response to daemon
3. `acknowledged` - Daemon confirmed receipt (via PATCH)
4. `failed` - Error occurred

### `users`

```ruby
# Key fields for Botster Hub:
t.string :api_key                          # Encrypted, for daemon auth
t.string :username                         # GitHub username
t.string :github_app_token                 # OAuth token
t.string :github_app_refresh_token         # For token refresh
t.datetime :github_app_token_expires_at
t.string :github_app_installation_id       # For bot attribution
```

## 🧪 Testing

### Manual End-to-End Test

1. **Start Rails server:**

   ```bash
   rails server
   ```

2. **Start daemon:**

   ```bash
   bin/botster_hub start
   ```

3. **Trigger a mention:**
   - Comment on a GitHub issue: `@trybotster test this`

4. **Verify flow:**

   ```bash
   # Check daemon log
   tail -f ~/.botster_hub/botster_hub.log

   # Check active sessions
   bin/botster_hub status
   ```

5. **Test completion:**

   ```bash
   # In spawned terminal
   echo "RESOLVED" > .botster_status

   # Daemon should clean up within 5 seconds
   ```

### Testing Without GitHub

Create a test message in Rails console:

```ruby
user = User.find_by(username: "yourusername")
message = user.bot_messages.create!(
  event_type: "github_mention",
  payload: {
    repo: "test/repo",
    issue_number: 999,
    comment_body: "@trybotster test this",
    comment_author: "someone",
    issue_title: "Test Issue",
    issue_body: "Test description",
    issue_url: "https://github.com/test/repo/issues/999",
    is_pr: false,
    context: "Test Issue\n\nTest description\n\nComment:\n@trybotster test this"
  }
)

# Note: The message will only be delivered to users who have access to "test/repo"
# You can bypass the repo check by setting repo to nil or empty string for testing
```

## 🔧 Troubleshooting

### Daemon Not Receiving Messages

```bash
# Check daemon is running
bin/botster_hub status

# Check configuration
bin/botster_hub config

# Verify API key is correct
curl -H "X-API-Key: your_key" http://localhost:3000/bots/messages

# Check daemon logs
tail -f ~/.botster_hub/botster_hub.log
```

### Terminal Not Spawning

```bash
# Verify agent command is available
which claude
which aider

# Check AppleScript permissions
# System Preferences → Security & Privacy → Automation
# Grant Terminal permission to control Terminal.app

# Check worktree creation
git worktree list
```

### Sessions Not Cleaning Up

```bash
# Check completion marker file
cat ~/botster-sessions/org-repo-123/.botster_status
# Should contain "RESOLVED" or "DONE"

# Manual cleanup
bin/botster_hub cleanup

# Force kill session
bin/botster_hub kill org-repo-123
```

### Webhook Not Working

```bash
# Check Rails logs
tail -f log/development.log | grep GitHub

# Verify webhook secret
echo $GITHUB_WEBHOOK_SECRET

# Test webhook locally with ngrok
ngrok http 3000
# Update GitHub App webhook URL to ngrok URL
```

## 🎨 Design Decisions

### Why No `app/services/`?

**Rule:** Project-specific logic lives in `app/models/`, generic utilities in `lib/`.

- ✅ `app/models/github/app.rb` - Project-specific GitHub integration
- ❌ `app/services/github_app_service.rb` - Unnecessary abstraction

Services create unnecessary layers. Models are the natural home for business logic in Rails.

### Why RESTful Routes Only?

All routes use `resources` except webhooks (external API constraints).

```ruby
# ✅ RESTful
resources :messages, only: [:index, :update]

# ❌ Custom actions
get 'messages/acknowledge'

# ✅ Exception: External webhook naming
post 'github/webhooks', to: 'github/webhooks#receive'
```

RESTful routes enforce consistency. The `update` action handles acknowledgment naturally.

### Why HTTP Polling over WebSockets?

- **Simple**: Just bash + curl, no WebSocket client needed
- **Reliable**: No connection state to manage
- **Proxy-friendly**: Works with any load balancer
- **Good enough**: 5-second polling is fine for GitHub mentions
- **Zero dependencies**: Pure bash with no external tools

### Why Local-First?

- **Privacy**: Code stays on your machine
- **Control**: You decide what agents can do
- **Flexibility**: Use any CLI tool
- **Performance**: Direct git operations

## 🚧 Roadmap

- [ ] iTerm2 support (better tab management)
- [ ] Linux support (gnome-terminal, terminator)
- [ ] Session timeout configuration
- [ ] Web UI for monitoring active sessions
- [ ] Rate limiting per user
- [ ] Metrics dashboard
- [ ] Multi-user worktree conflict resolution

## 📚 Additional Documentation

- [BOTSTER_HUB.md](BOTSTER_HUB.md) - Detailed architecture documentation
- [GitHub App Setup](docs/github-app-setup.md) - Step-by-step GitHub App configuration
- [MCP Tools](app/mcp/tools/) - MCP tool implementations

## 🤝 Contributing

Contributions welcome! Please follow:

1. RESTful routes only (except webhooks)
2. No `app/services/` directory
3. Models for project logic, `lib/` for generic utilities
4. Comprehensive tests for new features

## 📄 License

MIT License - See LICENSE file

## 🙏 Acknowledgments

Built with:

- Ruby on Rails 8.1
- GitHub Apps API
- Model Context Protocol (MCP)
- Pure bash (no dependencies!)
- macOS Terminal.app AppleScript automation

---

**Questions?** Open an issue or check the [detailed architecture docs](BOTSTER_HUB.md).
