# Web UI Primitives Runtime

## Goal

Replace the old browser-side "template cloning plus Stimulus reconciliation" approach for live operator surfaces with a Rails-owned React/Catalyst runtime that renders trusted Botster primitives from structured data.

This spec is the web renderer application of the shared contract in [cross-client-ui-primitives.md](cross-client-ui-primitives.md). The cross-client spec owns primitive names, shared action semantics, and renderer-neutral state ownership rules. This document only defines the web-specific rollout and the phase-1 adapter boundary.

Adaptive viewport behavior is specified separately in [adaptive-ui-viewport-and-presentation.md](adaptive-ui-viewport-and-presentation.md). That viewport-aware work is a phase-2 follow-on for the web runtime, not part of the initial phase-1 React island.

This is the web equivalent of the TUI model:

- Rails owns the trusted runtime, primitive registry, styling, and accessibility behavior
- hub/Lua owns authoritative state and declarative composition
- the browser renders locally and emits structured actions

Botster's operator frontend is a React/Catalyst application hosted by Rails. Rails still owns authentication, persistence, and HTTP endpoints, but live hub surfaces, settings workflows, and plugin-defined UI should use the React runtime and the hub collection/event contract instead of Turbo, Stimulus, or server-rendered HTML fragments.

## Problem

The current `agent_list_controller.js` now owns too much application logic:

- websocket-driven state
- workspace grouping
- selection syncing
- per-row activity derivation
- hosted preview lifecycle
- duplicate row rendering in sidebar and main panel
- manual DOM reconciliation through cloned templates and `data-*` mutation

That is the wrong shape for Stimulus. The next runtime must render from normalized state instead of progressively mutating the DOM.

## Decision Summary

- React is the browser runtime for live operator surfaces
- Catalyst/Tailwind components are the browser component system
- React Query owns browser request caches and loading/error states
- Zustand owns local UI state plus hub-pushed entity collections
- Rails owns the primitive/component registry
- hub/Lua does not send HTML, CSS, or JavaScript
- phase 1 stays state-first, not tree-first
- Botster composites exist for phase 1, but they are runtime-owned, not Lua-public

That last point matters: phase 1 proves the React island using the existing hub state feed. It does not simultaneously ship a generic schema renderer for arbitrary Lua-authored web trees.

## Versioning

This document defines `web-ui-runtime/v1`.

Rules:

- `v1` additions must be backward-compatible
- removing props or changing action payload semantics requires `v2`
- phase-1 implementation may keep adapter code internal as long as the contracts below remain stable

## Phase 1 Boundary

Phase 1 originally covered only the agent/workspace UI:

- sidebar workspace tree
- main hub workspace list
- shared session row logic
- hosted preview indicator and error state
- row actions menu

Later phases move the remaining frontend surfaces onto the same runtime. The React/Catalyst direction explicitly does not include:

- Turbo/Stimulus compatibility paths for hub-owned UI
- server-rendered HTML fragments for live hub surfaces
- duplicate browser renderers for the same hub-owned surface
- connection-code cards in plugin/layout surfaces; pairing URLs are requested and shown only by the React `Share` modal
- request caches implemented in Zustand
- arbitrary user-authored browser composition

Settings/forms remain Rails-authenticated React/Catalyst surfaces until a shared Lua form contract exists.

## Browser State Ownership

The browser has three state classes:

- Hub-pushed collections and events: normalized into Zustand entity stores from the shared hub connection.
- Request/response data: loaded through React Query. This includes `/hubs.json`, `/hubs/:id/settings.json`, and target-scoped agent/accessory config discovery.
- Pure UI state: kept in Zustand or component-local state when it is not remote data.

Rules:

- Components do not call ad hoc `fetch()` for cacheable remote data. Add a query in `app/frontend/lib/queries.js`.
- Components do not add getter-style request caches to hub sessions. The hub session may expose transport commands; React Query owns request lifecycle, dedupe, stale state, and invalidation.
- Settings mutations that change agent/accessory config invalidate the matching React Query keys instead of forcing a legacy hub cache refresh.
- Loading UI must describe unknown/pending state as loading. Empty or "not configured" states render only after the query resolves successfully.
- Hub UI uses the single shared hub connection acquired through `hub-bridge`; React is not a privileged second client and must use the same wire-format events and collections as other clients.

## Contract Layers

There are three separate contracts in `v1`.

### 1. Hub transport contract for phase 1

The hub continues to send the existing state-oriented payloads. The React island normalizes them locally.

### 2. Rails-owned primitive registry

Rails defines the stable browser primitive inventory and prop schemas. This is the future Lua-facing surface area.

