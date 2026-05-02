# Plan: Document Plugin Entity Architecture And Authoring Guide

## Context And Constraints

Ticket: `ticket_1777662642_447001` - Document plugin entity architecture and
authoring guide.

Ticket description: update Botster docs, Lua primitive references, UI contract
docs, and Project Pipelines docs for plugin-owned entities, action lifecycle
feedback, and cold-turkey plugin UI migration expectations. Include examples
for plugin authors and testing guidance. The new architecture must be
documented as canonical, not an optional legacy-compatible mode.

Hard constraints from repo/vault:

- Hub/Lua owns authoritative plugin state and action handlers; browser and TUI
  are equal clients consuming shared entity frames and renderer-neutral UI nodes.
- Plugin durable state flows through `entity_snapshot`, `entity_upsert`,
  `entity_patch`, and `entity_remove`; do not document browser-only stores,
  plugin-specific list snapshots, or refresh commands as acceptable.
- `ui_tree_snapshot` is presentation/control state only. It must not compete
  with plugin-owned entity frames for durable model state.
- UI examples must use the shipped shared primitives: Lua `ui_contract` nodes,
  `ui.bind`, `ui.bind_list`, generic `ui_action` feedback, Catalyst-backed web
  rendering for Botster primitive surfaces, and ratatui-backed TUI rendering.
- No visible UI work is planned. `tmp/tailwind_plus_preview` is absent in this
  worktree; if implementation touches visible primitive styling unexpectedly,
  use existing Catalyst primitives and `app/frontend/ui_contract/registry.tsx`
  as the design source.
- CLI verification must use `cd cli && ./test.sh ...`, never raw `cargo test`.
- Cold-turkey convention applies: delete or rewrite obsolete prose; do not add
  `legacy`, `v1`, `v2`, or compatibility-mode framing around the old model.

## Doc Surface Inventory

### 1. Botster Docs

Add `docs/plugin-entities.md`.

TOC:

- `# Plugin Entities`
- `## Canonical Model`: hub/Lua publishes durable records; clients render from
  generic entity stores.
- `## Entity Lifecycle`: register, snapshot, upsert, patch, remove, and
  subscribe-time baseline behavior.
- `## UI Binding Lifecycle`: `ui_tree_snapshot` as presentation, `ui.bind` /
  `ui.bind_list` as data references, exact-match `where` filters.
- `## Action Feedback Lifecycle`: generic `ui_action`, `action_request_id`,
  `action.result`, `ui_action_result`, pending/success/error ownership.
- `## Authoring Checklist`: namespace, string `id`, stable node ids, top-level
  patch semantics, filtered child lists, no browser-only stores.

Update `README.md`.

TOC changes:

- Add a short cross-reference from `## Plugins` to `docs/plugin-entities.md`.
- Keep the high-level README concise; no full duplicate guide.
- Describe plugin UI as Lua-authored shared primitives plus entity frames.

Update `docs/lua-architecture-vision.md`.

TOC changes:

- Add a current architecture note after the Lua layer overview explaining that
  plugin state is published as entity frames.
- Rewrite old "web routes and templates / state management" language so it
  points to shared UI primitives and entity broadcasts for plugin UI.
- Avoid phase-number or old-runtime framing for plugin entity docs.

### 2. Lua Primitive References

Update `docs/lua/primitives.md`.

TOC changes:

- Expand `## Plugin-Owned Entities` into the canonical Lua author reference.
- Add minimal plugin author examples for:
  - `lib.entity_broadcast.register`
  - `Hub.get():entity_snapshot`
  - `entity_upsert`, `entity_patch`, `entity_remove`
  - `ui.bind` and filtered `ui.bind_list`
  - handler returns through `action.HANDLED` and `action.result`
- Add gotchas: owner namespace matching, reserved built-in entity names,
  required string `id`, snapshot `all()` arrays, top-level patch merge only,
  and broadcaster errors being isolated from mutators.
- Cross-link `docs/lua/session-actions.md` for per-session capabilities.

Update `docs/lua/session-actions.md`.

TOC changes:

- Add a short section distinguishing `session_action` entities from arbitrary
  plugin-owned domain entities.
- Link generic invocation feedback to the `ui_action` lifecycle documented in
  `docs/lua/primitives.md` and the new authoring guide.

### 3. UI Contract Docs

Update `cli/src/ui_contract/README.md`.

TOC changes:

