# `ui_contract` — cross-client UI DSL (Phase A)

Phase-A foundation of Botster's unified, renderer-agnostic UI contract. This
module defines **only** shared types + a Lua DSL. It does **not** render
anything — Phase B (TUI adapter) and Phase C (web React renderer) consume
these types.

## Specs

This module is the Rust-side source of truth for:

- [`docs/specs/cross-client-ui-primitives.md`](../../../docs/specs/cross-client-ui-primitives.md)
- [`docs/specs/adaptive-ui-viewport-and-presentation.md`](../../../docs/specs/adaptive-ui-viewport-and-presentation.md)
- [`docs/specs/web-ui-primitives-runtime.md`](../../../docs/specs/web-ui-primitives-runtime.md)

Knowledge-vault background:

- `~/knowledge/notes/cross-client ui should share semantic primitives and actions with renderer-specific adapters.md`
- `~/knowledge/notes/phase one web ui composites stay internal while Lua public contract stops at primitives.md`
- `~/knowledge/notes/rust-registered lua primitives are globals not modules.md`
- `~/knowledge/notes/hub and tui run separate lua vms.md`

## Module layout

| File | Purpose |
|---|---|
| `tokens.rs` | Shared scalar tokens (`UiTone`, `UiAlign`, `UiSpace`, `UiSize`, `UiInteractionDensity`, `UiPresentation`, …). |
| `viewport.rs` | `UiViewport` plus its classes (`UiWidthClass`, `UiHeightClass`, `UiPointer`, `UiOrientation`). |
| `node.rs` | `UiNode`, `UiChild`, `UiAction`, `UiCapabilitySet`, `UiResponsive<T>`, `UiConditional`, `UiCondition`. |
| `props.rs` | Strongly-typed Props structs for every current Lua-public primitive + `DialogProps`. |
| `lua.rs` | `register(&Lua)` — installs the `ui` table as a Lua global. |

## Wire format

All types serialize as JSON matching the TypeScript types in the specs
(camelCase fields, lowercase enum variants, string-valued `UiSpace`).

### Node envelope

```json
{
  "type": "stack",
  "id": "…optional stable id…",
  "props": { "direction": "vertical", "gap": "2" },
  "children": [ /* UiChild */ ],
  "slots":    { "title": [ /* UiChild */ ] }
}
```

### Responsive values (at any prop-value position)

```json
{ "$kind": "responsive", "width": { "compact": "vertical", "expanded": "horizontal" } }
{ "$kind": "responsive", "height": { "short": "sm", "tall": "md" } }
{ "$kind": "responsive", "width": { … }, "height": { … } }
```

The two dimensions are split because `"regular"` is valid in both
`UiWidthClass` and `UiHeightClass`; a flat map would be ambiguous.

### Conditional wrappers (at child / slot position)

```json
{ "$kind": "when",   "condition": { "width": "compact" }, "node": { "type": "…" } }
{ "$kind": "hidden", "condition": { "width": "compact" }, "node": { "type": "…" } }
```

`$kind = "when"` renders the inner node only when the condition matches;
`$kind = "hidden"` renders the inner node only when the condition does **not**
match. Both are accepted anywhere a `UiNode` is (children arrays, slots).

## Lua DSL

`ui_contract::lua::register(&Lua)` installs a `ui` global.

### Primitive constructors (current, Lua-public)

All prop shapes align with `cross-client-ui-primitives.md` — web-runtime-only
extensions (`Panel.padding`, `Panel.radius`, `Stack.padding`,
`Button.leadingIcon`, `Button.disabled`, `IconButton.disabled`,
`Tree.density`) are intentionally excluded.

