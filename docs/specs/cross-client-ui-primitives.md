# Cross-Client UI Primitives

## Goal

Define one Botster UI contract that can be rendered by multiple clients:

- React in the web UI
- Rust/ratatui in the TUI

The shared contract should describe semantic UI structure and actions, not implementation details from the browser DOM or ratatui widgets.

Adaptive surface behavior across compact and expanded viewports is specified separately in [adaptive-ui-viewport-and-presentation.md](adaptive-ui-viewport-and-presentation.md).

## Why Unify

Botster already has two important facts:

- the TUI is primitive-based today
- the web runtime is moving toward primitive-based rendering

If those contracts drift, Botster will end up maintaining two UI systems:

- one set of concepts for Lua/TUI
- another set of concepts for Rails/web

That is unnecessary duplication. The better split is:

- one shared semantic interface
- two renderer implementations

## Design Rule

Unify at the semantic node and action layer, not at the renderer widget layer.

Good shared concepts:

- stack
- panel
- text
- list
- tree
- button
- input
- menu
- dialog
- terminal view
- action ids
- selection state
- density
- tone

Bad shared concepts:

- DOM attributes
- Tailwind class names
- React component internals
- ratatui `Block` options
- web hover-only behavior
- terminal-only cursor tricks

## Contract Stack

Botster should use four layers.

### 1. Domain state

Authoritative app state from hub/Lua, shipped to clients as durable entity
frames:

- sessions
- workspaces
- spawn targets
- worktrees
- hub metadata
- plugin-defined entities

The durable shared state contract is:

- `entity_snapshot`
- `entity_upsert`
- `entity_patch`
- `entity_remove`

Both the browser and TUI are equal consumers of this entity-frame stream. They
may store it in different local structures, but the entity envelopes are the
only shared durable model-state path.

For plugin-owned data, `plugin.db` is private durable persistence and entity
frames are the public read-model contract. Plugins should publish explicit
normalized records for shared renderers instead of exposing raw table shape or
creating plugin-specific browser stores.

Entity-backed plugin templates should make that split visible in files:
`db.lua` for `plugin.db` schema/migrations, model/repository modules for
validated mutations, `entity_contract.lua` for published entity names and bound
read-model fields, and `entities.lua` for projections plus snapshot/delta
publishers. Per-client modal, disclosure, focus, and draft affordances stay in
`ui.local_state` and presentation actions instead of the database or entity
stream.

Plugin entity types occupy the binding path's first segment directly. A Project
Pipelines ticket collection binds as `/project-pipelines.ticket`; one title
field binds as `/project-pipelines.ticket/ticket_123/title`.

Presentation and control state is separate:

- selection
- notifications
- preview lifecycle
- modal state
- route registry
- rendered UI tree snapshots

Those may influence rendering, navigation, or local workflow, but they must not
compete with entity frames as durable domain state.

### 2. Shared UI contract

Renderer-agnostic node tree plus action envelopes.

This is the layer that should be shared between web and TUI.
It may contain `$bind` references into entity stores and derived display props,
but it should not duplicate full entity records as a second state store.

### 3. Renderer implementation

Client-specific rendering:

- React components for web
- Rust widgets for TUI

### 4. Platform adapter

Client-specific affordances and side effects:

- browser navigation
- ratatui focus handling
- modal host behavior
- clipboard integration
- hover behavior
- external link opening

## Core Shared Types

```ts
type UiNode = {
  type: UiPrimitiveType
  id?: string
  props?: Record<string, unknown>
  children?: UiNode[]
  slots?: Record<string, UiNode[]>
}

type UiAction = {
  id: string
  payload?: Record<string, unknown>
  disabled?: boolean
}

type UiCapabilitySet = {
  hover: boolean
  dialog: boolean
  tooltip: boolean
  externalLinks: boolean
  binaryTerminalSnapshots: boolean
}
```

Rules:

- `type` names are Botster semantic primitives
- `props` hold primitive-specific state, not renderer config
- `slots` are preferred over positional `children` whenever a component has semantic regions like `title`, `subtitle`, `start`, `end`, or `footer`
- `id` is stable across frames and enables controlled or uncontrolled state in either renderer
- pointer kind is defined by `UiViewport.pointer` in the adaptive viewport spec, not duplicated in `UiCapabilitySet`

