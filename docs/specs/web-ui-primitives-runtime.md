# Web UI Primitives Runtime

## Goal

Define the Rails-owned React/Catalyst runtime that renders trusted Botster
primitives from structured data.

This spec is the web renderer application of the shared contract in [cross-client-ui-primitives.md](cross-client-ui-primitives.md). The cross-client spec owns primitive names, shared action semantics, and renderer-neutral state ownership rules. This document defines the web runtime boundary and the internal workspace/session adapter.

Adaptive viewport behavior is specified separately in [adaptive-ui-viewport-and-presentation.md](adaptive-ui-viewport-and-presentation.md). That viewport-aware work extends the web runtime without changing the entity-backed plugin UI model.

This is the web equivalent of the TUI model:

- Rails owns the trusted runtime, primitive registry, styling, and accessibility behavior
- hub/Lua owns authoritative state and declarative composition
- the web client renders locally and emits structured actions

Botster's operator frontend is a React/Catalyst application hosted by Rails. Rails still owns authentication, persistence, and HTTP endpoints, but live hub surfaces, settings workflows, and plugin-defined UI should use the React runtime and the hub collection/event contract instead of Turbo, Stimulus, or server-rendered HTML fragments.

UI contract work should be classified by the rendered surface. Lua-authored
primitive trees use the React/Catalyst primitive registry on the web client;
they are not Hotwire/Elements work and should not add Rails-rendered fragments
or Stimulus reconciliation paths. Consult `tmp/tailwind_plus_preview` only when
the change introduces or restyles visible Catalyst primitives; state binding and
entity-store work should preserve the existing primitive presentation.
Collection primitives follow the same split: web `table` renders through
`app/frontend/components/catalyst/table.jsx`, while `list`, `list_item`, `tree`,
and `tree_item` render through the shared primitive registry. TUI renderers map
the same nodes to ratatui widgets. Do not add plugin-specific browser or TUI
renderers when a Lua-authored surface can express the view with these shared
primitives and `$bind` / `bind_list`.

Lua-authored trees may contain `$bind` and `bind_list` sentinels. The web
runtime resolves them from the same Zustand entity stores that consume
`entity_snapshot`, `entity_upsert`, `entity_patch`, and `entity_remove`.
Plugin entity types use the existing `<plugin>.<type>` store key directly, so
`/project-pipelines.ticket` expands a ticket list and
`/project-pipelines.ticket/ticket_123/title` resolves one scalar field. A
`bind_list` can add `where: { field: value }` to filter records by exact
top-level field matches before expanding the row template. A tree containing
bindings subscribes to the referenced entity stores, allowing an `entity_patch`
to update bound text without another `ui_tree_snapshot`.

Lua-authored action submitters use the generic `ui_action` lifecycle in the
React/Catalyst primitive registry. The web renderer generates
`action_request_id`, disables only the activating button or icon button while
pending, renders the pending affordance from local Catalyst/Tailwind classes,
and then renders semantic success/error text plus optional navigation returned
by `ui_action_result`. Do not reintroduce plugin-specific pending keys,
per-screen notice panels, or Lua-provided CSS/classes for this feedback path.

When this lifecycle changes visible button, icon button, or result treatment,
inspect `tmp/tailwind_plus_preview` if reference material exists; otherwise use
the vendored Catalyst primitives and existing registry styles. This remains
React/Catalyst web work, not Elements/Hotwire/Stimulus work. A Hotwire surface
would need its own adapter later, with Stimulus limited to behavior/data
attributes and styling still owned by the applicable UI layer.

## Problem

Live operator surfaces need normalized state and structured primitive rendering.
Controller-driven DOM mutation concentrates too much application logic in the
browser layer:

- websocket-driven state
- workspace grouping
- selection syncing
- per-row activity derivation
- session action lifecycle
- duplicate row rendering in sidebar and main panel

