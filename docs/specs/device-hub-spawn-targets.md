# Device Hub and Spawn Targets

## Summary

Botster should move from "one hub process per repo/directory" to "one hub process per device".

The device hub owns:

- one local runtime and socket
- one Olm/WebRTC identity
- one browser peer connection per browser identity/tab
- many explicitly admitted spawn targets
- many workspaces and sessions across those targets

This preserves the current `.botster` override behavior while removing the ambient `cwd` assumption from the runtime.

## Goals

1. One Botster hub process per device, not per repo.
2. Preserve existing config behavior: device config plus target-local `.botster/`, with target-local override.
3. Deny by default: no process may spawn in an arbitrary directory unless that path was explicitly admitted as a spawn target.
4. Support both git-backed and plain-directory targets.
5. Treat git capabilities as dynamic properties of a target path, not immutable target type.
6. Keep one WebRTC connection per browser identity/tab and reuse it across all target/workspace/session activity.

## Non-Goals

- Backward compatibility with repo-scoped hub identities or crypto stores.
- Silent migration of old hub IDs, sockets, or E2E relay state.
- A general-purpose file manager.

## Core Model

### Device Hub

A device hub is the single long-lived Botster process on a machine.

It is device-scoped, not repo-scoped:

- starts from the user's home directory
- owns the device-local Unix socket and lock
- owns the device-local crypto identity for browser communication
- manages all admitted spawn targets

The hub process must never derive trust or repo identity from its own `cwd`.

### Spawn Target

A spawn target is an explicitly admitted filesystem root where Botster is allowed to spawn sessions.

The trust boundary is the target path itself, not whether the path is currently a git repo.

Stored target fields:

```json
{
  "id": "tgt_01jabcxyz",
  "name": "trybotster",
  "path": "/Users/exampleuser/Rails/trybotster",
  "enabled": true,
  "created_at": "2026-03-20T20:00:00Z",
  "updated_at": "2026-03-20T20:00:00Z"
}
```

Rules:

- `path` is canonicalized on write.
- one canonical path maps to at most one active target
- disabled targets cannot be used for new spawns
- target admission is explicit and user-driven

### Derived Target Capabilities

Target capabilities are discovered fresh from the filesystem when needed.

They are not the authorization boundary and should not be treated as immutable target type.

Derived fields:

```json
{
  "path": "/Users/exampleuser/Rails/trybotster",
  "is_git_repo": true,
  "repo_root": "/Users/exampleuser/Rails/trybotster",
  "repo_name": "wiedymi/restty",
  "current_branch": "main",
  "default_branch": "main",
  "has_botster_dir": true,
  "supports_worktrees": true
}
```

Examples:

- a plain directory may later become a git repo
- a git repo may gain or lose a `.botster/` directory
- `current_branch` is live state, not persisted truth

## Security Model

### Deny By Default

No spawn path is valid unless it resolves to an admitted spawn target or a derived worktree belonging to that target.

Disallowed:

- spawn from process `cwd`
- spawn from raw path input in normal create-agent flows
- loading target-local `.botster/` from an arbitrary browsed directory
- worktree creation rooted in a non-admitted path

Allowed:

- spawn in the admitted target root
- spawn in worktrees created from an admitted git-backed target
- load `.botster/` from the admitted target root when present

### Enforcement Layer

Security must be enforced in Rust, not only in TUI/browser code.

The UI may prevent bad inputs, but runtime enforcement is authoritative.

Required checks:

1. `target_id` is required for spawn operations.
2. `target_id` resolves to an enabled admitted target.
3. every execution path resolves to a canonical path
4. canonical spawn path is either:
   - equal to target root, or
   - inside a Botster-managed worktree derived from that target

## Config Resolution

Current behavior is correct and should be preserved:

- device config from `~/.botster/`
- target-local config from `{target}/.botster/`
- target-local definitions override device definitions by name

The only change is where `repo_root` comes from.

Old model:

- config resolution uses ambient process repo root

New model:

- config resolution uses the selected target root

This applies to:

- agents
- accessories
- workspace templates
- plugins

For plain directories, `{target}/.botster/` is still valid if present.

## Git Capability Model

Git is a capability, not the definition of a target.

Behavior:

- if target is currently git-backed:
  - detect `current_branch`
  - support branch-aware agent spawning
  - support worktree creation and lookup
  - support repo-scoped GitHub event subscriptions when configured
- if target is not currently git-backed:
  - spawn directly in target root
  - disable branch/worktree UI affordances
  - skip repo-scoped GitHub subscriptions

Git-dependent commands must check capabilities fresh at execution time.

## Workspaces and Sessions