```lua
-- direction is REQUIRED per spec
ui.stack{ direction = "vertical" | "horizontal" | ui.responsive(...),
          gap = ..., align = ..., justify = ..., children = {...} }

ui.inline{ gap = ..., align = ..., justify = ..., wrap = ..., children = {...} }

ui.panel{ title = ..., tone = ..., border = ...,
          interaction_density = ... | ui.responsive(...),
          children = {...} }

ui.scroll_area{ axis = ..., children = {...} }

ui.text{ text = ..., tone = ..., size = ..., weight = ...,
         monospace = ..., italic = ..., truncate = ... }

ui.icon{ name = ..., size = ..., tone = ..., label = ... }

ui.badge{ text = ..., tone = ..., size = ... }

ui.status_dot{ state = ..., label = ... }

ui.empty_state{ title = ..., description = ..., icon = ...,
                primary_action = ui.action(...) }

-- No `disabled` field: disabled travels on `action.disabled` (UiAction).
-- `icon` is the cross-client canonical name (NOT `leadingIcon`).
ui.button{ label = ..., action = ..., variant = ..., tone = ..., icon = ... }

-- No `disabled` field: use `action.disabled`.
ui.icon_button{ icon = ..., label = ..., action = ..., tone = ... }

-- Form primitives. If `value` / `selected` is omitted and `id` is present,
-- renderers may own local state; otherwise Lua owns the controlled state.
-- `on_change` actions receive the next `{ value = ... }` or
-- `{ selected = ... }` payload merged into the action payload.
ui.text_input{ id = ..., label = ..., value = ..., placeholder = ...,
               on_change = ui.action(...) }

ui.textarea{ id = ..., label = ..., value = ..., placeholder = ...,
             on_change = ui.action(...) }

-- TUI v1 note: textarea renders as read-only/display-only text in the TUI.
-- Web uses the Catalyst textarea and emits on_change. Use text_input for
-- editable TUI text entry until multiline editing is added to the adapter.

ui.checkbox{ id = ..., label = ..., selected = ...,
             on_change = ui.action(...) }

ui.select{
  id = ..., label = ..., value = ..., placeholder = ...,
  options = {
    { value = "normal", label = "Normal" },
    { value = "high", label = "High" },
  },
  on_change = ui.action(...),
}

-- No shared props in current (web's `density` is renderer-internal).
ui.tree{ children = {...} }

-- No shared props in current. Use list_item children or ui.bind_list rows.
ui.list{ children = {...} }

-- `title` slot is REQUIRED; `detail` is optional for secondary row content:
ui.list_item{
  id = ..., selected = ..., notification = ..., action = ...,
  title    = { ... },  -- required
  subtitle = { ... },  -- optional
  start    = { ... },  -- optional
  end_     = { ... },  -- optional (Lua `end` is reserved)
  detail   = { ... },  -- optional
}

-- Scannable read-oriented tables. Rows may be a direct entity binding.
ui.table{
  columns = {
    { key = "title", label = "Ticket" },
    { key = "status", label = "Status" },
  },
  rows = ui.bind("/project-pipelines.ticket"),
}

-- `title` slot is REQUIRED; spec slot keys may be hoisted to top level:
ui.tree_item{
  id = ..., expanded = ..., selected = ..., notification = ..., action = ...,
  title    = { ... },  -- required
  subtitle = { ... },  -- optional
  start    = { ... },  -- optional
  end_     = { ... },  -- optional (Lua `end` is reserved)
  children = { ... },  -- optional nested tree_items
}
```

#### Slot schema enforcement

Constructors reject unknown slot keys at construction time and raise a Lua
error if a required slot is missing. This catches typos like
`slots = { footr = ... }` immediately rather than silently misrendering.

### Primitive constructors (internal / experimental)

```lua
ui.dialog{ open = ..., title = ..., presentation = "auto" | "inline" | "overlay" | "sheet" | "fullscreen",
           body = { ... },  -- hoisted into slots.body
           footer = { ... } -- hoisted into slots.footer
}
```

`Dialog` is deferred from the Lua-public current surface per
`docs/specs/web-ui-primitives-runtime.md`. It is registered here so renderers
can adopt it in Phase B / Phase C. Presentation defaults to `"auto"`. Top-level
`body` and `footer` keys are automatically hoisted into `slots` per the
cross-client spec's Dialog shape.

### `Menu` / `MenuItem` / `Toggle`

**Intentionally not exposed.** Menu primitives are web-runtime-internal in
current until the cross-client menu interaction model stabilizes. `Toggle` is
deferred from the first shared form primitive set.

### Adaptive helpers

```lua
-- Responsive values. Width-only shorthand:
ui.responsive({ compact = "vertical", expanded = "horizontal" })

-- Explicit form (required when mixing dimensions):
ui.responsive({
  width  = { compact = "sidebar", expanded = "panel" },
  height = { short = "compact", tall = "comfortable" },
})

-- Conditional render (bare string = widthClass match):
ui.when("expanded", sidebar_node)
ui.when({ width = "compact", pointer = "coarse" }, compact_toolbar)

-- Hidden render (same condition semantics, inverted):
ui.hidden({ width = "compact" }, metadata_node)
```

### Action helper

```lua
ui.action("botster.session.select", { sessionUuid = "sess-…" })
-- => { id = "botster.session.select", payload = { sessionUuid = "sess-…" } }
```