That is the wrong shape for Stimulus. The next runtime must render from normalized state instead of progressively mutating the DOM.

## Decision Summary

- React is the web client runtime for live operator surfaces
- Catalyst/Tailwind components are the web component system
- React Query owns client request caches and loading/error states
- Zustand owns local UI state plus hub-pushed entity collections
- Rails owns the primitive/component registry
- hub/Lua does not send HTML, CSS, or JavaScript
- workspace/session composites are runtime-owned, not Lua-public
- Lua-authored plugin surfaces use shared primitives plus generic entity bindings

That last point matters: workspace/session composites encode product-specific
Botster behavior. Plugin authors should use the generic primitive and binding
contract instead of depending on those internal composites.

## Versioning

This document defines `web-ui-runtime`.

Rules:

- `current` additions must be backward-compatible
- removing props or changing action payload semantics requires a new contract
- workspace/session implementation may keep adapter code internal as long as the contracts below remain stable

## Current Boundary

The first runtime boundary covered the agent/workspace UI:

- sidebar workspace tree
- main hub workspace list
- shared session row logic
- session action indicator and error state
- row actions menu

The React/Catalyst direction explicitly does not include:

- Turbo/Stimulus compatibility paths for hub-owned UI
- server-rendered HTML fragments for live hub surfaces
- duplicate client renderers for the same hub-owned surface
- connection-code cards in plugin/layout surfaces; pairing URLs are requested and shown only by the React `Share` modal
- request caches implemented in Zustand
- arbitrary user-authored web client composition outside trusted Lua primitive
  trees and the shared binding grammar

Settings/forms remain Rails-authenticated React/Catalyst surfaces. Lua-authored
plugin surfaces use the shared form primitives when they need entity-backed
plugin forms.

## Client State Ownership

The web client has three state classes:

- Hub-pushed collections and events: normalized into Zustand entity stores from the shared hub connection.
- Request/response data: loaded through React Query. This includes `/hubs.json`, `/hubs/:id/settings.json`, and target-scoped agent/accessory config discovery.
- Pure UI state: kept in Zustand or component-local state when it is not remote data.

Rules:

- Components do not call ad hoc `fetch()` for cacheable remote data. Add a query in `app/frontend/lib/queries.js`.
- Components do not add getter-style request caches to hub sessions. The hub session may expose transport commands; React Query owns request lifecycle, dedupe, stale state, and invalidation.
- Settings mutations that change agent/accessory config invalidate the matching React Query keys instead of forcing a hub cache refresh.
- Loading UI must describe unknown/pending state as loading. Empty or "not configured" states render only after the query resolves successfully.
- Hub UI uses the single shared hub connection acquired through `hub-bridge`; React is not a privileged second client and must use the same wire-format events and collections as other clients.
- Plugin-owned durable state must use the generic entity path. Unknown
  namespaced entity types such as `project-pipelines.ticket` are accepted by
  the browser entity store registry with `id` as the record key; Project
  Pipelines and other plugins must not add plugin-specific Zustand stores,
  hooks, reducers, or client-local snapshots for that state.
- Browser consumers that need plugin entity data should use the generic
  selectors in `app/frontend/lib/entity-selectors.js`:
  `selectEntityList({ entityType, hubId? })`,
  `selectEntity({ entityType, id, hubId? })`, and
  `selectEntityField({ entityType, id, field, hubId? })`. The optional
  `hubId` filters returned records but does not partition the underlying
  single connected-hub stores.

## Contract Layers

There are three separate contracts in `current`.

### 1. Hub transport contract

The hub continues to send the existing state-oriented payloads. The React island normalizes them locally.

### 2. Rails-owned primitive registry

Rails defines the stable web primitive inventory and prop schemas. This is the future Lua-facing surface area.

### 3. Internal workspace/session composites

The workspace/session surface uses Botster-specific composites inside the Rails
runtime. Lua-authored plugin surfaces use the public primitive set and generic
entity bindings instead.