### 3. Internal phase-1 composites

The first React island uses Botster-specific composites for the workspace/session surface. These are stable within the Rails runtime, but are not public to Lua in phase 1.

## Phase 1 Transport Contract

The React island should adapt the current hub payload, not invent a second transport.

### Hub input shape

```ts
type HostedPreviewStateV1 = {
  status?: "inactive" | "starting" | "running" | "error"
  url?: string | null
  error?: string | null
  install_url?: string | null
}

type SessionSummaryV1 = {
  id: string
  session_uuid: string
  session_type?: "agent" | "accessory" | string | null
  label?: string | null
  display_name?: string | null
  title?: string | null
  task?: string | null
  target_name?: string | null
  branch_name?: string | null
  agent_name?: string | null
  profile_name?: string | null
  notification?: boolean
  is_idle?: boolean | null
  port?: number | null
  hosted_preview?: HostedPreviewStateV1 | null
  in_worktree?: boolean | null
}

type OpenWorkspaceSummaryV1 = {
  id: string
  name?: string | null
  agents?: string[]
}

type AgentWorkspaceSurfaceInputV1 = {
  hub_id: string
  agents: SessionSummaryV1[]
  open_workspaces: OpenWorkspaceSummaryV1[]
  selected_session_uuid?: string | null
  surface: "sidebar" | "panel"
}
```

### Normalized browser store

The runtime should normalize that input into a store shaped like:

```ts
type AgentWorkspaceStoreV1 = {
  sessionsById: Record<string, SessionSummaryV1>
  sessionOrder: string[]
  workspacesById: Record<string, {
    id: string
    title: string
    sessionIds: string[]
  }>
  workspaceOrder: string[]
  ungroupedSessionIds: string[]
  selectedSessionId: string | null
  collapsedWorkspaceIds: string[]
  surface: "sidebar" | "panel"
}
```

Rules:

- hub data remains the single remote source of truth
- `collapsedWorkspaceIds` is browser-local UI state
- selection is derived from route plus hub state, then stored as `selectedSessionId`
- runtime selectors derive display names, title lines, preview affordances, and row density

## Primitive Inventory

`v1` intentionally exposes a small primitive set. Anything not listed here is out of scope for `v1`.

| Category | Component | `v1` status | Lua public in `v1` |
|---|---|---|---|
| Foundation | `Stack` | supported | yes |
| Foundation | `Inline` | supported | yes |
| Foundation | `Panel` | supported | yes |
| Foundation | `ScrollArea` | supported | yes |
| Content | `Text` | supported | yes |
| Content | `Icon` | supported | yes |
| Content | `Badge` | supported | yes |
| Content | `StatusDot` | supported | yes |
| Content | `EmptyState` | supported | yes |
| Actions | `Button` | supported | yes |
| Actions | `IconButton` | supported | yes |
| Actions | `Menu` | supported | no |
| Actions | `MenuItem` | supported | no |
| Navigation | `Tree` | supported | yes |
| Navigation | `TreeItem` | supported | yes |
| Botster composite | `WorkspaceList` | supported | no |
| Botster composite | `WorkspaceGroup` | supported | no |
| Botster composite | `SessionRow` | supported | no |
| Botster composite | `HostedPreviewIndicator` | supported | no |
| Botster composite | `HostedPreviewError` | supported | no |
| Botster composite | `SessionActionsMenu` | supported | no |

Deferred from `v1`:

- `Grid`
- `Separator`
- `Spacer`
- `Heading`
- `Code`
- `LinkButton`
- `Disclosure`
- `Dialog`
- `Tooltip`
- form primitives
- `Tabs`

## Shared Schema Types

The public registry uses these shared scalar types.

`Density` in this web runtime spec is a phase-1 surface variant for shared workspace and session components. It is intentionally separate from the shared cross-client `UiInteractionDensityV1` token defined in [cross-client-ui-primitives.md](cross-client-ui-primitives.md).

```ts
type Space = "0" | "1" | "2" | "3" | "4" | "6"
type Density = "sidebar" | "panel"
type Tone = "default" | "muted" | "accent" | "success" | "warning" | "danger"
type NodeV1 = {
  type: string
  props: Record<string, unknown>
}

type ActionBindingV1 = {
  id:
    | "botster.workspace.toggle"
    | "botster.workspace.rename.request"
    | "botster.session.select"
    | "botster.session.preview.toggle"
    | "botster.session.preview.open"
    | "botster.session.move.request"
    | "botster.session.delete.request"
  payload: Record<string, unknown>
  disabled?: boolean
}
```

