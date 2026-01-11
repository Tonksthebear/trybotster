# Rust CLI Feature Parity Checklist

This document captures every feature from the Rust CLI that must be replicated in Go.

## Process Per Feature
1. **Study Rust** - Read the Rust implementation, understand all nuances
2. **Implement Go** - Write clean, modular Go code matching behavior
3. **Test** - Write tests proving the feature works correctly
4. **Iterate** - Fix issues until tests pass

---

## 1. Configuration (`config.rs`)

### Fields
- `server_url` - Rails server URL (default: https://trybotster.com)
- `headscale_url` - Headscale control server URL
- `token` - New device token (btstr_ prefix)
- `api_key` - Legacy API key (deprecated)
- `poll_interval` - Seconds between server polls (default: 5)
- `agent_timeout` - Seconds before idle agent stops (default: 3600)
- `max_sessions` - Max concurrent agents (default: 20)
- `worktree_base` - Base directory for worktrees

### Environment Overrides
- `BOTSTER_SERVER_URL`
- `HEADSCALE_URL`
- `BOTSTER_TOKEN` (takes precedence)
- `BOTSTER_API_KEY` (legacy)
- `BOTSTER_WORKTREE_BASE`
- `BOTSTER_POLL_INTERVAL`
- `BOTSTER_MAX_SESSIONS`
- `BOTSTER_AGENT_TIMEOUT`

### Token Validation
- `has_token()` returns true only if token has `btstr_` prefix
- `get_api_key()` prefers new token over legacy api_key

---

## 2. Authentication (`auth.rs`)

### Device Authorization Flow (RFC 8628)
1. POST `/hubs/codes` with `device_name` - get device_code, user_code, verification_uri
2. Display verification URL and user code to user
3. Optionally open browser (unless `BOTSTER_NO_BROWSER` set)
4. Poll GET `/hubs/codes/{device_code}` at interval
5. Handle responses: 200=success, 202=pending, 400/401/403=error
6. Return access_token on success

### Token Validation
- GET `/devices` with Bearer token
- Returns true if 2xx response

---

## 3. Agent (`agent/mod.rs`)

### Core Fields
- `id` - UUID
- `repo` - "owner/repo" format
- `issue_number` - Optional issue number
- `branch_name` - Git branch
- `worktree_path` - Path to worktree
- `start_time` - When agent started
- `status` - AgentStatus enum (Initializing, Running, etc.)
- `last_invocation_url` - GitHub URL that triggered this agent
- `tunnel_port` - Port for HTTP tunnel
- `terminal_window_id` - macOS Terminal window ID

### Dual PTY Architecture
- `cli_pty` - Primary PTY (runs main agent process)
- `server_pty` - Optional secondary PTY (runs dev server)
- `active_pty` - Which PTY is displayed (PtyView enum: Cli/Server)

### Session Key Format
- Issue-based: `{repo-safe}-{issue_number}` (e.g., "owner-repo-42")
- Branch-based: `{repo-safe}-{branch-safe}` (e.g., "owner-repo-feature-x")

### Scrollback
- Ring buffer for scrollback history
- `scroll_up(lines)`, `scroll_down(lines)`
- `scroll_to_top()`, `scroll_to_bottom()`
- `is_scrolled()` - true if not at live view
- `get_scroll_offset()` - current offset

### VT100 Terminal Emulation
- Uses `vt100` crate for terminal parsing
- `get_vt100_screen()` - rendered screen as lines
- `get_vt100_screen_with_cursor()` - screen + cursor position
- `get_screen_as_ansi()` - screen as ANSI escape sequences
- `get_screen_hash()` - hash for change detection

### Raw Output
- `drain_raw_output()` - raw bytes for browser streaming

---

## 4. PTY Session (`agent/pty.rs`)

### Spawning
- Create PTY with initial dimensions
- Spawn shell/command in PTY
- Set working directory to worktree
- Pass environment variables
- Run init commands after spawn

### I/O
- Reader thread captures PTY output
- `write_input(bytes)` - write to PTY
- Output goes to ring buffer + VT100 parser + raw buffer

### Resize
- `resize(rows, cols)` - resize PTY
- Clear VT100 screen on resize (prevents old content issues)

### Cleanup
- `kill_child()` - kill PTY child process

---

## 5. Terminal Notifications (`agent/notification.rs`)

### OSC 9 Detection
- Standard desktop notification: `\x1b]9;message\x07`
- Extract message text

### OSC 777 Detection
- rxvt-unicode style: `\x1b]777;notify;title;body\x07`
- Extract title and body

### AgentNotification Enum
- `Osc9(Option<String>)`
- `Osc777 { title: String, body: String }`

---

## 6. Git Worktree (`git.rs`)

### Repository Detection
- `detect_current_repo()` - find repo root, extract owner/repo from remote

### Worktree Operations
- `create_worktree_from_current(issue_number)` - create worktree for issue
- `create_worktree_with_branch(branch_name)` - create worktree with custom branch
- `delete_worktree_by_issue_number(issue_number)` - delete worktree
- `delete_worktree_by_path(path, branch)` - delete worktree by path
- `find_existing_worktree_for_issue(issue_number)` - check if worktree exists
- `list_worktrees(repo)` - list all worktrees
- `cleanup_worktree(clone_dir, worktree_path)` - clean up stale worktree
- `prune_stale_worktrees(repo)` - git worktree prune

### .botster_* Files
- `.botster_copy` - glob patterns for files to copy to worktree
- `.botster_init` - commands to run after worktree creation
- `.botster_teardown` - commands to run before worktree deletion

### Safety Checks (Defense in Depth)
1. Path must be within managed base directory
2. Branch name should start with "botster-"
3. Check for Claude settings marker file
4. `git2::is_worktree()` check - refuse to delete main repo

### Claude Trust
- Create `.claude/settings.local.json` with `allowedDirectories` and `permissionMode: "acceptEdits"`

---

## 7. Hub State (`hub/state.rs`)

### Agent Management
- `agents: HashMap<String, Agent>` - indexed by session key
- `agent_keys_ordered: Vec<String>` - insertion order for navigation
- `selected: usize` - currently selected index

### Selection Navigation
- `select_next()` - next agent (wraps)
- `select_previous()` - previous agent (wraps)
- `select_by_index(idx)` - 1-based index selection
- `select_by_key(key)` - select by session key

### Worktree UI
- `available_worktrees: Vec<(path, branch)>` - for spawn UI
- `load_available_worktrees()` - populate list

---

## 8. Hub Actions (`hub/actions.rs`)

### HubAction Enum
- `Quit`
- `TogglePolling`
- `Resize { rows, cols }`
- `SelectNext`, `SelectPrevious`
- `SelectByIndex(usize)`
- `TogglePtyView`
- `ScrollUp(lines)`, `ScrollDown(lines)`
- `ScrollToTop`, `ScrollToBottom`
- `SpawnAgent(AgentSpawnConfig)`
- `CloseSelectedAgent`
- `Input(Vec<u8>)`
- `ShowMenu`, `HideMenu`
- `MenuSelect(usize)`
- ... (more)

### Dispatch Pattern
- `dispatch(hub, action)` - central action handler
- All state changes flow through actions

---

## 9. Server Communication (`server/`)

### Polling
- GET `/hubs/{hub_id}/messages` - fetch pending messages
- PATCH `/hubs/{hub_id}/messages/{id}` - acknowledge message

### Heartbeat
- PATCH `/hubs/{hub_id}/heartbeat` - register hub, update last_seen

### Registration
- POST `/hubs` with hub_identifier, device_id

---

## 10. Message Handling (`server/messages.rs`)

### Parse GitHub Webhook Events
- Extract `event_type`, `issue_number`, `repo`, `sender`
- Handle: issue_mention, issue_comment, pull_request_review

### Convert to HubAction
- `message_to_hub_action(parsed, context)` -> `Option<HubAction>`
- Returns `SpawnAgent` action with full config

### Existing Agent Notification
- If agent exists for issue, send notification instead of spawning new

---

## 11. HTTP Tunnel (`tunnel.rs`)

### WebSocket Connection
- Connect to `/cable` (ActionCable)
- Subscribe to `TunnelChannel` with hub_id

### HTTP Forwarding
- Receive `http_request` messages
- Forward to local dev server on agent's port
- Send `http_response` back via ActionCable

### Port Management
- `allocate_tunnel_port()` - find available port 4001-4999
- Track agent -> port mapping

---

## 12. Browser Relay (`relay/`)

### Tailscale Integration
- Connect to Headscale via embedded tailscale (tsnet in Go)
- Browser connects via tsconnect WASM

### Browser State
- Connection status
- QR code URL

### Events
- `BrowserEvent` enum
- `browser_event_to_hub_action()` conversion

### Output Streaming
- Send terminal output to browser
- Handle browser resize

---

## 13. TUI (`tui/`)

### Terminal Guard
- RAII pattern for terminal cleanup
- Enable raw mode, alternate screen, mouse capture
- Cleanup on drop (even on panic)

### Layout (render.rs)
- Left panel: QR code + connection info
- Right panel: Terminal output
- Bottom: Status bar (polling status, agent count)
- Agent tabs at top

### Input Handling (input.rs)
- Key events -> HubAction conversion
- q=quit, p=toggle polling, Tab=toggle PTY
- Up/Down=scroll, 1-9=select agent
- Menu navigation

### QR Code (qr.rs)
- Generate QR code for connection URL
- ASCII art output for terminal

---

## 14. Device Identity (`device.rs`)

### Device Registration
- Generate device fingerprint
- Register with server: POST `/devices`
- Persist device_id to config

---

## 15. Notifications (`notifications.rs`)

### Send to Rails
- POST `/hubs/{hub_id}/notifications`
- Include repo, issue_number, invocation_url, notification_type

### Notification Types
- `question_asked` - agent needs user input (OSC 9/777)

---

## 16. Prompt Management (`prompt.rs`)

### Priority
1. Local `.botster_prompt` in worktree
2. Fetch from GitHub: `Tonksthebear/trybotster/cli/botster_prompt`

---

## 17. CLI Commands (`commands/`)

### json
- `json-get <file> <key>` - get value using dot notation
- `json-set <file> <key> <value>` - set value
- `json-delete <file> <key>` - delete key

### worktree
- `list-worktrees` - list all worktrees
- `delete-worktree <issue_number>` - delete worktree with teardown

### prompt
- `get-prompt <worktree_path>` - get prompt for worktree

### update
- `update` - self-update from GitHub releases
- `update --check` - check for updates

---

## 18. Signal Handling

### Graceful Shutdown
- Handle SIGINT, SIGTERM, SIGHUP
- Set shutdown flag
- Clean up all agents
- Send shutdown notification to server

### Panic Recovery
- Custom panic hook
- Restore terminal state before printing panic

---

## 19. Logging

### File Logging
- Log to `/tmp/botster-hub.log`
- Don't interfere with TUI

---

## 20. Environment Variables Summary

| Variable | Purpose |
|----------|---------|
| BOTSTER_TOKEN | Authentication token |
| BOTSTER_API_KEY | Legacy auth (deprecated) |
| BOTSTER_SERVER_URL | Rails server URL |
| HEADSCALE_URL | Headscale control server |
| BOTSTER_WORKTREE_BASE | Base dir for worktrees |
| BOTSTER_POLL_INTERVAL | Polling interval |
| BOTSTER_MAX_SESSIONS | Max concurrent agents |
| BOTSTER_AGENT_TIMEOUT | Idle agent timeout |
| BOTSTER_NO_BROWSER | Don't auto-open browser |
| BOTSTER_CONFIG_DIR | Override config directory |
| CI | Detect CI environment |