Workspaces and sessions must carry target identity explicitly so reconnect, browser UI, MCP, and restoration never depend on process `cwd`.

### Workspace Manifest

```json
{
  "id": "ws_01jabcxyz",
  "name": "Fix auth bug",
  "target_id": "tgt_01jabcxyz",
  "target_path": "/Users/exampleuser/Rails/trybotster",
  "target_repo": "wiedymi/restty",
  "status": "active",
  "created_at": "2026-03-20T20:00:00Z",
  "updated_at": "2026-03-20T20:00:00Z",
  "metadata": {}
}
```

### Session Manifest

```json
{
  "uuid": "sess_01jabcxyz",
  "workspace_id": "ws_01jabcxyz",
  "target_id": "tgt_01jabcxyz",
  "target_path": "/Users/exampleuser/Rails/trybotster",
  "repo": "wiedymi/restty",
  "branch": "botster-issue-42",
  "worktree_path": "/Users/exampleuser/botster-sessions/restty-botster-issue-42",
  "status": "active",
  "created_at": "2026-03-20T20:00:00Z",
  "updated_at": "2026-03-20T20:00:00Z"
}
```

`repo` may be `null` for non-git targets.

## Hub Identity and Startup

The hub must become device-scoped.

Changes:

- remove repo-path-derived local hub identity
- use a single device hub identity and socket location
- `botster start` should behave the same regardless of launch directory
- the hub process should `chdir` to `HOME` on startup
- `botster attach` should attach to the local device hub by default

This intentionally discards the old "hub per repo" model.

## E2E Encryption and WebRTC

The new architecture should use:

- one Olm identity per device hub
- one `CryptoService` for that hub
- one WebRTC peer connection per browser identity/tab

All activity for all targets, workspaces, and sessions should flow over that shared peer connection.

Reasons:

- lower socket and cleanup pressure
- one stable trust anchor per device hub
- no reason to open separate peer connections per target

This change is allowed to break existing repo-scoped relay identity continuity. No migration is required.

## GitHub Integration

Current GitHub delivery is already repo-scoped.

The device hub must subscribe to GitHub events per admitted git-backed target repo, not per launch directory.

This implies:

- target capability refresh determines which targets are currently repo-backed
- subscription set updates as targets are added, removed, enabled, disabled, or change repo state

## UI Model

Two related but distinct flows are required.

### 1. Spawn Target Picker

Used in create-agent/create-accessory flows.

Shows only admitted targets.

Capabilities shown per target:

- path
- git repo status
- current branch
- `.botster/` presence

No raw path input is allowed here.

### 2. Add Spawn Target Browser

Used to admit new targets.

This is a filesystem browser, but scoped to target admission.

Features:

- browse directories
- inspect candidate path metadata
- detect git status and current branch
- detect `.botster/`
- explicit "Add as spawn target" confirmation

Browsing a path does not admit it.

Admission is a separate confirm step.

## TUI and Web Requirements

Both clients need:

- list admitted targets
- inspect target capabilities
- browse filesystem directories for admission
- add/remove/enable/disable targets
- select target before spawning

The browser does not need to be a general file manager.

## Backend API Shape

Representative operations:

- `spawn_targets.list()`
- `spawn_targets.get(target_id)`
- `spawn_targets.inspect(path)`
- `spawn_targets.add(path, name?)`
- `spawn_targets.update(target_id, attrs)`
- `spawn_targets.disable(target_id)`
- `spawn_targets.remove(target_id)`
- `spawn_targets.list_dir(path)`

Representative command changes:

- `list_configs(target_id)`
- `create_agent(target_id, ...)`
- `create_accessory(target_id, ...)`

`target_id` becomes mandatory for spawn-affecting commands.

## Runtime Rules

### Agent Spawn

1. resolve target from `target_id`
2. inspect capabilities
3. resolve config from device root + target root
4. if git-backed and branch/worktree requested:
   - use git/worktree flow
5. otherwise:
   - spawn directly in target root

### Accessory Spawn

Same target resolution rules as agents.

Accessory spawns never bypass target admission.

### `.botster/` Loading

Only load target-local `.botster/` from the admitted target root.

Never load `.botster/` from arbitrary process `cwd`.

## Agent Manifests

Agent definitions expand from a single initialization script to a directory with structured config:

```
~/.botster/agents/orchestrator/
  initialization      # bash script — what gets typed into the PTY
  manifest.json       # structured metadata
  system_prompt.md    # optional, markdown system prompt
```

### Manifest

```json
{
  "plugins": ["orchestrator", "messaging"]
}
```