### Browser-local presentation state

Plugin surfaces can bind ephemeral browser-local state with `ui.local_state(key,
default)`. This is for presentation only: modal open flags, local disclosure
state, and similar per-browser UI state. It is scoped by hub and target surface,
is not sent to Lua/hub, and resets on browser reload.

```lua
ui.button{
  label = "Open",
  action = ui.action("botster.presentation.set", {
    key = "ticket-1-spawn-open",
    value = true,
  }),
}

ui.dialog{
  open = ui.local_state("ticket-1-spawn-open", false),
  title = "Spawn agent",
  footer = {
    ui.button{
      label = "Cancel",
      action = ui.action("botster.presentation.clear", {
        key = "ticket-1-spawn-open",
      }),
    },
  },
}
```

### Slots

Any primitive with semantic regions uses the `slots` key, not positional
`children`. Slot keys match the spec:

```lua
ui.tree_item{
  id = "sess-1",
  slots = {
    title    = { ui.text{ text = "Primary" } },
    subtitle = { ui.text{ text = "Secondary" } },
    start    = { ui.status_dot{ state = "active" } },
    end_     = { ui.icon_button{ icon = "more", label = "…", action = ui.action("…") } },
  },
}
```

`end` is a reserved word in Lua, so authors write `end_` and the marshalling
layer rewrites it to `end` on the wire.

### Casing

Authors may write prop keys in either `snake_case` or `camelCase` in Lua —
the marshalling layer converts top-level keys to the wire-format `camelCase`.
Example:

```lua
ui.panel{ interaction_density = "compact", ... }  -- emits "interactionDensity"
ui.panel{ interactionDensity  = "compact", ... }  -- same wire output
```

The conversion applies to top-level prop keys and `ui.when` / `ui.hidden`
condition keys. Nested structures (e.g. the payload inside `ui.action`) are
passed through verbatim.

### Prop allowlist

Every primitive declares its canonical prop set (matching the cross-client
spec). Unknown props — web-only extensions, typos, or author mistakes — are
rejected at construction with an error that lists the allowed props:

```
ui.panel: unknown prop `padding`. Allowed props: ["title", "tone", "border", "interactionDensity"]
```

The allowlist covers both casing forms: `leading_icon` and `leadingIcon` are
both rejected for Button because neither is a cross-client canonical Button
prop (only `icon` is).

### Controlled vs uncontrolled state

The TUI rule applies in both renderers:

- explicit `value` or `selected` ⇒ Lua owns the state (controlled)
- stable `id` without `value` / `selected` ⇒ renderer owns local state

## Integration points

- **Hub VM** (`crate::lua::LuaRuntime`) — registered inside
  `crate::lua::primitives::register_all`.
- **TUI VM** (`crate::clients::tui::layout_lua::LayoutLua`) — registered inside
  `LayoutLua::new`, before executing the layout source.

Both VMs use the same `ui_contract::lua::register` function so the DSL stays
identical across them.

## Running the tests

Unit tests (all Props round-trip + Lua constructor unit tests):

```bash
cd cli
./test.sh --unit -- ui_contract
```

Integration tests (end-to-end Lua → JSON wire shapes):

```bash
cd cli
./test.sh --integration -- list_and_list_item_are_public_lua_primitives
./test.sh --integration -- table_renders_rows_from_plugin_entity_bind
```

`./test.sh --integration -- ui_contract` filters the *test name* pattern, not
the test file. Use focused test names or run the full integration suite with
`./test.sh --integration`.

## Non-goals for Phase A

- No TUI adapter — Phase B.
- No React renderer — Phase C.
- No `Menu` / `MenuItem` exposure to Lua.
- Workspace/session composites (`WorkspaceList`, `SessionRow`, …) stay
  web-runtime-internal per `docs/specs/web-ui-primitives-runtime.md`; public Lua
  plugin surfaces use shared primitives, entity bindings, and the composite
  primitives listed below.

## Wire protocol — composite primitives

The wire protocol replaces "rebuild + rebroadcast the entire `UiNode`
tree on every state change" with a delta protocol: structural snapshots
ship only on connect / structural change, per-entity field deltas ship on
data change. To keep authored layouts thin, the data-driven UI regions
they used to inline are now first-class composites:

