# Plugin Entities

Plugin entities are the canonical model for plugin-owned dynamic state in
Botster. A plugin publishes durable records from the hub/Lua runtime, and every
client consumes those records through the same entity frames before rendering a
Lua-authored UI tree.

This keeps the architecture explicit:

- hub/Lua owns plugin state, persistence, validation, and action handlers
- browser and TUI clients are equal consumers of entity frames and UI nodes
- `ui_tree_snapshot` carries presentation and control structure only
- renderer-specific stores, commands, or refresh paths do not own plugin model
  data

## Model Boundary

Plugins ship their model through hub-owned Lua modules, not through
client-specific stores:

| Layer | Owns | Does not own |
|---|---|---|
| `plugin.db` schema and migrations | durable plugin persistence, constraints, migration history | client subscriptions, renderer cache shape, modal/disclosure state |
| entity read models | normalized client-facing records derived from persistence and hub state | private SQL table shape, per-route tree snapshots, renderer components |
| `ui.bind` / `ui.bind_list` | references from Lua-authored UI nodes into entity records | persistence, validation, or a second copy of plugin data |
| local presentation state | per-client UI affordances such as modals, disclosure, focus, pending controls | durable workflow facts or records another client must see |

That means a plugin that needs durable models should include:

- a `plugin.db{}` declaration for tables, migrations, and durable writes
- repository/model functions that validate and mutate that database
- entity read-model publishers that expose stable, normalized record families
  through `entity_snapshot`, `entity_upsert`, `entity_patch`, and
  `entity_remove`
- Lua UI trees that bind to those entity families with explicit paths

Do not ship a plugin-specific browser store, custom subscription channel, or
renderer refresh command as the model layer. Clients may cache entity frames in
their own runtime structures, but the shared data contract remains the entity
read model. If the UI needs fields that do not match the table shape, add them
deliberately in the read-model publisher so the contract is visible and shared
by browser and TUI renderers.

## Canonical Model

A plugin entity type is a namespaced record family:

```text
<plugin>.<type>
```

Examples from Project Pipelines:

- `project-pipelines.ticket`
- `project-pipelines.pipeline_step`
- `project-pipelines.pipeline_gate`
- `project-pipelines.run_step`
- `project-pipelines.question`

The plugin namespace must match the registering plugin. Records use a non-empty
string `id` field. The first segment in a binding path is the entity type
itself, so `/project-pipelines.ticket` means the ticket collection and
`/project-pipelines.ticket/ticket_123/title` means one field.

Built-in entity families such as `session`, `workspace`, `spawn_target`,
`worktree`, `hub`, `connection_code`, `template`, and `session_action` are
reserved for core.

## Entity Lifecycle

Register entity families during plugin load with `lib.entity_broadcast`, then
publish snapshots and deltas from mutator paths. Treat these publishers as the
explicit contract/read-model layer between private persistence and clients:

```lua
local EB = require("lib.entity_broadcast")
local Hub = require("lib.hub")

local OWNER = "project-pipelines"
local ENTITY_TYPE = "project-pipelines.ticket"

local function ticket_rows()
  -- rows is the local query helper from project_pipelines/entities.lua.
  return rows("SELECT * FROM tickets ORDER BY updated_at DESC, created_at DESC")
end

EB.register(ENTITY_TYPE, {
  id_field = "id",
  owner_plugin = OWNER,
  all = ticket_rows,
})

Hub.get():entity_snapshot(ENTITY_TYPE, ticket_rows(), {
  owner_plugin = OWNER,
})

Hub.get():entity_upsert(ENTITY_TYPE, ticket, {
  owner_plugin = OWNER,
})

Hub.get():entity_patch(ENTITY_TYPE, ticket.id, {
  status = "closed",
}, {
  owner_plugin = OWNER,
})

Hub.get():entity_remove(ENTITY_TYPE, ticket.id, {
  owner_plugin = OWNER,
})
```

Use `entity_snapshot` when a plugin refreshes a whole family or answers an
explicit client baseline request. Use `entity_upsert` when a mutator creates or
replaces one record. Use `entity_patch` for sparse top-level field changes.
Use `entity_remove` when clients should drop one record.

Browser surfaces request their initial plugin baselines by inspecting the
received UI tree for `$bind` and `bind_list` sources. A surface that binds
`/project-pipelines.ticket` therefore pulls the `project-pipelines.ticket`
snapshot when that surface is opened, not during hub subscribe.

Patch semantics are intentionally shallow: nested tables replace the old nested
value. Send the full nested value you want clients to keep.

## UI Binding Lifecycle

Lua-authored screens render stable primitive trees and bind dynamic fields from
the entity store:

