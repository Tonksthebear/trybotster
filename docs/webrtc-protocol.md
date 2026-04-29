# WebRTC Message Protocol

This document defines the message protocol between the browser and CLI over
WebRTC DataChannel. The durable model-state frames in this protocol are also
the shared client state contract used by other clients, including the TUI over
its hub bridge. Browser and TUI clients are equal consumers of the entity frame
stream; WebRTC is only one transport.

## Client State Paths

Botster uses one durable shared state path for client-visible model data:

- `entity_snapshot`
- `entity_upsert`
- `entity_patch`
- `entity_remove`

Every client that maintains Botster model state should apply those four entity
frames into normalized per-entity stores. They are not browser-specific, and no
browser-only frame should compete with them as a second durable state channel.

Other frame families are intentionally non-durable:

- request-scoped responses such as `agent_config`, `session_types`, `fs:*`, and
  `template:response` answer one in-flight command and are not shared state
- transient events such as `transient_event`, `spawn_target_feedback`,
  `bridge_reconnected`, `hub_recovery_state`, and `hub_ready` drive immediate
  workflow or connection effects
- `ui_route_registry` is a presentation/control snapshot for routable surfaces,
  not model state
- `ui_tree_snapshot` is a presentation snapshot for one surface render, not a
  durable entity store
- terminal binary frames are PTY stream data for a terminal subscription, not
  hub model state

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

## CLI → Client Messages

The frames in this section are shown on the WebRTC transport, but durable entity
frames are client-wide. Browser and TUI consumers should apply the same
`entity_*` semantics when they receive those envelopes over their respective
hub transports.

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

These frames are the only durable shared state path. The browser applies them
to frontend entity stores; the TUI applies the same envelopes to Rust entity
stores. Presentation snapshots, route registries, request responses, and
transient events must not be treated as alternate model-state channels.

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

Error response. Request-scoped errors explain a failed command or subscription;
they do not mutate durable model state.

```json
{
  "type": "error",
  "subscriptionId": "sub_1_1234567890",
  "error": "Human-readable error message"
}
```

### ack

Acknowledgment (used in handshake). This is connection control, not durable
model state.

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
| `HubChannel` | Durable entity model state plus non-durable control/presentation frames |
| `TerminalRelayChannel` | PTY input/output for a specific session |
| `PreviewChannel` | Development server preview (future) |
