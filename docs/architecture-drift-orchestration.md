# Architecture Drift Orchestration

This document tracks the current cleanup program for keeping Botster aligned
with its first-principles architecture.

## North Star

Botster is a local-first PTY workspace platform with hot-reloadable Lua
product behavior and equal clients. The stable split is:

- Hub owns control-plane policy, lifecycle, routing, pairing, and recovery.
- Session I/O workers own per-session terminal data-plane mechanics.
- Client workers own transport-neutral stream state for browser, TUI, socket,
  and future clients.
- Lua plugins own product behavior, integrations, entity publication, and UI
  composition.
- Browser and TUI render the same entity-backed semantic UI contract.
- Rails owns auth, registry, billing, pairing/signaling, and the browser shell.

## Active Slices

### Hub Boundary Cleanup

Goal: remove current-facing wording that implies the Hub owns all state or
routes terminal bytes as a hot-path relay.

Scope:

- Current docs and module comments.
- Session protocol docs where "Session -> Hub" means daemon wire endpoint, not
  hub event-loop ownership.
- Worker contract diagrams that should show ClientWorker and SessionIo as the
  terminal data-plane path.

Verification:

```bash
cd cli && ./test.sh --check
```

### Plugin Entity State

Goal: move plugin-owned durable model state toward entity families plus generic
renderer bindings.

Current rule:

- Durable plugin state uses `entity_snapshot`, `entity_upsert`,
  `entity_patch`, and `entity_remove`.
- `ui_tree_snapshot` carries route, presentation, and control structure only.
- Plugin UI uses `ui.bind` and `ui.bind_list` to read entity records.
- Browser and TUI must not grow plugin-specific model stores.

Known migration targets:

- Project Pipelines screens that read repo rows during render and embed model
  data into `ui_tree_snapshot`.
- Plugin-local browser/subscription state such as form drafts and feedback.
- TUI `_tui_state` projections that still mirror entity-backed data for legacy
  Lua actions and layout.

### Device Hub Residue

Goal: remove current-facing residue from the old repo-scoped hub model.

Current rule:

- Botster runs one device hub per machine.
- Spawn authority comes from explicit spawn targets.
- Runtime trust and hub identity must not derive from process `cwd`.
- Repo/git state is a live capability of an admitted target, not a hub
  identity boundary.

Compatibility helpers may remain when needed, but they should be named and
documented as compatibility or migration support rather than current product
model.

## Integration Rules

- Keep slices small and independently committed.
- Use `cd cli && ./test.sh ...` for CLI verification.
- Prefer docs/comments for framing cleanup before behavioral refactors.
- Avoid broad compatibility layers; when ambiguity itself is the problem, make
  the current architecture the only current-facing story.
- Use explicit `agent_name`, `workspace_id`, and `target_id` when spawning
  Botster agents in multi-target hubs; target-name routing can be ambiguous
  when orchestration spans many active workspaces.
- Prompts for spawned agents must name the assigned worktree path, not the
  orchestrator's main checkout path, so each slice lands on its intended
  branch.