```lua
ui.list{ children = {
  ui.bind_list{
    source = "/project-pipelines.ticket",
    where = { project_id = project_id },
    item_template = ui.list_item{
      id = ui.bind("@/id"),
      action = ui.action("botster.nav.open", {
        path = ui.bind("@/path"),
      }),
      title = {
        ui.text{ text = ui.bind("@/title"), size = "sm", weight = "semibold" },
      },
      subtitle = {
        ui.text{ text = ui.bind("@/description"), size = "xs", tone = "muted" },
      },
    },
  },
} }
```

`ui.bind_list` expands to ordinary sibling nodes in browser and TUI renderers.
`where` filters are exact matches against top-level record fields. `@/...`
bindings are valid only inside the `item_template`; outside a list template,
bind against the absolute entity path.

Missing scalar values resolve to `null`. Missing list sources resolve to `[]`.
Bindings should name the read-model fields the UI expects. If a route needs
derived labels, status tones, counts, or navigation paths, publish those fields
on the entity record instead of teaching one renderer how to reconstruct them
from private tables.

## Presentation State Boundary

Plugin entities are for durable, shared model state. Browser-local presentation
state belongs in `ui.local_state(key, default)` and the `botster.presentation.*`
actions instead. Use that path for modal open flags, disclosure toggles, and
other per-client UI state that should not reload a plugin route or publish to
other clients.

```lua
ui.button{
  text = "Open dialog",
  action = ui.action("botster.presentation.set", {
    key = "ticket-123-dialog-open",
    value = true,
  }),
}

ui.dialog{
  open = ui.local_state("ticket-123-dialog-open", false),
  title = "Dialog",
  children = { ... },
}
```

Do not add plugin entity families, plugin DB tables, route segments, or
client-specific refresh commands just to open or close local UI. Mutators that
finish hub-side work may return
`action.result{ presentation = { clear = { "ticket-123-dialog-open" } } }` to
reset local presentation keys after success.

## Action Feedback Lifecycle

Button and icon-button submitters use the generic `ui_action` request/response
path. The renderer creates an `action_request_id`, marks only the activating
submitter pending, and sends the action envelope to the hub:

```json
{
  "type": "ui_action",
  "target_surface": "project_pipelines",
  "action_request_id": "ua_...",
  "envelope": {
    "id": "project_pipelines.create_ticket",
    "payload": {}
  }
}
```

Lua handlers return `action.HANDLED` for generic success or
`action.result{...}` for a semantic result:

```lua
local action = require("lib.action")

action.on("project_pipelines.create_ticket", "project_pipelines.create_ticket", function(envelope, ctx)
  local ticket = repo.create_ticket(...)
  refresh(ctx)

  return action.result{
    message = "Ticket created.",
    navigate = { label = "Open ticket", path = "/pipelines/tickets/" .. ticket.id },
  }
end)
```

The hub echoes the request id in `ui_action_result`. Pending, success, error,
and optional navigation rendering belong to each client renderer. Lua returns
semantic result data only; it does not send CSS classes, DOM attributes, or
renderer-specific pending flags.

## Authoring Checklist

- Use `<plugin>.<type>` entity names and pass `owner_plugin` when registering or
  publishing outside the normal plugin load context.
- Use a non-empty string `id` on every record.
- Ship durable plugin models as `plugin.db` schema/migrations plus entity
  read-model publishers; do not add plugin-specific browser stores.
- Register every entity family before publishing snapshots or deltas.
- Keep `all()` callbacks array-shaped and resilient; bad records are skipped.
- Publish snapshots for baselines and targeted deltas for mutators.
- Use `entity_patch` only for top-level sparse changes.
- Keep `ui_tree_snapshot` focused on route structure, stable node ids, and
  controls; bind durable values from entity stores.
- Use `ui.local_state` and `botster.presentation.*` for per-browser modal,
  disclosure, or focus state.
- Use `ui.bind_list{ where = { ... } }` for filtered child collections.
- Give repeated submitters stable node `id` values so `ui_action_result`
  feedback scopes to the clicked control.
- Prefer Project Pipelines as the reference plugin:
  `catalog/templates/plugins/project-pipelines/project_pipelines/entities.lua`,
  `web/screens/home.lua`, `web/screens/project.lua`, and `web/actions.lua`.

## Testing Guidance

For documentation and authoring changes, verify that examples name real modules,
helpers, and paths:

```bash
rg -n "lib.entity_broadcast|entity_snapshot|ui.bind_list|action.result" catalog/templates/plugins/project-pipelines cli/lua
rg -n "entity_snapshot|entity_upsert|entity_patch|entity_remove|bind_list|ui_action_result|action_request_id" docs cli/src/ui_contract catalog/templates/plugins/project-pipelines
```

For implementation changes, use the repo test script:

```bash
cd cli
./test.sh --unit -- ui_contract
./test.sh --integration -- table_renders_rows_from_plugin_entity_bind
```

Do not run raw `cargo test` for CLI verification; `cli/test.sh` sets the test
environment expected by Botster.