- In `Wire protocol -- $bind grammar`, add `where` filtering to the primary
  `ui.bind_list` example.
- State that plugin entity names occupy the first path segment directly and
  are not nested under `/plugin`.
- Add author-facing warnings that `@/...` is only valid inside a list template
  and that missing values resolve to `null` / `[]`.

Update `docs/specs/cross-client-ui-primitives.md`.

TOC changes:

- Strengthen `Domain state` and `Optimizations` to say entity frames are the
  only durable model-state path for plugin UI.
- Add an example mapping `/project-pipelines.ticket` and
  `/project-pipelines.ticket/ticket_123/title`.
- Add `bind_list where` filtering and sibling flattening to the shared
  browser/TUI requirements.
- Confirm `ui_tree_snapshot` remains presentation/control state only.

Update `docs/specs/web-ui-primitives-runtime.md`.

TOC changes:

- Strengthen current wording that plugin-owned state uses generic entity stores
  and selectors, not plugin-specific Zustand hooks or browser-only snapshots.
- Make the generic `ui_action` feedback lifecycle the only documented pending
  and result path for Lua-authored primitive submitters.
- Keep React/Catalyst as the web primitive runtime and avoid adding
  Hotwire/Stimulus/Elements instructions for Lua-authored plugin surfaces.

### 4. Project Pipelines Docs

Update `catalog/templates/plugins/project-pipelines/README.md`.

TOC changes:

- Keep `project_pipelines/entities.lua` as the concrete case study for the
  canonical entity authoring pattern.
- Add a `Plugin Entity Case Study` subsection explaining each entity family,
  why steps/gates are first-class records, and when targeted deltas are used.
- Add examples for overview lists, filtered detail children, stable submitter
  `id`s, action result messages, and plugin-owned sessions.
- Replace transitional language that says detail screens are "still" waiting
  for migration with canonical guidance: use `ui.bind_list` where supported;
  only use presentation snapshots for non-data route scaffolding.

Update `docs/project-pipelines-verification.md`.

TOC changes:

- Add a `Plugin Entity Documentation Verification` entry for this ticket.
- Record exact static checks and any runnable checks performed by the
  implementation step.
- Note that Project Pipelines is the reference plugin for entity-backed
  dynamic state and generic action lifecycle feedback.

## Cold-Turkey Edits

The implementation should replace these old or transitional passages instead of
annotating them as a second supported mode:

- `docs/specs/web-ui-primitives-runtime.md`: early "template cloning plus
  Stimulus reconciliation" and phase-1 migration language should remain only as
  historical motivation for the runtime, not as a plugin authoring option.
- `docs/specs/web-ui-primitives-runtime.md`: "Why Composites Stay Internal In
  Phase 1", "Acceptance Criteria For Phase 1", and "Immediate Implementation
  Sequence" should not be presented as the current authoring path for
  Lua-authored plugin surfaces.
- `catalog/templates/plugins/project-pipelines/README.md`: wording that detail
  screens "still render presentation snapshots until migrated" should be
  rewritten so entity-backed bindings are canonical and snapshots are limited
  to route/presentation scaffolding that is not durable data.
- `docs/lua-architecture-vision.md`: old "web routes and templates / state
  management" wording should be updated to the current entity/UI contract split.
- Search for `browser-only`, `plugin-specific list snapshot`, `template
  cloning`, `legacy`, `v1`, `v2`, and `ui_tree_snapshot` in the touched docs and
  remove any prose that frames obsolete plugin state paths as acceptable.

## Plugin Author Examples

Use Project Pipelines as the real, repository-backed example:

- `catalog/templates/plugins/project-pipelines/project_pipelines/entities.lua`
  demonstrates entity family registration and publishing.
- `catalog/templates/plugins/project-pipelines/project_pipelines/web/screens/home.lua`
  demonstrates `ui.bind_list` for overview collections.
- `catalog/templates/plugins/project-pipelines/project_pipelines/web/screens/project.lua`
  demonstrates filtered child collections.
- `catalog/templates/plugins/project-pipelines/project_pipelines/web/actions.lua`
  demonstrates action handlers and result feedback.

Inline examples in markdown should be minimal and mirror those source files
rather than inventing a separate sample plugin. They must cover:

- initial publish through `entity_snapshot`
- targeted change through `entity_upsert`
- field update through `entity_patch`
- removal through `entity_remove`
- filtered `ui.bind_list`
- submitter pending/success/error through generic `ui_action_result`