## Workspace Transport Contract

The React island should adapt the current hub payload, not invent a second transport.

### Hub input shape

```ts
type SessionActionState = {
  status?: "inactive" | "starting" | "running" | "error"
  url?: string | null
  error?: string | null
  install_url?: string | null
}

type SessionSummary = {
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
  notification?: boolean
  output_activity?: "active" | "idle" | null
  port?: number | null
  plugin_state?: SessionActionState | null
  in_worktree?: boolean | null
}

type OpenWorkspaceSummary = {
  id: string
  name?: string | null
  agents?: string[]
}

type AgentWorkspaceSurfaceInput = {
  hub_id: string
  agents: SessionSummary[]
  open_workspaces: OpenWorkspaceSummary[]
  selected_session_uuid?: string | null
  surface: "sidebar" | "panel"
}
```

### Normalized client store

The runtime should normalize that input into a store shaped like:

```ts
type AgentWorkspaceStore = {
  sessionsById: Record<string, SessionSummary>
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
- `collapsedWorkspaceIds` is client-local UI state
- selection is derived from route plus hub state, then stored as `selectedSessionId`
- runtime selectors derive display names, title lines, preview affordances, and row density

## Primitive Inventory

`current` intentionally exposes a small primitive set. Anything not listed here is out of scope for `current`.

| Category | Component | `current` status | Lua public in `current` |
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
| Collections | `List` | supported | yes |
| Collections | `ListItem` | supported | yes |
| Collections | `Table` | supported | yes |
| Navigation | `Tree` | supported | yes |
| Navigation | `TreeItem` | supported | yes |
| Botster composite | `WorkspaceList` | supported | no |
| Botster composite | `WorkspaceGroup` | supported | no |
| Botster composite | `SessionRow` | supported | no |
| Botster composite | `SessionActionIndicator` | supported | no |
| Botster composite | `SessionActionError` | supported | no |
| Botster composite | `SessionActionsMenu` | supported | no |

Deferred from `current`:

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

`Density` in this web runtime spec is a workspace/session surface variant. It is intentionally separate from the shared cross-client `UiInteractionDensity` token defined in [cross-client-ui-primitives.md](cross-client-ui-primitives.md).

```ts
type Space = "0" | "1" | "2" | "3" | "4" | "6"
type Density = "sidebar" | "panel"
type Tone = "default" | "muted" | "accent" | "success" | "warning" | "danger"
type Node = {
  type: string
  props: Record<string, unknown>
}

type ActionBinding = {
  id:
    | "botster.workspace.toggle"
    | "botster.workspace.rename.request"
    | "botster.session.select"
    | "botster.session.action.execute"
    | "botster.url.open"
    | "botster.session.move.request"
    | "botster.session.delete.request"
  payload: Record<string, unknown>
  disabled?: boolean
}
```

`Density` in this web runtime spec is a workspace/session surface variant. It is intentionally separate from the shared cross-client `UiInteractionDensity` token defined in [cross-client-ui-primitives.md](cross-client-ui-primitives.md).

## Lua-Public Primitive Props

These are the exact public prop shapes for the `current` primitive registry.

### `Stack`

```ts
type StackProps = {
  gap?: Space
  padding?: Space
  align?: "start" | "center" | "end" | "stretch"
  justify?: "start" | "center" | "end" | "between"
  children: Node[]
}
```

### `Inline`

```ts
type InlineProps = {
  gap?: Space
  padding?: Space
  align?: "start" | "center" | "end" | "stretch"
  justify?: "start" | "center" | "end" | "between"
  wrap?: boolean
  children: Node[]
}
```

### `Panel`

```ts
type PanelProps = {
  padding?: Space
  tone?: "default" | "muted"
  border?: boolean
  radius?: "sm" | "md"
  children: Node[]
}
```

### `ScrollArea`

```ts
type ScrollAreaProps = {
  axis?: "y" | "x" | "both"
  children: Node[]
}
```

### `List`

```ts
type ListProps = {}
```

`List` gives both web and TUI renderers a native collection boundary. Its rows
are `ListItem` children, often expanded from `ui.bind_list`.

### `ListItem`

```ts
type ListItemProps = {
  selected?: boolean
  notification?: boolean
  action?: ActionBinding
}
```

Required slot:

- `title`

Optional slots:

- `subtitle`
- `start`
- `end`
- `detail`

### `Table`

```ts
type TableColumnProps = {
  key: string
  label: string
}

