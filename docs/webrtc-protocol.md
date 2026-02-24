# WebRTC Message Protocol

This document defines the message protocol between the browser and CLI over WebRTC DataChannel.

## Framing

Messages use a simple type prefix:

| Prefix | Format | Use |
|--------|--------|-----|
| (none) | UTF-8 JSON | Structured messages |
| `0x01` | Raw bytes | PTY output (binary) |

## Message Structure

All JSON messages have a `type` field that identifies the message schema:

```json
{
  "type": "message_type",
  "subscriptionId": "sub_123",
  ...fields specific to type
}
```

### Common Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `type` | string | Yes | Message type identifier |
| `subscriptionId` | string | Sometimes | Links message to a subscription |

## Browser → CLI Messages

### subscribe

Subscribe to a channel for receiving events.

```json
{
  "type": "subscribe",
  "subscriptionId": "sub_1_1234567890",
  "channel": "HubChannel",
  "params": {
    "agent_index": 0,
    "pty_index": 0
  }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `subscriptionId` | string | Yes | Unique subscription identifier |
| `channel` | string | Yes | Channel name: `HubChannel`, `TerminalRelayChannel`, `PreviewChannel` |
| `params` | object | No | Channel-specific parameters |
| `params.agent_index` | integer | For terminal | Agent index (0-based) |
| `params.pty_index` | integer | For terminal | PTY index: 0=CLI, 1=Server |

### unsubscribe

Unsubscribe from a channel.

```json
{
  "type": "unsubscribe",
  "subscriptionId": "sub_1_1234567890"
}
```

### input (Terminal)

Send keyboard input to a terminal.

```json
{
  "subscriptionId": "sub_1_1234567890",
  "type": "input",
  "data": "ls -la\r"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `data` | string | Yes | Raw keyboard input (may include escape sequences) |

### resize (Terminal)

Resize terminal dimensions.

```json
{
  "subscriptionId": "sub_1_1234567890",
  "type": "resize",
  "rows": 24,
  "cols": 80
}
```

### handshake (Terminal)

Initial terminal handshake with dimensions.

```json
{
  "subscriptionId": "sub_1_1234567890",
  "type": "handshake",
  "rows": 24,
  "cols": 80
}
```

### list_agents (Hub)

Request current agent list.

```json
{
  "subscriptionId": "sub_1_1234567890",
  "type": "list_agents"
}
```

### list_worktrees (Hub)

Request available worktrees.

```json
{
  "subscriptionId": "sub_1_1234567890",
  "type": "list_worktrees"
}
```

### create_agent (Hub)

Create a new agent.

```json
{
  "subscriptionId": "sub_1_1234567890",
  "type": "create_agent",
  "issue_or_branch": "feature-xyz",
  "prompt": "Optional initial prompt",
  "from_worktree": "/path/to/existing/worktree"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `issue_or_branch` | string | Yes | Issue number or branch name |
| `prompt` | string | No | Initial prompt for the agent |
| `from_worktree` | string | No | Reopen from existing worktree path |

### delete_agent (Hub)

Delete an agent.

```json
{
  "subscriptionId": "sub_1_1234567890",
  "type": "delete_agent",
  "agent_id": "session-key-here",
  "delete_worktree": false
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `agent_id` | string | Yes | Agent session key |
| `delete_worktree` | boolean | No | Also delete the git worktree (default: false) |

## CLI → Browser Messages

### subscribed

Confirmation that subscription is active.

```json
{
  "type": "subscribed",
  "subscriptionId": "sub_1_1234567890"
}
```

### agent_list

List of all agents. Sent on HubChannel subscription and on request.

```json
{
  "type": "agent_list",
  "subscriptionId": "sub_1_1234567890",
  "agents": [
    {
      "index": 0,
      "id": "session-key-here",
      "repo": "owner/repo",
      "issue_number": 42,
      "branch_name": "botster-issue-42",
      "status": "Running",
      "port": 3001,
      "server_running": true,
      "has_server_pty": true,
      "pty_count": 2
    }
  ]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `agents` | array | List of agent objects (empty array `[]` if none) |

**Agent object fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `index` | integer | Yes | Agent index (0-based) |
| `id` | string | Yes | Session key |
| `repo` | string | No | Repository in `owner/repo` format |
| `issue_number` | integer | No | GitHub issue number |
| `branch_name` | string | No | Git branch name |
| `status` | string | No | Agent status |
| `port` | integer | No | Development server port |
| `server_running` | boolean | No | Whether dev server is running |
| `has_server_pty` | boolean | No | Whether server PTY exists |
| `pty_count` | integer | Yes | Number of PTY sessions |

### worktree_list

List of available worktrees. Sent on HubChannel subscription and on request.

```json
{
  "type": "worktree_list",
  "subscriptionId": "sub_1_1234567890",
  "worktrees": [
    {
      "path": "/Users/user/botster-sessions/repo-branch",
      "branch": "feature-branch"
    }
  ]
}
```

| Field | Type | Description |
|-------|------|-------------|
| `worktrees` | array | List of worktree objects (empty array `[]` if none) |

**Worktree object fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `path` | string | Yes | Absolute path to worktree |
| `branch` | string | Yes | Git branch name |

### agent_created

Broadcast when a new agent is created.

```json
{
  "type": "agent_created",
  "subscriptionId": "sub_1_1234567890",
  "agent": {
    "id": "session-key-here",
    "repo": "owner/repo",
    "issue_number": 42,
    "branch_name": "botster-issue-42"
  }
}
```

### agent_deleted

Broadcast when an agent is deleted.

```json
{
  "type": "agent_deleted",
  "subscriptionId": "sub_1_1234567890",
  "agent_id": "session-key-here"
}
```

### agent_status_changed

Broadcast when an agent's status changes.

```json
{
  "type": "agent_status_changed",
  "subscriptionId": "sub_1_1234567890",
  "agent_id": "session-key-here",
  "status": "Running"
}
```

### error

Error response.

```json
{
  "type": "error",
  "subscriptionId": "sub_1_1234567890",
  "error": "Human-readable error message"
}
```

### ack

Acknowledgment (used in handshake).

```json
{
  "type": "ack",
  "subscriptionId": "sub_1_1234567890",
  "timestamp": 1234567890000
}
```

## Binary Messages

### PTY Output

Terminal output is sent as binary with a `0x01` prefix:

```
[0x01][raw terminal bytes]
```

The raw bytes are the terminal output including ANSI escape sequences. No JSON encoding.

## Type Conventions

- **Arrays** are always JSON arrays `[]`, never objects `{}`
- **Optional fields** are omitted (not `null`)
- **Timestamps** are Unix milliseconds (integer)
- **Indices** are 0-based integers

## Channels

| Channel | Purpose |
|---------|---------|
| `HubChannel` | Agent lifecycle, worktrees, control plane |
| `TerminalRelayChannel` | PTY input/output for a specific agent+pty |
| `PreviewChannel` | Development server preview (future) |
