# Workspace Architecture

## Problem

Session persistence in Botster is fragile:
- Agent state lives in `context.json` inside worktrees (or `data_dir/.botster/agents/<key>/`) — two paths, special-cased for main vs worktree
- After a hard restart (hub + broker both die), broker session IDs in `context.json` are stale; ghost agents appear with blank terminals and no recovery
- PTY output is only in broker ring-buffers (ephemeral, ~120s max); once broker dies, history is gone
- No central registry — resurrection requires scanning worktrees, can silently miss or find stale agents
- Orphaned `context.json` files accumulate indefinitely (no GC)
- Main-branch agents are silently ephemeral if `data_dir` is unconfigured

## Design Goals

1. **Universal** — same persistence model for main branch, worktrees, and future agent types; no special-casing
2. **Rails-free** — works fully offline/private; Rails only enters when GitHub integration is needed
3. **Deterministic resurrection** — restart should always produce the same result given the same on-disk state
4. **Meaningful recovery** — resurrection includes terminal history, not just blank screens
5. **Cross-device ready** — designed so workspace sync can layer on via hub mesh (VPN/WebRTC), not retrofitted later

## Core Concepts

### Agent = Single PTY Session (AI-driven)

An agent is exactly one PTY process with autonomy and intent — it receives prompts and produces work. No composite agents.

### Accessory = Supporting PTY Session

Any other PTY session in a workspace — a Rails server, REPL, log tail, debugger, one-shot script, whatever. An accessory has no AI autonomy; it's a tool you run alongside agents. The distinction is conceptual, not technical: both are PTY sessions under the hood.

If a workspace needs a web server alongside a Claude agent, the Claude session is an agent and the web server is an accessory. Each has its own session directory, its own PTY log, its own lifecycle.

### Session = Persistent Record of One Agent Run

A session captures everything about one agent's execution:
- Identity (uuid, label, type, role)
- Context (workspace, repo, branch, worktree path if any)
- State machine (pending → active → suspended → closed | orphaned)
- PTY output (append-only log file — survives broker and hub restarts)
- Lifecycle events (audit trail)

### Workspace = Unit of Work

A workspace groups sessions working toward a shared goal. It is:
- **A template** — declares what agents are needed (e.g., "1 Claude + 1 Rails server")
- **A runtime** — sessions are spawned from the template, can be added/removed dynamically
- **A history** — sessions persist even after the workspace is "done"

A workspace may be backed by a GitHub issue (natural cross-device anchor) or be ad-hoc.

## Directory Layout

```
~/.botster/
  workspaces/
    <workspace-id>/
      manifest.json          # workspace identity, issue ref, status
      sessions/
        <session-uuid>/
          manifest.json      # session identity, state, worktree path, broker session IDs
          pty-0.log          # append-only PTY output (survives all restarts)
          events.jsonl       # lifecycle audit trail
```

For dev: `~/.botster-dev/workspaces/` (follows existing `data_dir` config).

## Schemas

### Workspace Manifest

```json
{
  "id": "ws-abc123",
  "title": "Fix auth bug",
  "repo": "owner/repo",
  "issue_number": 42,
  "issue_url": "https://github.com/owner/repo/issues/42",
  "status": "active",
  "created_at": "2026-03-01T12:00:00Z",
  "updated_at": "2026-03-01T14:30:00Z"
}
```

- `issue_number` / `issue_url` optional — ad-hoc workspaces omit them
- `status`: `active | suspended | closed`

### Session Manifest

```json
{
  "uuid": "sess-xyz789",
  "workspace_id": "ws-abc123",
  "label": "Fix auth bug",
  "type": "ai",
  "role": "developer",
  "repo": "owner/repo",
  "branch": "botster-issue-42",
  "worktree_path": "/Users/user/.botster/worktrees/owner-repo-botster-issue-42",
  "profile_name": "github",
  "status": "active",
  "broker_sessions": {
    "0": 7
  },
  "pty_dimensions": {
    "0": { "rows": 50, "cols": 220 }
  },
  "created_at": "2026-03-01T12:00:00Z",
  "updated_at": "2026-03-01T14:30:00Z"
}
```

- `worktree_path` is `null` for main-branch agents
- `type`: `agent | accessory` — agent is AI-driven, accessory is any other PTY session
- `role`: free-form label (e.g., `developer`, `reviewer`, `rails-server`, `repl`, `log-tail`)
- `broker_sessions` maps PTY index → broker session ID (invalidated on hard restart)
- `status`: `pending | active | suspended | closed | orphaned`

### Session Status State Machine

```
pending → active → suspended → closed
                 ↘            ↗
                  orphaned
```

- `pending`: workspace declared agent, not yet spawned
- `active`: PTY running, broker session IDs valid
- `suspended`: hub restarted gracefully, broker still holds PTY (session IDs may still be valid)
- `closed`: agent exited cleanly, worktree optionally deleted, PTY log preserved
- `orphaned`: hard restart detected stale session IDs, PTY log preserved, no live process

**On hub startup**: sessions in `active` state are marked `suspended` until broker connection is confirmed. If broker confirms session ID, promote back to `active`. If not, mark `orphaned`.

## Session Lifecycle

### Create

