# Adaptive UI Viewport And Presentation

## Goal

Define one renderer-neutral adaptive layout contract for Botster surfaces so authors can target:

- compact vs expanded screens in the web UI
- small vs large terminals in the TUI

without writing raw CSS breakpoints or terminal column checks in every surface.

This spec extends the shared UI contract in [cross-client-ui-primitives.md](cross-client-ui-primitives.md).

## Design Rule

Authors should branch on semantic viewport classes, not device names or pixel values.

Good:

- compact
- regular
- expanded
- coarse pointer
- fine pointer
- short height
- tall height

Bad:

- iPhone
- desktop
- `max-width: 768px`
- `cols < 100`

The renderer computes exact breakpoints. The shared contract only exposes stable semantic classes.

## Shared Viewport Context

Every surface renderer should receive:

```ts
type UiViewport = {
  widthClass: "compact" | "regular" | "expanded"
  heightClass: "short" | "regular" | "tall"
  pointer: "none" | "coarse" | "fine"
  orientation?: "portrait" | "landscape"
  keyboardOccluded?: boolean
}
```

Rules:

- web derives this from browser viewport size, input mode, and visible viewport state
- TUI derives this from terminal cols and rows
- `pointer` supersedes the older boolean pointer capability; `UiCapabilitySet.hover` remains separate
- `orientation` is optional in `current` and may be omitted until a surface actually consumes it
- `keyboardOccluded` is optional and primarily useful for web mobile keyboards
- surfaces must not depend on raw viewport numbers unless a renderer-specific hint is explicitly needed

## Renderer Mapping

These are implementation guidelines, not Lua-visible APIs.

### Web

Web should derive `UiViewport` from:

- current visible viewport width and height
- pointer media query or equivalent input heuristics
- orientation
- visual viewport keyboard occlusion when available

Recommended behavior:

- use the visible viewport, not only layout viewport, when mobile keyboards are open
- recompute on resize, orientation change, and visual viewport change

### TUI

TUI should derive `UiViewport` from:

- terminal columns
- terminal rows

Recommended behavior:

- `widthClass` reflects usable content width, not host monitor size
- `heightClass` reflects usable rows after overlays or chrome if applicable
- `pointer` is usually `none`

## Recommended Default Thresholds

These defaults are non-normative. Renderers own the exact thresholds, but the first implementation should start from one shared baseline instead of guessing.

### Web defaults

- width: `compact < 640`, `regular 640-1023`, `expanded >= 1024`
- height: `short < 700`, `regular 700-999`, `tall >= 1000`

### TUI defaults

- width: `compact < 80 cols`, `regular 80-119 cols`, `expanded >= 120 cols`
- height: `short < 24 rows`, `regular 24-39 rows`, `tall >= 40 rows`

These are starting points only. The renderer may tune them after real surface testing.

## Semantic Size Classes

These classes should drive authored surface decisions.

### Width classes

- `compact`
  - single-column layouts
  - no persistent secondary pane
  - larger targets
  - fewer inline details
- `regular`
  - moderate multi-region layouts
  - some secondary metadata visible
- `expanded`
  - split-pane layouts
  - persistent navigation or detail panes
  - more metadata and secondary actions visible

### Height classes

- `short`
  - avoid tall overlays
  - reduce stacked chrome
  - prioritize primary content
- `regular`
  - standard behavior
- `tall`
  - allow richer overlays and more secondary context

## Shared Adaptive Helpers

The authoring layer should expose four core adaptive tools.

### 1. Responsive values

```lua
ui.responsive({
  compact = "vertical",
  expanded = "horizontal",
})
```

Rules:

- keyed by semantic viewport classes, not pixels
- fallback order for width: exact match, then next smaller class, then next larger class
- fallback order for height: exact match, then next smaller class, then next larger class
- can be used for layout direction, density, visibility, and presentation

### 2. Conditional rendering

```lua
ui.when("expanded", sidebar_node)
ui.when({ width = "compact" }, compact_toolbar)
```

Rules:

- bare string shorthand means `widthClass`
- table form is preferred for anything more complex than width-only checks
- conditions must be semantic
- renderers may skip entire branches when conditions fail

### 3. Hidden rendering

```lua
ui.hidden({ width = "compact" }, metadata_node)
```

Rules:

- same condition semantics as `ui.when`
- intended for low-priority secondary content

### 4. Presentation policy

```lua
ui.dialog({
  title = "Rename Workspace",
  presentation = "auto",
})
```

Allowed values:

- `auto`
- `inline`
- `overlay`
- `sheet`
- `fullscreen`

`auto` is preferred. Renderers should choose the best native presentation for the current viewport.

## Presentation Resolution Rules

When `presentation = "auto"`:

### Menus

- `expanded` + `fine` pointer
  - popover/dropdown behavior is allowed