`Density` in this web runtime spec is a phase-1 surface variant for shared workspace/session components. It is intentionally separate from the shared cross-client `UiInteractionDensityV1` token defined in [cross-client-ui-primitives.md](cross-client-ui-primitives.md).

## Lua-Public Primitive Props

These are the exact public prop shapes for the `v1` primitive registry.

### `Stack`

```ts
type StackPropsV1 = {
  gap?: Space
  padding?: Space
  align?: "start" | "center" | "end" | "stretch"
  justify?: "start" | "center" | "end" | "between"
  children: NodeV1[]
}
```

### `Inline`

```ts
type InlinePropsV1 = {
  gap?: Space
  padding?: Space
  align?: "start" | "center" | "end" | "stretch"
  justify?: "start" | "center" | "end" | "between"
  wrap?: boolean
  children: NodeV1[]
}
```

### `Panel`

```ts
type PanelPropsV1 = {
  padding?: Space
  tone?: "default" | "muted"
  border?: boolean
  radius?: "sm" | "md"
  children: NodeV1[]
}
```

### `ScrollArea`

```ts
type ScrollAreaPropsV1 = {
  axis?: "y" | "x" | "both"
  children: NodeV1[]
}
```

### `Text`

```ts
type TextPropsV1 = {
  text: string
  size?: "xs" | "sm" | "md"
  tone?: Tone
  weight?: "regular" | "medium" | "semibold"
  italic?: boolean
  truncate?: boolean
  monospace?: boolean
}
```

### `Icon`

```ts
type IconPropsV1 = {
  name: string
  size?: "xs" | "sm" | "md"
  tone?: Tone
  label?: string
}
```

### `Badge`

```ts
type BadgePropsV1 = {
  text: string
  tone?: "default" | "accent" | "success" | "warning" | "danger"
  size?: "sm" | "md"
}
```

### `StatusDot`

```ts
type StatusDotPropsV1 = {
  state: "neutral" | "idle" | "active" | "success" | "warning" | "danger"
  label?: string
}
```

### `EmptyState`

```ts
type EmptyStatePropsV1 = {
  title: string
  description?: string
  icon?: string
  primaryAction?: ActionBindingV1
}
```

### `Button`

```ts
type ButtonPropsV1 = {
  label: string
  action: ActionBindingV1
  variant?: "solid" | "ghost"
  tone?: "default" | "accent" | "danger"
  leadingIcon?: string
  disabled?: boolean
}
```

### `IconButton`

```ts
type IconButtonPropsV1 = {
  icon: string
  label: string
  action: ActionBindingV1
  tone?: "default" | "accent" | "danger"
  disabled?: boolean
}
```

### `Tree`

```ts
type TreePropsV1 = {
  // Web-only phase-1 surface variant, not the shared interaction-density token.
  density: Density
  children: NodeV1[]
}
```

### `TreeItem`

```ts
type TreeItemPropsV1 = {
  id: string
  selected?: boolean
  notification?: boolean
  action?: ActionBindingV1
  startSlot?: NodeV1[]
  title: NodeV1[]
  subtitle?: NodeV1[]
  endSlot?: NodeV1[]
}
```

## Internal Phase-1 Composite Contract

These composites are runtime-owned in `v1`. They are stable enough to build the React island, but they are not exposed to Lua until the state-first migration has landed cleanly.

### `WorkspaceList`

```ts
type WorkspaceListPropsV1 = {
  density: Density
  groups: WorkspaceGroupPropsV1[]
  ungroupedSessions?: SessionRowPropsV1[]
  emptyState?: EmptyStatePropsV1
}
```

Emits: none directly. Child composites emit the actions.

### `WorkspaceGroup`

```ts
type WorkspaceGroupPropsV1 = {
  id: string
  title: string
  count: number
  expanded: boolean
  density: Density
  canRename: boolean
  sessions: SessionRowPropsV1[]
}
```

Emits:

- `botster.workspace.toggle` with `{ workspaceId }`
- `botster.workspace.rename.request` with `{ workspaceId, currentName }`

### `SessionRow`

```ts
type SessionRowPropsV1 = {
  sessionId: string
  sessionUuid: string
  density: Density
  primaryName: string
  titleLine?: string
  subtext: string
  selected: boolean
  notification: boolean
  sessionType: "agent" | "accessory"
  activityState: "hidden" | "idle" | "active"
  hostedPreview?: HostedPreviewIndicatorPropsV1 | null
  previewError?: HostedPreviewErrorPropsV1 | null
  actionsMenu: SessionActionsMenuPropsV1
  canMoveWorkspace: boolean
  canDelete: boolean
  inWorktree?: boolean | null
}
```