- `plugins` — optional whitelist of plugins this agent can access via MCP. Only plugins that are also available at the target level will load (see Plugin Scoping below). If omitted, the agent inherits the full target ceiling. If present, what you declare is what you get. An empty array (`[]`) explicitly opts out of all plugins.

### System Prompt

Agent behavioral instructions delivered as context to the spawned AI session.

Two formats, checked in precedence order:

1. `system_prompt.md` file — wins if present. Markdown, any length.
2. `manifest.json` `"system_prompt"` field — inline string for short instructions.

At spawn time, Botster writes the resolved system prompt into the worktree as `.claude/CLAUDE.md` (or appends to an existing one) before the initialization script runs. The agent never needs to know how it got there.

### Config Layering

Agent manifests follow the same 2-layer merge as other config:

- device manifest (`~/.botster/agents/{name}/manifest.json`) — base
- target-local manifest (`{target}/.botster/agents/{name}/manifest.json`) — overrides per target

System prompts follow the same layering. A target-local `system_prompt.md` overrides the device-level one, allowing per-repo behavioral adjustments without changing the device default.

## Plugin Scoping

Plugins are defined in the device root (`~/.botster/plugins/`). They are not duplicated per target.

Activation is controlled at two levels:

### Target Level — Availability Ceiling

Spawn targets declare which plugins are available for sessions in that target:

```json
{
  "id": "tgt_01jabcxyz",
  "path": "/Users/exampleuser/Rails/trybotster",
  "plugins": ["github", "orchestrator", "messaging"]
}
```

A target with no `plugins` field gets no plugins. Deny-by-default, consistent with spawn target security posture.

### Agent Level — Selection Within Ceiling

Agent manifests optionally declare which plugins the agent wants (see Agent Manifests above). If an agent manifest includes a `plugins` field, the agent receives the intersection of what it requests and what the target makes available. If the agent has no manifest or no `plugins` field, it inherits the full target ceiling — the target remains the security boundary, and the manifest is an additional restriction, not a required gate.

### MCP Tool Resolution

When a session connects to MCP, tools are loaded — not filtered from a global list, but constructed for that session:

```
session_uuid → session manifest → agent_key + target_id
target_id    → target's available plugins (ceiling)
agent_key    → agent manifest's requested plugins (optional)
result       → if agent declares plugins: intersection
               if agent has no plugins field: target ceiling
               if target has no plugins field: nothing (deny-by-default)
```

Tools that don't belong to the session's resolved plugin set are never registered. They don't exist for that session.

### Plugin Lifecycle

Plugins load at hub startup from `~/.botster/plugins/`. They remain loaded in the hub's Lua runtime. The scoping controls which sessions can access their MCP tools, not whether the plugin code is resident in memory.

This means:

- plugin hot-reload continues to work device-wide
- no load/unload churn as targets are selected
- the hub has full plugin capability; sessions get scoped access

## Phased Rollout

### Phase 1: Target Registry and Security Boundary

- add spawn target persistence
- canonicalize and deduplicate paths
- add explicit target admission
- block arbitrary-path spawns

### Phase 2: Device Hub Identity

- replace repo-scoped hub identity with device-scoped identity
- start from `HOME`
- change attach/discovery to target-independent device hub behavior
- let device hub own fresh relay identity

### Phase 3: Target-Aware Runtime

- thread `target_id` through commands and spawn flows
- resolve configs from target root
- persist target metadata in workspace/session manifests

### Phase 4: Agent Manifests and Plugin Scoping

- add `manifest.json` support to agent config directories
- add `system_prompt.md` / inline `system_prompt` resolution
- write resolved system prompt into worktree at spawn time
- add `plugins` field to spawn target registry
- add `plugins` field to agent manifests
- implement MCP tool resolution: session → target ceiling ∩ agent selection
- update config resolver to merge device + target-local manifests

### Phase 5: TUI and Web UX

- add spawn target picker
- add target admission browser
- group sessions by target and workspace
- plugin selection UI in target settings and agent config

### Phase 6: Multi-Repo GitHub Subscriptions

- subscribe per admitted git-backed target repo
- refresh subscriptions when target capabilities change

## Open Questions

1. Should disabling a target only block new sessions, or also block restart/reopen of old sessions?
2. Should removing a target preserve historical workspaces as read-only records?
3. How aggressively should target capability refresh happen:
   - on every relevant command
   - periodically in background
   - both
4. Should the target admission browser allow manual path paste, or only navigation plus confirm?

## Decision

Proceed with a device-scoped hub and explicit spawn targets.

Spawn targets are the filesystem trust boundary.

Git support is dynamic capability, not target identity.