type TableProps = {
  columns?: TableColumnProps[]
  rows?: Record<string, unknown>[]
}
```

`Table` is the shared primitive for read-oriented tabular plugin data. Web uses
the Catalyst table components; the TUI uses ratatui `Table`. Keep row data in
the generic entity stores and bind it directly, for example
`rows = ui.bind("/project-pipelines.run")`, instead of creating plugin-specific
client state.

### `Text`

```ts
type TextProps = {
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
type IconProps = {
  name: string
  size?: "xs" | "sm" | "md"
  tone?: Tone
  label?: string
}
```

### `Badge`

```ts
type BadgeProps = {
  text: string
  tone?: "default" | "accent" | "success" | "warning" | "danger"
  size?: "sm" | "md"
}
```

### `StatusDot`

```ts
type StatusDotProps = {
  state: "neutral" | "idle" | "active" | "success" | "warning" | "danger"
  label?: string
}
```

### `EmptyState`

```ts
type EmptyStateProps = {
  title: string
  description?: string
  icon?: string
  primaryAction?: ActionBinding
}
```

### `Button`

```ts
type ButtonProps = {
  label: string
  action: ActionBinding
  variant?: "solid" | "ghost"
  tone?: "default" | "accent" | "danger"
  leadingIcon?: string
  disabled?: boolean
}
```

### `IconButton`

```ts
type IconButtonProps = {
  icon: string
  label: string
  action: ActionBinding
  tone?: "default" | "accent" | "danger"
  disabled?: boolean
}
```

### `Tree`

```ts
type TreeProps = {
  // Web-only workspace/session surface variant, not the shared interaction-density token.
  density: Density
  children: Node[]
}
```

### `TreeItem`

```ts
type TreeItemProps = {
  id: string
  selected?: boolean
  notification?: boolean
  action?: ActionBinding
  startSlot?: Node[]
  title: Node[]
  subtitle?: Node[]
  endSlot?: Node[]
}
```

## Internal Workspace/Session Composite Contract

These composites are runtime-owned in `current`. They are not exposed to Lua;
Lua-authored plugin surfaces should use shared primitives, `ui.bind`, and
`ui.bind_list`.

### `WorkspaceList`

```ts
type WorkspaceListProps = {
  density: Density
  groups: WorkspaceGroupProps[]
  ungroupedSessions?: SessionRowProps[]
  emptyState?: EmptyStateProps
}
```

Emits: none directly. Child composites emit the actions.

### `WorkspaceGroup`

```ts
type WorkspaceGroupProps = {
  id: string
  title: string
  count: number
  expanded: boolean
  density: Density
  canRename: boolean
  sessions: SessionRowProps[]
}
```

Emits:

- `botster.workspace.toggle` with `{ workspaceId }`
- `botster.workspace.rename.request` with `{ workspaceId, currentName }`

### `SessionRow`

```ts
type SessionRowProps = {
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
  sessionAction?: SessionActionIndicatorProps | null
  actionError?: SessionActionErrorProps | null
  actionsMenu: SessionActionsMenuProps
  canMoveWorkspace: boolean
  canDelete: boolean
  inWorktree?: boolean | null
}
```

Emits:

- `botster.session.select` with `{ sessionId, sessionUuid }`

### `SessionActionIndicator`

```ts
type SessionActionIndicatorProps = {
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

- `botster.url.open` with `{ sessionId, sessionUuid, url }` when `status === "running"` and `url` is present

### `SessionActionError`

```ts
type SessionActionErrorProps = {
  sessionId: string
  sessionUuid: string
  visible: boolean
  message: string
  installUrl?: string | null
}
```

Emits:

- no hub action
- optional web client navigation to `installUrl`

### `SessionActionsMenu`

```ts
type SessionActionsMenuProps = {
  sessionId: string
  sessionUuid: string
  hasForwardedPort: boolean
  previewStatus: "inactive" | "starting" | "running" | "error" | "unavailable"
  previewUrl?: string | null
  actionError?: string | null
  canMoveWorkspace: boolean
  canDelete: boolean
  inWorktree?: boolean | null
}
```

Emits:

- `botster.session.action.execute` with `{ sessionId, sessionUuid }`
- `botster.url.open` with `{ sessionId, sessionUuid, url }`
- `botster.session.move.request` with `{ sessionId, sessionUuid }`
- `botster.session.delete.request` with `{ sessionId, sessionUuid, inWorktree }`

## Workspace/Session Action Contract

These action ids are the only user-intent events the workspace/session
composites may emit.

| Action id | Payload | Adapter behavior |
|---|---|---|
| `botster.workspace.toggle` | `{ workspaceId }` | local UI state only |
| `botster.workspace.rename.request` | `{ workspaceId, currentName }` | open rename UI, then call `hub.renameWorkspace` |
| `botster.session.select` | `{ sessionId, sessionUuid }` | navigate/select, then call `hub.selectAgent` |
| `botster.session.action.execute` | `{ sessionUuid, actionId, params? }` | call `hub.executeSessionAction(sessionUuid, actionId, params)` |
| `botster.url.open` | `{ sessionId, sessionUuid, url }` | web client navigation only |
| `botster.session.move.request` | `{ sessionId, sessionUuid }` | open move UI, then call `hub.moveAgentWorkspace` |
| `botster.session.delete.request` | `{ sessionId, sessionUuid, inWorktree }` | open delete UI, then call `hub.deleteAgent` |

Rules:

- action ids are semantic Botster events, not DOM event names
- local UI actions and hub-routed actions share one action envelope shape
- workspace/session composites may still open Rails-owned modals or prompts for
  rename, move, and delete

## Density Model

The sidebar and main panel must share component logic and differ only by density.

Allowed densities in `current`:

- `sidebar` — compact row height, hover-revealed actions, tighter typography
- `panel` — larger card-like row layout, always-present affordances where appropriate

Any variant beyond those two is out of scope for `current`.

## Why Workspace/Session Composites Stay Internal

`WorkspaceGroup`, `SessionRow`, and the preview/menu composites encode a lot of current product behavior:

- session naming fallback rules
- activity indicator derivation
- accessory-vs-agent affordances
- session action state mapping
- action availability rules

That behavior is product-specific. Freezing it into the Lua contract would lock
Botster into premature APIs. The public Lua surface stops at shared primitives,
entity bindings, and generic action feedback, while these composites remain an
internal Rails runtime detail.

## Acceptance Criteria For Workspace/Session Runtime

- sidebar and main panel share the same row/group logic with density variants
- session action indicator, action error state, and actions menu render from normalized state
- hub transport remains structured state, not HTML
- Rails continues to own the page shell and primitive registry
- the action vocabulary above is sufficient to reproduce the current session/workspace behavior

## Lua-Authored Plugin Surface Path

Plugin surfaces publish durable state as generic entity frames, render stable
Lua primitive trees, bind data with `$bind` and `ui.bind_list`, and return
generic `ui_action_result` frames for submitters. Project Pipelines is the
reference plugin for this model. New plugin UI should extend that path, not add
plugin-specific browser stores, renderer-specific pending state, or duplicate
client renderers.
