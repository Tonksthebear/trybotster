# Web UI Primitives Runtime

## Goal

Replace the current browser-side "template cloning plus Stimulus reconciliation" approach for live operator surfaces with a Rails-owned React runtime that renders trusted component primitives from structured state.

The long-term direction is to make the web UI programmable in the same way the TUI is programmable:

- the runtime owns primitives
- Lua composes them
- the browser renders them locally

This is not a plan to let the CLI send arbitrary HTML or JavaScript.

## Problem

The agent/workspace UI has crossed the line where Stimulus is acting as a miniature frontend framework:

- websocket-driven authoritative state
- per-row async preview lifecycle
- conditional visibility and affordances
- hidden/system-session filtering
- duplicated row logic across sidebar and main list
- manual DOM reconciliation through cloned templates and `data-*` attributes

That model is now fragile. The hosted-preview disable bug was a good example: stale row state is easy to create because the rendering layer is manually derived from incremental messages instead of from a normalized state tree.

## Product Direction

Botster's web UI should follow the same principle as the TUI:

- Rails ships a trusted web runtime plus primitive components
- CLI/Lua owns live state and composition
- the browser renders locally from structured data

This enables:

- richer live UIs with fewer reconciliation bugs
- reusable browser primitives
- future user-customizable web frontends
- a safer extensibility boundary than raw HTML/CSS/JS injection

## Non-Goals

- CLI-generated HTML
- CLI-served arbitrary JavaScript components
- replacing all Rails views with React
- turning the entire app into an SPA
- exposing raw third-party component APIs as the public Lua contract

## Architectural Decision

### Rails owns presentation primitives

Rails owns:

- the browser runtime
- the trusted component registry
- design tokens and styling
- accessibility behavior
- command dispatch plumbing
- schema validation

### CLI/Lua owns composition and state

CLI/Lua owns:

- authoritative live state
- layout composition
- action intent definitions
- feature-specific orchestration

### Browser owns rendering and reconciliation

The browser runtime:

- receives structured state and UI trees
- validates them against the supported schema
- renders them using React
- dispatches actions back to the hub

## Why React

React is justified here not because "React is trendy," but because Botster needs a stable browser runtime with real reconciliation semantics.

Benefits:

- predictable re-rendering from normalized state
- shared components between sidebar and main panel
- easier derivation of computed UI state
- cleaner lifecycle handling across websocket updates
- a natural basis for a component registry and schema renderer

Preact would be sufficient for isolated islands. React is the better fit if Botster is becoming a programmable web UI platform.

## Why Not CLI HTML or Remote React Code

The CLI should not ship presentation code directly into the web app.

That would create:

- version skew between CLI and browser runtime
- upgrade fragility
- security/trust issues
- styling collisions
- no stable compatibility boundary

The protocol must remain declarative.

Good:

- `type`
- `props`
- `children`
- `action ids`
- structured state payloads

Bad:

- HTML strings
- raw CSS payloads
- arbitrary JavaScript bundles
- serialized React components

## Rendering Model

### Page model

Rails still renders the application shell:

- layouts
- navigation
- settings pages
- mostly static pages
- mount points for interactive islands

React mounts only inside explicit islands.

Turbo and Stimulus may still exist on the same page, but they must not mutate inside a React-owned subtree.

### Data model

The browser runtime should keep a normalized store for each live surface. For agent/workspace UI, that store likely starts with:

- `sessionsById`
- `workspaceOrder`
- `workspacesById`
- `selectedSessionId`
- `expandedWorkspaceIds`
- `ui.pendingActions`

All row UI should derive from selectors rather than from direct DOM mutation.

### Action model

Browser actions should remain command-oriented:

- `toggle_hosted_preview`
- `delete_session`
- `move_session`
- `rename_workspace`
- `select_session`

The browser sends action ids plus payloads to the hub. The hub remains authoritative.

## Extensibility Model

This spec follows the existing vault decision:

- theme tokens first
- primitive composition second
- sandboxed custom rendering later

The intended progression is:

1. Rails-owned React primitives for core Botster UI
2. Lua composition against a constrained component registry
3. Optional advanced extension points later, likely sandboxed

## Phase 1 Scope

Phase 1 is deliberately narrow:

- move the agent/workspace surface to a React island
- keep the rest of the app on Rails/Turbo/Stimulus
- define the initial primitive inventory
- prove the runtime with one real live surface

The first React island should own:

- workspace tree
- session rows
- row actions menu
- hosted preview indicator/state
- hosted preview error strip
- selection state
- expansion state

## Initial Primitive Inventory

The first task is to define the primitives Rails will own. These are not raw Radix or shadcn primitives as the public API. They are Botster primitives with stable contracts.

### Foundation primitives

- `Stack`
- `Inline`
- `Grid`
- `Panel`
- `Separator`
- `ScrollArea`
- `Spacer`

### Content primitives