Emits:

- `botster.session.select` with `{ sessionId, sessionUuid }`

### `HostedPreviewIndicator`

```ts
type HostedPreviewIndicatorPropsV1 = {
  sessionId: string
  sessionUuid: string
  hasForwardedPort: boolean
  status: "inactive" | "starting" | "running" | "error" | "unavailable"
  url?: string | null
  error?: string | null
  installUrl?: string | null
}
```

Emits:

- `botster.session.preview.open` with `{ sessionId, sessionUuid, url }` when `status === "running"` and `url` is present

### `HostedPreviewError`

```ts
type HostedPreviewErrorPropsV1 = {
  sessionId: string
  sessionUuid: string
  visible: boolean
  message: string
  installUrl?: string | null
}
```

Emits:

- no hub action
- optional browser navigation to `installUrl`

### `SessionActionsMenu`

```ts
type SessionActionsMenuPropsV1 = {
  sessionId: string
  sessionUuid: string
  hasForwardedPort: boolean
  previewStatus: "inactive" | "starting" | "running" | "error" | "unavailable"
  previewUrl?: string | null
  previewError?: string | null
  canMoveWorkspace: boolean
  canDelete: boolean
  inWorktree?: boolean | null
}
```

Emits:

- `botster.session.preview.toggle` with `{ sessionId, sessionUuid }`
- `botster.session.preview.open` with `{ sessionId, sessionUuid, url }`
- `botster.session.move.request` with `{ sessionId, sessionUuid }`
- `botster.session.delete.request` with `{ sessionId, sessionUuid, inWorktree }`

## Action Contract

These action ids are the only user-intent events the phase-1 composites may emit.

| Action id | Payload | Phase-1 adapter behavior |
|---|---|---|
| `botster.workspace.toggle` | `{ workspaceId }` | local UI state only |
| `botster.workspace.rename.request` | `{ workspaceId, currentName }` | open rename UI, then call `hub.renameWorkspace` |
| `botster.session.select` | `{ sessionId, sessionUuid }` | navigate/select, then call `hub.selectAgent` |
| `botster.session.preview.toggle` | `{ sessionId, sessionUuid }` | call `hub.toggleHostedPreview(sessionUuid)` |
| `botster.session.preview.open` | `{ sessionId, sessionUuid, url }` | browser navigation only |
| `botster.session.move.request` | `{ sessionId, sessionUuid }` | open move UI, then call `hub.moveAgentWorkspace` |
| `botster.session.delete.request` | `{ sessionId, sessionUuid, inWorktree }` | open delete UI, then call `hub.deleteAgent` |

Rules:

- action ids are semantic Botster events, not DOM event names
- local UI actions and hub-routed actions share one action envelope shape
- phase 1 may still open Rails-owned modals or prompts for rename, move, and delete

## Density Model

The sidebar and main panel must share component logic and differ only by density.

Allowed densities in `v1`:

- `sidebar` — compact row height, hover-revealed actions, tighter typography
- `panel` — larger card-like row layout, always-present affordances where appropriate

Any variant beyond those two is out of scope for `v1`.

## Why Composites Stay Internal In Phase 1

`WorkspaceGroup`, `SessionRow`, and the preview/menu composites encode a lot of current product behavior:

- session naming fallback rules
- activity indicator derivation
- accessory-vs-agent affordances
- hosted preview state mapping
- action availability rules

That behavior is still moving. Freezing it into the Lua contract before the React island lands would lock Botster into premature APIs. The public `v1` Lua surface should therefore stop at primitives, while phase 1 uses internal composites as the migration bridge.

## Acceptance Criteria For Phase 1

- the agent/workspace UI no longer depends on cloned HTML templates for row rendering
- sidebar and main panel share the same row/group logic with density variants
- hosted preview indicator, preview error state, and actions menu render from normalized state
- hub transport remains structured state, not HTML
- Rails continues to own the page shell and primitive registry
- the action vocabulary above is sufficient to reproduce the current session/workspace behavior

## Immediate Implementation Sequence

1. Build the React island adapter around `AgentWorkspaceSurfaceInputV1`
2. Normalize state into `AgentWorkspaceStoreV1`
3. Implement the internal composites above in `sidebar` and `panel` densities
4. Map the action ids above onto the existing hub transport methods
5. Remove template cloning from `agent_list_controller.js`

The next contract after this one is not "more React." It is a separate spec for when the Lua-authored node tree becomes public beyond the internal phase-1 adapter.