- `compact` or `coarse` pointer
  - sheet or full-width action list is preferred
- TUI
  - overlay/menu panel bound to selection or focus

### Dialogs

- `expanded`
  - centered overlay dialog
- `compact`
  - sheet or fullscreen modal
- `short`
  - prefer fullscreen over centered overlay

### Secondary panes

- `expanded`
  - persistent split pane allowed
- `compact`
  - convert to drill-in navigation or stacked sections

## Interaction Density And Hit Targets

Interaction density is shared, but the renderer should adapt it through viewport context.

```ts
type UiInteractionDensity = "compact" | "comfortable"
```

This name intentionally avoids collision with the web runtime phase-1 surface variant `Density = "sidebar" | "panel"`.

Recommended behavior:

- `compact` width does not necessarily mean `compact` density
- on web with `pointer = coarse`, prefer `comfortable`
- on TUI, `compact` density is usually acceptable because keyboard navigation dominates

So authors should be allowed to say:

```lua
interactionDensity = ui.responsive({
  compact = "comfortable",
  expanded = "compact",
})
```

## Priority-Based Content Collapse

Compound rows and panels should collapse by priority instead of disappearing ad hoc.

Recommended levels:

### Priority 1

Must remain visible:

- primary title
- selection state
- primary action

### Priority 2

Hide when space is tight:

- subtitle
- badges
- non-critical metadata

### Priority 3

Move into menus or details:

- tertiary metadata
- destructive or secondary actions
- extended descriptions

This should be a surface-authoring convention even if it is not a first-class primitive prop yet.

## Split Layout Policy

For surfaces with navigation plus detail, use this default policy.

### Expanded width

- allow persistent sidebar plus detail panel
- allow preview panes and inspector panes

### Regular width

- allow narrower sidebars or stacked major sections
- avoid triple-pane layouts

### Compact width

- one dominant pane at a time
- navigation becomes drill-in, tabs, or stacked sections
- terminal and editor-like surfaces should take full width

## Workspace And Session Surface Rules

The agent and workspace UI should follow these adaptive defaults.

### Expanded

- persistent workspace navigation is allowed
- row metadata can include title line and subtext
- row action menus may be inline or trailing
- preview error panels may render inline beneath rows

### Compact

- workspace and session content should collapse into one main column
- row hit targets should increase
- secondary metadata should collapse before primary naming
- menus should use sheet-style presentation instead of hover-first popovers
- selected session detail should navigate or replace the list region instead of assuming split panes

## Terminal Surface Rules

Terminal surfaces need extra rules because usable space is fragile.

### Compact width

- terminal should dominate the surface
- secondary panels should move behind menus or overlays
- persistent chrome should shrink

### Keyboard occlusion

- when `keyboardOccluded = true`, avoid fixed footers or overlays that hide the terminal input region
- resize to the visible viewport where possible

### Short height

- prefer fullscreen presentations for dialogs and menus
- suppress non-essential header/footer chrome

## API Shape For Authors

Recommended author-facing context:

```lua
ctx.viewport.width_class
ctx.viewport.height_class
ctx.viewport.pointer
ctx.viewport.orientation
ctx.viewport.keyboard_occluded
```

Recommended author-facing helpers:

```lua
ui.responsive(map)
ui.when(condition, node)
ui.hidden(condition, node)
```

The goal is to keep surface code declarative instead of embedding layout math.

## Optimizations

### 1. Renderer owns breakpoints

Authors should never need to know the actual pixel or terminal-column cutoffs.

That lets:

- web tune breakpoints without changing authored surfaces
- TUI tune thresholds for 80x24, 120x40, and larger terminals without changing authored surfaces

### 2. Use one viewport model for both clients

Do not create separate `mobile` and `terminal_small` concepts. They are both compact surfaces.

### 3. Presentation is a policy, not a component fork

Use one `dialog` primitive with adaptive presentation instead of separate `dialog`, `sheet`, and `fullscreen_modal` primitives.

### 4. Prefer semantic degradation

The adaptation should be:

- split pane -> stacked pane
- popover -> sheet
- inline metadata -> collapsed metadata

not:

- entirely different surface definitions per client

## Non-Goals

- pixel-perfect parity between web and TUI
- exposing raw renderer breakpoints in the shared contract
- replacing all existing responsive Rails view code immediately
- solving every terminal size edge case before the first adaptive runtime lands

## Recommended Immediate Follow-Up

1. Add `ctx.viewport` and `ui.responsive()` to the shared authoring model.
2. Keep the phase-1 web React island on its existing `sidebar` and `panel` surface variants.
3. Add `UiViewport` consumption to the web runtime as a phase-2 follow-on.
4. Add a small TUI adapter that derives the same viewport classes from terminal rows and cols.
5. Use the workspace/session surface as the first adaptive reference implementation.