- `Text`
- `Heading`
- `Code`
- `Icon`
- `Badge`
- `StatusDot`
- `EmptyState`

### Action primitives

- `Button`
- `IconButton`
- `LinkButton`
- `Menu`
- `MenuItem`
- `MenuSection`
- `Disclosure`
- `Dialog`
- `Tooltip`

### Form primitives

- `TextField`
- `Select`
- `Checkbox`
- `Toggle`
- `RadioGroup`
- `FieldMessage`

### Navigation/list primitives

- `List`
- `ListItem`
- `Tree`
- `TreeGroup`
- `TreeItem`
- `Tabs`

### Botster-specific composites for phase 1

These should be provided by Rails as first-party composites built from the primitives above:

- `WorkspaceList`
- `WorkspaceGroup`
- `WorkspaceHeader`
- `SessionRow`
- `SessionActivityIndicator`
- `HostedPreviewIndicator`
- `HostedPreviewError`
- `SessionActionsMenu`

The first phase should not attempt to express the entire agent/workspace UI purely from low-level layout nodes. A few Botster composites are the pragmatic bridge.

## Known Props/State Needed For Phase 1

The initial component contracts will need to cover at least:

### `WorkspaceGroup`

- `id`
- `title`
- `count`
- `expanded`
- `renamable`

### `SessionRow`

- `sessionId`
- `name`
- `titleLine`
- `subtext`
- `selected`
- `notification`
- `activityState`
- `isAccessory`
- `canMoveWorkspace`

### `HostedPreviewIndicator`

- `status`
- `url`
- `error`
- `installUrl`
- `visible`

Allowed statuses:

- `inactive`
- `starting`
- `running`
- `error`

### `SessionActionsMenu`

- `canPreview`
- `previewStatus`
- `previewUrl`
- `canMove`
- `canDelete`
- action ids for preview toggle, move, and delete

## Protocol Shape

The runtime should support a declarative node format similar to:

```json
{
  "type": "SessionRow",
  "key": "session:abc",
  "props": {
    "sessionId": "abc",
    "name": "main",
    "selected": false,
    "notification": false,
    "activityState": "active",
    "hostedPreview": {
      "status": "running",
      "url": "https://example.trycloudflare.com"
    }
  }
}
```

For phase 1, it is acceptable to keep the hub protocol as state-first rather than tree-first:

- hub still sends normalized session/workspace state
- browser React components render from that state

The component-tree protocol can follow after the first React island lands.

## Migration Plan

### Phase 0: Runtime decision and boundary definition

- choose React for the browser runtime
- define island boundaries
- define what stays in Rails/Stimulus
- define the primitive/component registry ownership model

### Phase 1: Agent/workspace React island

- create a React mount point for the sidebar and/or main workspace panel
- build a normalized store fed by existing hub events
- implement `WorkspaceGroup`, `SessionRow`, `HostedPreviewIndicator`, and `SessionActionsMenu`
- remove template cloning and most per-row DOM mutation logic from `agent_list_controller.js`

### Phase 2: Stable primitive contracts

- formalize the first Botster web component registry
- document supported component ids and prop schemas
- add validation and fallback behavior for unknown nodes

### Phase 3: Declarative UI composition from CLI/Lua

- allow Lua to describe surfaces using the registry
- keep actions declarative and browser-routed
- add capability negotiation/versioning

### Phase 4: User-extensible frontend composition

- support user-defined composition for allowed surfaces
- maintain the staged extensibility model from the vault
- add sandboxed escape hatches only after the primitives layer is mature

## Implementation Notes

### Shared state source

The existing `agent_list` payload should remain the initial authoritative source. Do not invent a second browser-only state channel for phase 1.

### Shared component code

Sidebar and main workspace list should render from shared row/group components with different density props instead of duplicated templates.

### Styling

Botster primitives should own styling. Third-party component libraries may be used internally, but must not become the public protocol contract.

### Turbo coexistence

Turbo can remain for navigation. React islands should:

- mount on page load
- clean up before Turbo cache if necessary
- rehydrate/reconnect cleanly on revisits

## Risks

- React introduced without a clear state boundary becomes a second source of truth
- exposing third-party components directly would freeze Botster to their API
- trying to make every surface schema-driven too early would slow delivery
- trying to skip composites and use only ultra-low-level primitives at first would overcomplicate the first migration

## Acceptance Criteria For Phase 1

- Agent/workspace UI no longer depends on cloned HTML templates for row rendering
- Preview indicator, actions menu, and error strip render from a normalized React state model
- Sidebar and main panel share row logic
- Preview enable/disable state updates render correctly without manual DOM bookkeeping
- Rails continues to own the page shell
- CLI continues to send structured state, not HTML

## First Concrete Deliverable

Before any runtime work starts, produce a versioned inventory of supported web primitives and phase-1 composites:

- foundation primitives
- content primitives
- action primitives
- form primitives
- navigation/list primitives
- Botster-specific composites

That inventory is the first real contract. Everything else should build on it.