| Constructor | Wire `type` | Required props |
|---|---|---|
| `ui.session_list{ density?, grouping?, owner_plugin?, visibility?, surface? }` | `session_list` | none |
| `ui.workspace_list{ density? }` | `workspace_list` | none |
| `ui.spawn_target_list{ on_select?, on_remove? }` | `spawn_target_list` | none |
| `ui.worktree_list{ target_id }` | `worktree_list` | `target_id` |
| `ui.session_row{ session_uuid, density? }` | `session_row` | `session_uuid` |
| `ui.session_terminal{ session_uuid, back? }` | `session_terminal` | `session_uuid` |
| `ui.surface_nav{ section?, density? }` | `surface_nav` | none |
| `ui.iframe{ src, title?, sandbox?, bridge? }` | `iframe` | `src` |
| `ui.hub_recovery_state{}` | `hub_recovery_state` | none |
| `ui.connection_code{}` | `connection_code` | none |
| `ui.new_session_button{ action }` | `new_session_button` | `action` |

These primitives are **data-driven**: they carry no slots and no children
on the wire. Each renderer (web React, ratatui TUI) reads from its
client-side entity store and expands the composite into the same flat tree
the current hub-rendered layout used to ship. Both renderers honor the same
density / grouping tokens. `surface_nav` renders `surfaces.register(..., nav={...})`
entries in core-owned navigation. `session_terminal` is the surface-local way
for plugin routes such as `/vault/sessions/:session_uuid` to mount Botster's
core terminal viewer without routing through the global `/sessions/:session_uuid`
page.
`iframe` is web-first: browser clients resolve `botster-plugin-asset://` URLs
over the paired hub transport and mount the result in a sandboxed iframe, while
TUI clients render a placeholder.

Density follows the `UiSurfaceDensity` token (`sidebar` | `panel`) — see
`tokens::UiSurfaceDensity`. This is distinct from `UiInteractionDensity`
(`compact` | `comfortable`), which is a renderer-internal hit-target token.

### Action templates on list composites

`spawn_target_list.on_select` / `on_remove` are **action templates**: each
renderer merges the template's `id` (and any `payload`) with the per-row
identifier (e.g. `target_id`) before dispatch. When omitted, the composite
uses default action ids (`botster.spawn_target.select` /
`botster.spawn_target.remove`). The same convention will apply to other
list composites that grow per-row actions in the future.

## Wire protocol — `$bind` grammar

For plugin composites that need reactive data without registering a custom
React/TUI renderer, the wire format also defines a binding sentinel:

```json
{ "$bind": "/<entity_type>/<id>/<field>" }
```

The sentinel may appear at any prop-value position. Both renderers replace
it with the resolved value before primitive dispatch.

Path grammar:

| Path | Resolves to |
|---|---|
| `/<type>/<id>/<field>` | scalar lookup |
| `/<type>/<id>` | whole record |
| `/<type>` | array of records, sorted by store insertion order |
| `@/<field>` | item-relative — only valid inside `ui.bind_list`'s `item_template` |

Plugin-owned entity types use the same first path segment; there is no
`/plugin/...` prefix. For example, a Project Pipelines ticket title binds as
`/project-pipelines.ticket/ticket_123/title`, and the ticket collection binds
as `/project-pipelines.ticket`. Missing stores, ids, and fields resolve to
`null`; plain list binds such as `ui.bind("/project-pipelines.ticket")` resolve
missing list sources to `[]`. `@/...` paths are only valid while expanding a
list template, because they resolve against the current list item.

Plus the list expansion helper:

```lua
ui.bind_list{
  source = "/project-pipelines.ticket",
  where = { status = "open" },
  item_template = ui.tree_item{
    id = ui.bind("@/id"),
    title = { ui.text{ text = ui.bind("@/title") } },
    subtitle = { ui.text{ text = ui.bind("@/status") } },
  },
  empty_template = ui.empty_state{
    title = "No tickets",
    description = "Create a ticket to start a pipeline.",
  },
}
```

The web resolver (`app/frontend/ui_contract/binding.tsx`) and the TUI
resolver (`cli/src/clients/tui/ui_contract_adapter/binding.rs`) must agree on
this grammar. `bind_list` expands to ordinary sibling nodes when placed inside
`children` or slot arrays so plugin layouts can use it directly in stable
presentation trees. `where` filters are exact top-level field matches and are
applied by both renderers before the row template expands. When no records
match, including when the source store is absent, an optional `empty_template`
expands as a single sibling node; it is not item-scoped, so `@/...` bindings
inside the empty template resolve to `null`.

## Versioning

This is `ui-contract` with the wire-protocol composite extensions
listed above. Additive changes are backward-compatible; removing props or
changing payload semantics requires a major bump.