## Shared Primitive Set

This is the recommended shared primitive inventory.

### Layout primitives

- `stack`
- `inline`
- `form`
- `panel`
- `scroll_area`
- `overlay`

### Content primitives

- `text`
- `icon`
- `badge`
- `status_dot`
- `empty_state`

### Collection primitives

- `list`
- `list_item`
- `tree`
- `tree_item`
- `table`

### Action primitives

- `button`
- `icon_button`
- `menu`
- `menu_item`
- `dialog`

### Input primitives

- `text_input`
- `textarea`
- `checkbox`
- `select`

### Botster-specialized primitives

- `terminal_view`
- `connection_code_view`

These specialized primitives are still valid shared primitives because both clients already have native implementations for them or need them soon.

## Recommended Prop Model

The shared props should be small and semantic.

### Shared style tokens

```ts
type UiInteractionDensity = "compact" | "comfortable"
type UiTone = "default" | "muted" | "accent" | "success" | "warning" | "danger"
type UiAlign = "start" | "center" | "end" | "stretch"
```

These tokens should mean the same thing in both clients, even though the concrete rendering differs.

### `stack`

```ts
type StackProps = {
  direction: "vertical" | "horizontal"
  gap?: "0" | "1" | "2" | "3" | "4" | "6"
  align?: UiAlign
  justify?: "start" | "center" | "end" | "between"
}
```

This deliberately replaces separate `HSplit` and `VSplit` as the shared semantic contract. The TUI can still translate `stack.direction` into its internal split nodes.

### `form`

```ts
type FormProps = {}
```

`form` is an explicit validation boundary for input controls and submit-style
buttons. The web renderer maps it to a native `<form>` and prevents native page
submission; `button` actions inside the form run native validity checks before
dispatching their Botster action. Non-submit action primitives such as
`icon_button`, tree item actions, and empty-state primary actions do not validate
the form. The TUI may render a form like a vertical stack until richer form
affordances exist.

### `panel`

```ts
type PanelProps = {
  title?: string
  tone?: "default" | "muted"
  border?: boolean
  interactionDensity?: UiInteractionDensity
}
```

### `text`

```ts
type TextProps = {
  text: string
  tone?: UiTone
  size?: "xs" | "sm" | "md"
  weight?: "regular" | "medium" | "semibold"
  monospace?: boolean
  italic?: boolean
  truncate?: boolean
}
```

### `list_item`

```ts
type ListItemProps = {
  selected?: boolean
  notification?: boolean
  action?: UiAction
}
```

Required slots:

- `title`

Optional slots:

- `subtitle`
- `start`
- `end`
- `detail`

`list` has no shared props in `current`. It exists to give renderers a native
collection boundary; rows are authored as `list_item` children or expanded from
`ui.bind_list`.

### `tree_item`

```ts
type TreeItemProps = {
  id: string
  expanded?: boolean
  selected?: boolean
  notification?: boolean
  action?: UiAction
}
```

Required slots:

- `title`

Optional slots:

- `subtitle`
- `start`
- `end`
- `children`

### `table`

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

`columns` define the ordered visible fields. Each column `key` is looked up on
each row object and `label` is rendered in the header. `rows` are plain JSON
objects so plugin-owned entity lists can bind directly:

```lua
ui.table{
  columns = {
    { key = "title", label = "Ticket" },
    { key = "status", label = "Status" },
  },
  rows = ui.bind("/project-pipelines.ticket"),
}
```

Tables are for read-oriented, scannable collections. Row actions or row-local
composition should use `list` / `list_item` or `tree` / `tree_item` until the
table contract grows explicit row action semantics.

### `button`

```ts
type ButtonProps = {
  label: string
  action: UiAction
  variant?: "solid" | "ghost"
  tone?: "default" | "accent" | "danger"
  icon?: string
}
```

### `menu`

```ts
type MenuProps = {
  trigger: UiNode[]
}
```

Required slot:

- `items`

### `dialog`

```ts
type DialogProps = {
  open: boolean
  title: string
}
```

Optional slots:

- `body`
- `footer`