1. Generate workspace if needed (or use existing for this issue/branch)
2. Generate session UUID
3. Write session manifest with `status: pending`
4. Spawn PTY → register with broker → get session IDs
5. Open `pty-0.log` for append
6. Update manifest: `status: active`, broker_sessions, pty_dimensions
7. Log `created` event to `events.jsonl`

### Runtime

- Agent reads/writes its own metadata via session manifest (replaces context.json)
- PTY output tee'd to `pty-0.log` continuously
- Manifest updated on significant state changes

### Close (clean)

1. Unregister PTY from broker
2. Update manifest: `status: closed`
3. Log `closed` event
4. Optionally delete worktree (manifest and PTY log preserved)

### Hub Restart (graceful — broker still running)

1. On startup, mark all `active` sessions → `suspended`
2. Connect to broker
3. For each `suspended` session: verify broker still holds session ID
   - Confirmed → promote to `active`, create ghost handles, replay `pty-0.log` into shadow screen
   - Not found → mark `orphaned`, replay `pty-0.log` for history display only
4. Log `resurrected` or `orphaned` event

### Hub Restart (hard — broker also restarted)

1. All sessions marked `suspended` on startup
2. No broker session IDs are valid
3. All `suspended` sessions → `orphaned`
4. `pty-0.log` still available for history replay
5. User can manually re-spawn orphaned sessions (creates new session UUID, new `active` session in same workspace)

## Migration from Current State

The existing `context.json` (in worktrees and `data_dir/agents/`) maps to this as:

| Old field | New location |
|-----------|-------------|
| `repo` | session manifest `repo` |
| `branch_name` | session manifest `branch` |
| `worktree_path` | session manifest `worktree_path` |
| `prompt` | session manifest (add `prompt` field) |
| `metadata.broker_session_N` | session manifest `broker_sessions` |
| `metadata.broker_pty_rows_N` | session manifest `pty_dimensions` |
| `metadata.issue_number` | workspace manifest `issue_number` |
| `metadata.invocation_url` | workspace manifest `issue_url` |
| `profile_name` | session manifest `profile_name` |
| `created_at` | session manifest `created_at` |

Migration path: on first startup after deploy, if old `context.json` found, migrate to new layout and delete old file.

## Implementation Plan

### Phase 1 — Central Session Store
**Goal:** Fix resurrection fragility. No behavior change for users.

1. Add `data_dir/workspaces/` directory creation on hub init
2. Update `agent.lua`: write session manifest to central store instead of worktree
3. Add session UUID generation (simple: timestamp + random suffix)
4. Update `broker_reconnected` handler to scan central store
5. On startup, mark active sessions suspended; validate against broker
6. Migration: detect old `context.json`, migrate on first run

### Phase 2 — PTY Log Files
**Goal:** Meaningful resurrection after hard restarts.

**Tee primitive:**
1. Add Rust primitive `pty_tee(session_id, log_path, cap_bytes)` — tees broker output to file; Lua passes `log_path` (derived from session UUID), broker validates it stays within the workspace session directory; runs in broker reader path (not Hub/Lua handlers) so output during Hub downtime is captured
2. Tee state persists in broker session struct; survives Hub reconnects without re-arming
3. File opened once per session with `O_APPEND|O_CREAT`; handle kept in session state
4. `log_path` validated to stay within the session's workspace directory
5. Log rotation: `pty-0.log` (current) → `pty-0.log.1` (previous segment) at `cap_bytes` (default 10MB); max 1 rotation
6. Write failures emit a lifecycle event and mark tee degraded — never crash the broker read loop
7. Store `log_path` in session manifest for diagnosability

**Lua wiring:**
8. Lua calls `pty_tee` on session create, passes log path derived from session UUID

**Resurrection:**
9. On graceful restart (broker alive): use broker snapshot as source of truth — skip log replay to prevent duplicate history
10. On hard restart (orphaned session): replay `pty-0.log.1` then `pty-0.log` into shadow screen
11. Apply manifest PTY dimensions before replay to preserve cursor/layout fidelity
12. Missing log → graceful fallback to broker snapshot if alive, else empty history + `tee_missing` lifecycle event
13. Orphaned sessions show explicit read-only indicator in TUI so users know input is not live

**Tests:** tee survives Hub reconnect, missing log fallback, rotation boundary at cap_bytes, duplicate-replay prevention on graceful restart

### Phase 3 — Workspace Grouping
**Goal:** TUI shows sessions grouped by workspace.

1. Workspace manifest creation (auto-create from issue_number or ad-hoc)
2. Sessions carry `workspace_id`
3. TUI layout: workspace → sessions grouping
4. Workspace status derived from session statuses

### Phase 4 — Agent = Single PTY Rework
**Goal:** Simplify agent concept, enable richer workspace templates.

1. Dissolve current multi-PTY agent composite
2. Each named PTY becomes its own session with a `type` and `role`
3. Workspace template declares required sessions: `[{role: "developer", type: "agent"}, {role: "rails-server", type: "accessory", cmd: "bin/rails s"}]`
4. Hub spawns agents and accessories from template on workspace create

## Cross-Device (Future)

Workspace manifests sync via hub mesh (VPN/WebRTC) — no Rails.

- Workspace ID is stable across devices
- When GitHub issue is the anchor, issue number is the natural dedup key
- Session manifests stay local; workspace roster (which sessions exist) syncs
- PTY logs stay local (too large to sync); remote hubs see session list but not output

Rails only enters for GitHub webhook delivery, same as today.
