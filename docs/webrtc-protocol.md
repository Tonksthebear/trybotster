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
  "subscriptionId": "terminal_sess-abc123",
  "channel": "HubChannel",
  "params": {
    "session_uuid": "sess-abc123"
  }
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `subscriptionId` | string | Yes | Unique subscription identifier |
| `channel` | string | Yes | Channel name: `HubChannel`, `TerminalRelayChannel`, `PreviewChannel` |
| `params` | object | No | Channel-specific parameters |
| `params.session_uuid` | string | For terminal/preview | Session UUID identifying the PTY session |

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

### entity_snapshot / entity_upsert / entity_patch / entity_remove

Hub model state is sent through entity frames. Subscribing to the hub channel
sends one `entity_snapshot` per registered entity type; subsequent changes use
`entity_upsert`, `entity_patch`, or `entity_remove`.

```json
{
  "type": "entity_snapshot",
  "subscriptionId": "sub_1_1234567890",
  "entity_type": "session",
  "snapshot_seq": 42,
  "items": [
    {
      "id": "session-key-here",
      "session_uuid": "sess-abc123",
      "workspace_id": "ws-1730000000000-abcdef",
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
| `entity_type` | string | `session`, `workspace`, `spawn_target`, `worktree`, `hub`, or `connection_code` |
| `snapshot_seq` | integer | Monotonic sequence used to reject stale frames |
| `items` | array | Full records for `entity_snapshot` |
| `id` | string | Entity id for upsert, patch, and remove frames |
| `entity` | object | Full record for `entity_upsert` |
| `patch` | object | Sparse top-level field changes for `entity_patch` |

**Session fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Session key |
| `session_uuid` | string | Yes | Session UUID for addressing |
| `workspace_id` | string | No | Owning workspace ID for grouping |
| `repo` | string | No | Repository in `owner/repo` format |
| `issue_number` | integer | No | GitHub issue number |
| `branch_name` | string | No | Git branch name |
| `status` | string | No | Agent status |
| `port` | integer | No | Development server port |
| `server_running` | boolean | No | Whether dev server is running |
| `has_server_pty` | boolean | No | Whether server PTY exists |
| `pty_count` | integer | Yes | Number of PTY sessions |

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
- **Session UUIDs** are string identifiers for PTY sessions

## Channels

| Channel | Purpose |
|---------|---------|
| `HubChannel` | Entity model state and control plane |
| `TerminalRelayChannel` | PTY input/output for a specific session |
| `PreviewChannel` | Development server preview (future) |