### `text_input`

```ts
type TextInputProps = {
  value?: string
  placeholder?: string
  label?: string
  required?: boolean
  onChange?: UiAction
}
```

Controlled/uncontrolled rule:

- if `value` is present, Lua owns the input state
- if `value` is absent and the node `id` is present, the renderer may own local state

The renderer emits `onChange` with the next `{ value }` merged into the action payload.

`required` is shared metadata for accessibility, renderer styling, and native
web control attributes where available. It does not replace action-handler
validation: renderers may block submit-style button dispatch for invalid
controls inside `form`, but the hub/plugin action handler must still validate
required fields. The TUI may ignore the visual treatment until editable form
affordances mature.

### `textarea`

```ts
type TextareaProps = {
  value?: string
  placeholder?: string
  label?: string
  required?: boolean
  onChange?: UiAction
}
```

The ownership and `onChange` rules match `text_input`.

### `checkbox`

```ts
type CheckboxProps = {
  label?: string
  selected?: boolean
  onChange?: UiAction
}
```

For `checkbox`, explicit `selected` means Lua-controlled; omitted
`selected` plus a stable node `id` lets renderers own local state. Renderers emit
`onChange` with the next `{ selected }` merged into the action payload.
`required` is intentionally not part of `CheckboxProps` yet because checkbox
requiredness means "must be checked" rather than "must have a value"; model that
as domain validation until a checked-required checkbox contract is specified.

### `select`

```ts
type SelectProps = {
  label?: string
  value?: string
  placeholder?: string
  required?: boolean
  options: Array<{ value: string; label: string }>
  onChange?: UiAction
}
```

That matches the TUI's current controlled/uncontrolled widget behavior and should be preserved in web as well.

### `terminal_view`

```ts
type TerminalViewProps = {
  sessionUuid?: string | null
}
```

### `connection_code_view`

```ts
type ConnectionCodeViewProps = {
  url: string
  qrAscii?: string[]
}
```

The web renderer may choose a canvas/SVG QR implementation while the TUI uses ASCII output.

## Action Contract

Actions should be shared across clients.

Examples:

- `botster.session.select`
- `botster.session.close.request`
- `botster.session.action.execute`
- `botster.workspace.toggle`
- `botster.workspace.rename.request`
- `botster.menu.open`

Rules:

- actions are semantic intent ids, not click handlers
- payloads use stable domain ids like `sessionUuid` and `workspaceId`
- renderer-local events may exist internally, but the public contract stays semantic

Submit-style actions are correlated request/response interactions. The renderer
that dispatches a `button` or `icon_button` action generates an
`action_request_id`, marks only that submitter pending, and sends the id beside
the semantic action envelope. The hub echoes the id in a `ui_action_result`
frame with numeric `v` versioning, the original `action_id`, `ok`, `handled`,
`via`, and optional `message`, `error`, or `navigate = { label, path }`.

Pending and result rendering belong to each renderer. Lua/plugin payloads may
return semantic result metadata through the action handler, but they must not
carry DOM attributes, Tailwind classes, spinner choices, or other
renderer-specific feedback state.

## Shared Botster Surface Composition

The workspace/session UI should be described using shared primitives rather than client-specific composites.

Recommended composition:

- `tree`
- `tree_item` for workspace headers
- nested `list` or `tree_item` rows for sessions
- `status_dot` for activity
- `icon_button` or `menu` for row actions
- `badge` or `status_dot` for action state
- `panel` plus `text` for action errors

This lets both renderers share the same semantic tree even if the web temporarily keeps some helpers like `SessionRow` internally during migration.

## Optimizations

### 1. Keep entities separate from nodes

Do not stuff full session objects into every row node.

Prefer:

- normalized session/workspace entities
- thin UI nodes that reference ids and derived display props

That reduces payload size and keeps selectors shared.

`ui_tree_snapshot` is therefore a presentation snapshot for one rendered
surface. It is not a competing durable state stream. If a row needs live session
or workspace data, reference the entity store by id or binding path and let both
clients resolve it from the same `entity_*` frames.

