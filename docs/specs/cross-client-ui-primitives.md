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

Authoritative app state from hub/Lua:

- sessions
- workspaces
- selection
- notifications
- preview lifecycle
- modal state

### 2. Shared UI contract

Renderer-agnostic node tree plus action envelopes.

This is the layer that should be shared between web and TUI.

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
type UiNodeV1 = {
  type: UiPrimitiveTypeV1
  id?: string
  props?: Record<string, unknown>
  children?: UiNodeV1[]
  slots?: Record<string, UiNodeV1[]>
}

type UiActionV1 = {
  id: string
  payload?: Record<string, unknown>
  disabled?: boolean
}

type UiCapabilitySetV1 = {
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
- pointer kind is defined by `UiViewportV1.pointer` in the adaptive viewport spec, not duplicated in `UiCapabilitySetV1`

## Shared Primitive Set

This is the recommended shared primitive inventory.

### Layout primitives

- `stack`
- `inline`
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

### Action primitives

- `button`
- `icon_button`
- `menu`
- `menu_item`
- `dialog`

### Input primitives

- `text_input`
- `checkbox`
- `toggle`
- `select`

### Botster-specialized primitives

- `terminal_view`
- `connection_code_view`

These specialized primitives are still valid shared primitives because both clients already have native implementations for them or need them soon.

## Recommended Prop Model

The shared props should be small and semantic.

### Shared style tokens

```ts
type UiInteractionDensityV1 = "compact" | "comfortable"
type UiToneV1 = "default" | "muted" | "accent" | "success" | "warning" | "danger"
type UiAlignV1 = "start" | "center" | "end" | "stretch"
```

These tokens should mean the same thing in both clients, even though the concrete rendering differs.

### `stack`

```ts
type StackPropsV1 = {
  direction: "vertical" | "horizontal"
  gap?: "0" | "1" | "2" | "3" | "4" | "6"
  align?: UiAlignV1
  justify?: "start" | "center" | "end" | "between"
}
```

This deliberately replaces separate `HSplit` and `VSplit` as the shared semantic contract. The TUI can still translate `stack.direction` into its internal split nodes.

### `panel`

```ts
type PanelPropsV1 = {
  title?: string
  tone?: "default" | "muted"
  border?: boolean
  interactionDensity?: UiInteractionDensityV1
}
```

### `text`

```ts
type TextPropsV1 = {
  text: string
  tone?: UiToneV1
  size?: "xs" | "sm" | "md"
  weight?: "regular" | "medium" | "semibold"
  monospace?: boolean
  italic?: boolean
  truncate?: boolean
}
```

### `list_item`

```ts
type ListItemPropsV1 = {
  selected?: boolean
  disabled?: boolean
  action?: UiActionV1
}
```

Required slots:

- `title`

Optional slots:

- `subtitle`
- `start`
- `end`
- `detail`

### `tree_item`

```ts
type TreeItemPropsV1 = {
  id: string
  expanded?: boolean
  selected?: boolean
  notification?: boolean
  action?: UiActionV1
}
```

Required slots:

- `title`

Optional slots:

- `subtitle`
- `start`
- `end`
- `children`

### `button`

```ts
type ButtonPropsV1 = {
  label: string
  action: UiActionV1
  variant?: "solid" | "ghost"
  tone?: "default" | "accent" | "danger"
  icon?: string
}
```

### `menu`

```ts
type MenuPropsV1 = {
  trigger: UiNodeV1[]
}
```

Required slot:

- `items`

### `dialog`

```ts
type DialogPropsV1 = {
  open: boolean
  title: string
}
```

Optional slots:

- `body`
- `footer`

### `text_input`

```ts
type TextInputPropsV1 = {
  id: string
  value?: string
  placeholder?: string
  label?: string
}
```

Controlled/uncontrolled rule:

- if `value` is present, Lua owns the input state
- if `value` is absent and `id` is present, the renderer may own local state

That matches the TUI's current controlled/uncontrolled widget behavior and should be preserved in web as well.

### `terminal_view`

```ts
type TerminalViewPropsV1 = {
  sessionUuid?: string | null
}
```

### `connection_code_view`

```ts
type ConnectionCodeViewPropsV1 = {
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
- `botster.session.preview.toggle`
- `botster.workspace.toggle`
- `botster.workspace.rename.request`
- `botster.menu.open`

Rules:

- actions are semantic intent ids, not click handlers
- payloads use stable domain ids like `sessionUuid` and `workspaceId`
- renderer-local events may exist internally, but the public contract stays semantic

## Shared Botster Surface Composition

The workspace/session UI should be described using shared primitives rather than client-specific composites.

Recommended composition:

- `tree`
- `tree_item` for workspace headers
- nested `list` or `tree_item` rows for sessions
- `status_dot` for activity
- `icon_button` or `menu` for row actions
- `badge` or `status_dot` for preview state
- `panel` plus `text` for preview error

This lets both renderers share the same semantic tree even if the web temporarily keeps some helpers like `SessionRow` internally during migration.

## Optimizations

### 1. Keep entities separate from nodes

Do not stuff full session objects into every row node.

Prefer:

- normalized session/workspace entities
- thin UI nodes that reference ids and derived display props

That reduces payload size and keeps selectors shared.

### 2. Share action ids across clients

The TUI and web should not invent separate command names for the same user intent.

If the user is selecting a session, both clients should emit the same action id and payload shape.

### 3. Use slots, not ad-hoc field names, for compound rows

The TUI list widget already has implicit regions like title and secondary lines. The web rows already have start, body, and end regions. Slots make those regions explicit and portable.

### 4. Separate semantics from renderer hints

If a client needs rendering hints, keep them in a renderer-specific namespace:

```ts
type RendererHintsV1 = {
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

The shared primitives can map onto the current Rust render tree like this:

| Shared primitive | Current TUI concept |
|---|---|
| `stack(direction=horizontal)` | `HSplit` |
| `stack(direction=vertical)` | `VSplit` |
| `overlay` | `Centered` plus clear/block handling |
| `panel` | `BlockConfig` |
| `list` / `list_item` | `WidgetType::List` and `ListProps` |
| `text` | `WidgetType::Paragraph` lines/spans |
| `text_input` | `WidgetType::Input` |
| `terminal_view` | `WidgetType::Terminal` |
| `connection_code_view` | `WidgetType::ConnectionCode` |

That means the near-term work is primarily an adapter, not a full renderer rewrite.

## Mapping To Web Runtime

The web runtime should map the same shared primitives onto React components.

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