## Dependency Coverage Map

| Closed dependency ticket | Documentation coverage |
|---|---|
| Add hub plugin entity publishing APIs | `docs/plugin-entities.md`, `docs/lua/primitives.md` entity lifecycle |
| Extend browser entity store for plugin entity types | `docs/specs/web-ui-primitives-runtime.md`, `docs/plugin-entities.md` client consumption |
| Add UI binding support for plugin entities | `cli/src/ui_contract/README.md`, `docs/specs/cross-client-ui-primitives.md`, `docs/plugin-entities.md` binding examples |
| Add generic UI action pending and result feedback | `docs/lua/primitives.md`, `docs/specs/web-ui-primitives-runtime.md`, `docs/plugin-entities.md` action lifecycle |
| Add TUI consumption for plugin entities | `docs/specs/cross-client-ui-primitives.md`, `docs/plugin-entities.md` browser/TUI parity |
| Migrate Project Pipelines dynamic state to plugin entities | `catalog/templates/plugins/project-pipelines/README.md`, `docs/project-pipelines-verification.md` case study |
| Polish Project Pipelines UI on entity model | `catalog/templates/plugins/project-pipelines/README.md`, `docs/specs/web-ui-primitives-runtime.md` UI convention guardrails |

## Implementation Sequence

1. Replace this stale `PLAN.md` with this ticket plan and attach it as a
   Project Pipelines artifact.
2. Add `docs/plugin-entities.md` as the canonical concept and authoring guide.
3. Update `docs/lua/primitives.md` and `docs/lua/session-actions.md` so the Lua
   primitive reference points authors to the canonical entity and action model.
4. Update `cli/src/ui_contract/README.md`,
   `docs/specs/cross-client-ui-primitives.md`, and
   `docs/specs/web-ui-primitives-runtime.md` so shared/browser/TUI UI contract
   docs agree on entity paths, bindings, filters, and generic action feedback.
5. Update `catalog/templates/plugins/project-pipelines/README.md` as the
   concrete case study and remove transitional snapshot-migration language.
6. Update `README.md`, `docs/lua-architecture-vision.md`, and
   `docs/project-pipelines-verification.md` with cross-references and
   verification notes.
7. Run cold-turkey cleanup searches and rewrite any remaining prose that treats
   pre-entity plugin UI/state paths as supported.
8. Run verification checks and attach the results to the implementation gate.

## Verification Plan

Static checks:

```bash
rg -n "entity_snapshot|entity_upsert|entity_patch|entity_remove|bind_list|ui_action_result|action_request_id" docs cli/src/ui_contract catalog/templates/plugins/project-pipelines
rg -n "browser-only|plugin-specific list snapshot|template cloning|legacy|v1|v2|ui_tree_snapshot" docs cli/src/ui_contract catalog/templates/plugins/project-pipelines
rg -n "lib.entity_broadcast|entity_snapshot|ui.bind_list|action.result" catalog/templates/plugins/project-pipelines cli/lua
```

Doc link/path checks:

- Confirm every path referenced in docs exists with `rg --files`.
- Confirm every Lua module and helper named in examples exists:
  `lib.entity_broadcast`, `lib.hub`, `ui.bind`, `ui.bind_list`,
  `action.result`, `project_pipelines/entities.lua`,
  `project_pipelines/web/actions.lua`.
- Confirm examples mirror real Project Pipelines source where possible.

Runnable checks:

- If markdown-only edits do not add executable examples, no full CLI test is
  required, but the implementation should record the static checks above.
- If Lua snippets are moved into executable tests or source files, run the
  focused CLI suite through `cd cli && ./test.sh ...`.
- If Project Pipelines template source changes, run the existing relevant
  template/catalog check through `cd cli && ./test.sh --integration -- project_pipelines`.

## Gate Evidence

Plan artifact: `PLAN.md`.

Repo/vault evidence used:

- `AGENTS.md`, `CLAUDE.md`, `README.md`.
- `project_pipelines_current_context` for ticket, dependency, gate, and review
  findings.
- `docs/lua/primitives.md`, `docs/lua/session-actions.md`.
- `docs/specs/cross-client-ui-primitives.md`,
  `docs/specs/web-ui-primitives-runtime.md`.
- `cli/src/ui_contract/README.md`.
- `catalog/templates/plugins/project-pipelines/README.md` and source files
  under `catalog/templates/plugins/project-pipelines/project_pipelines/`.