For plugin UI, this is mandatory rather than an optimization: publish durable
records through `entity_snapshot`, `entity_upsert`, `entity_patch`, and
`entity_remove`, then bind UI nodes to those records. Do not introduce
client-local model stores or presentation snapshots as alternate durable data
paths. If the renderer needs a field that does not exist in persistence, add it
to the plugin entity read model so browser and TUI clients consume the same
contract.

### 2. Share action ids across clients

The TUI and web should not invent separate command names for the same user intent.

If the user is selecting a session, both clients should emit the same action id and payload shape.

### 3. Use slots, not ad-hoc field names, for compound rows

The TUI list widget already has implicit regions like title and secondary lines. The web rows already have start, body, and end regions. Slots make those regions explicit and portable.

### 4. Separate semantics from renderer hints

If a client needs rendering hints, keep them in a renderer-specific namespace:

```ts
type RendererHints = {
  web?: Record<string, unknown>
  tui?: Record<string, unknown>
}
```

Examples:

- web tooltip placement
- TUI highlight symbol

These must never replace the shared semantic props.

### 5. Capability-gate instead of forking the contract

Some primitives degrade differently by client:

- tooltips
- hover-revealed actions
- external links
- QR rendering

Use capability checks rather than separate web and TUI primitive names.

### 6. Preserve controlled/uncontrolled widget ownership

The TUI already has a useful rule:

- explicit `value` or `selected` means Lua-controlled
- stable `id` without state means renderer-controlled

The web runtime should use the same rule. This avoids two different state-ownership models.

## Mapping To Existing TUI Runtime

The TUI does not need a flag day rewrite.

The TUI should continue applying `entity_snapshot`, `entity_upsert`,
`entity_patch`, and `entity_remove` into Rust entity stores, then adapt shared
UI nodes against those stores. Route registries, UI tree snapshots, transient
events, and request-scoped replies are presentation or control inputs around
the entity store, not replacement model stores.

Bindings use the shared `/<entity_type>/<id>/<field>` grammar. Plugin-owned
types such as `project-pipelines.ticket` occupy the `<entity_type>` segment
directly, so a list source is `/project-pipelines.ticket` and a field binding
is `/project-pipelines.ticket/ticket_123/title`. `ui.bind_list` expands over
the entity store in insertion order and must flatten into ordinary children or
slot siblings in both browser and TUI renderers. `ui.bind_list` may include a
`where` object; browser and TUI renderers must apply it as exact matches against
top-level entity fields before expanding the row template.

The shared primitives can map onto the current Rust render tree like this:

| Shared primitive | Current TUI concept |
|---|---|
| `stack(direction=horizontal)` | `HSplit` |
| `stack(direction=vertical)` | `VSplit` |
| `overlay` | `Centered` plus clear/block handling |
| `panel` | `BlockConfig` |
| `list` / `list_item` | `WidgetType::List` and `ListProps` |
| `table` | ratatui `Table` |
| `text` | `WidgetType::Paragraph` lines/spans |
| `text_input` | `WidgetType::Input` |
| `terminal_view` | `WidgetType::Terminal` |
| `connection_code_view` | `WidgetType::ConnectionCode` |

That means the near-term work is primarily an adapter, not a full renderer rewrite.

## Mapping To Web Runtime

The web runtime should map the same shared primitives onto React components.
Like the TUI, it should consume durable model data from `entity_*` frames and
treat `ui_route_registry` and `ui_tree_snapshot` as presentation/control inputs.

Examples:

- `stack` -> flex layout primitive
- `panel` -> bordered container primitive
- `tree_item` -> semantic row with slots
- `menu` -> dropdown/menu implementation
- `dialog` -> modal/sheet implementation
- `terminal_view` -> existing terminal display mount

## Recommended Immediate Direction

1. Treat this shared spec as the source of truth for primitive names, actions, slots, and state ownership.
2. Let the web React island adapt into this contract first.
3. Add a TUI adapter from the shared nodes into the current `RenderNode`/`WidgetType` system.
4. Only after both clients work through the same semantic contract should Botster expose the tree format more broadly to Lua.

## Non-Goals

- forcing identical visuals across TUI and web
- exposing renderer internals in the shared contract
- making every Botster screen schema-driven immediately
- rewriting the current TUI renderer before the adapter path is proven
