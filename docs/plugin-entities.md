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

## Shipping A Model

Use this sequence when a plugin owns data that must survive reloads and appear
in a shared UI:

1. Declare the private durable schema in `plugin.db{}` at plugin load time.
   Put tables, constraints, additive migrations, and indexes there; keep this
   shape optimized for persistence and plugin invariants.
2. Mutate the database through plugin-owned model/repository functions. Validate
   inputs there, write audit/history rows there, and publish entity deltas after
   successful commits.
3. Put the client-facing contract in an `entity_contract.lua` module. It should
   name each `<plugin>.<type>` family and document the read-model fields screens
   bind, without exposing the raw table schema.
4. Build publishers in `entities.lua` that project database rows and hub state
   into normalized read models, register those families with
   `lib.entity_broadcast`, and publish snapshots or targeted deltas.
5. Keep transient presentation in `ui.local_state` plus
   `botster.presentation.*`. Modal open flags, disclosure, focus, and pending
   controls are per-client state, not `plugin.db` rows or plugin entities.

`project_pipelines/db.lua`, `repo.lua`, `entity_contract.lua`, and
`entities.lua` are the reference layout for entity-backed plugin templates.

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

The planned Cloudflare stable-url plugin publishes
`cloudflare-stable-urls.stable_url` as the read model for stable webhook URL
claims. Its public fields and forbidden secret-bearing fields are specified in
[`specs/stable-webhook-url-contracts.md`](specs/stable-webhook-url-contracts.md#stable-url-entity-contract).

Keep plugin model names and entity names aligned, but do not expose private
table shape by accident. Use singular entity type names for one record family
(`project-pipelines.ticket`, not `project-pipelines.tickets`), and keep record
fields flat, scalar-friendly, and renderer-ready. Relationship ids, labels,
status tones, button ids, navigation paths, and small booleans such as
`has_terminal` belong on the read model when screens need them. Large nested
graphs, decoded private JSON blobs, SQL column names that only make sense
inside `plugin.db`, and renderer-specific view objects do not.

Plugin modules should make the split obvious:

- `db.lua` owns tables, migrations, constraints, and persistence names.
- `repo.lua` owns validation, mutations, and private lookup helpers.
- `entity_contract.lua` owns published `<plugin>.<type>` names and the field
  shape screens may bind.
- `entities.lua` owns projection from private rows and hub state into client
  read models.

When a screen needs a field that does not exist yet, add a projection field to
`entities.lua` and document it in `entity_contract.lua`. Do not reconstruct it
from raw repo rows in the browser or hide it in a route-specific tree snapshot.

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
  query = function(request, context)
    local id = request.id
    if id then
      return rows("SELECT * FROM tickets WHERE id = ? LIMIT 1", id)
    end
    return ticket_rows()
  end,
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

Broad baseline requests include only default entity families. Register large,
historical, or detail-only families with `default = false` and expose them
through `query(request, context)` instead:

```lua
EB.register("project-pipelines.run_step", {
  id_field = "id",
  default = false,
  all = function(context) return run_step_rows(context) end,
  query = function(request, context) return run_step_rows_for_request(request, context) end,
})
```

Explicit client requests for that entity type still work. The flag only keeps
unrequested detail families out of broad hub/browser hydration.

Browser surfaces request plugin data by inspecting the received UI tree for
`$bind` and `bind_list` sources. A surface that binds an unfiltered
`/project-pipelines.ticket` list pulls a full `project-pipelines.ticket`
snapshot when that surface is opened, not during hub subscribe. A surface that
binds `/project-pipelines.ticket/ticket_123/title` sends a targeted id request.
A `ui.bind_list{ source = "/project-pipelines.run_step", where = { run_id =
"run_123" } }` sends a scoped request for that exact top-level field set.

Patch semantics are intentionally shallow: nested tables replace the old nested
value. Send the full nested value you want clients to keep.

### Targeted And Scoped Hydration

Plugins that expose entity-backed detail or filtered overview screens should
register a `query(request, context)` provider for each entity family used by
concrete id bindings or filtered `bind_list` sections. The provider returns the
same read-model row shape as `all()`, but only for the requested route or
working set.

Supported request shapes are intentionally separate:

- `{ id = "record-id" }` means merge this one record into the client store.
  If the provider returns no visible row for that id, Botster sends an
  `entity_remove` for that id so stale client rows disappear.
- `{ where = { field = value, ... } }` means replace only the client rows whose
  top-level fields exactly match that scope. Botster sends an
  `entity_scoped_snapshot` frame and preserves unrelated rows for the same
  entity family.

Do not mix `id` and `where` in one request. The hub rejects mixed requests so
id merge semantics and scoped replacement semantics stay unambiguous. `where`
values are scalar only (`string`, `number`, or `boolean`) and match exact
top-level read-model fields. If a screen filters on `latest_run_status`,
`standalone`, `ticket_id`, or `has_pr_url`, those fields must exist on the
entity records returned by both `all()` and `query()`.

Scoped snapshots do not advance the whole-family sequence gate. Browser stores
therefore maintain a local render revision counter so same-size scoped
replacements still re-render. TUI stores apply the same scoped replacement
semantics. Unsupported query providers emit no authoritative empty frame; they
log and leave existing client state alone. That makes missing providers safe,
but plugin authors should still add query providers for every scoped list or
concrete id binding that must hydrate a route directly.

Use the optional `context` table passed to `all()` and `query()` to share
expensive lookups across one batch. Keep the context ephemeral and derived; it
is for amortizing read-model projection work, not storing durable state.

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
Use `ui.bind_if(path, node)` inside a bound template for optional row children
such as session buttons; the condition should be a plain model field like
`has_terminal`, while paths and labels stay as separate entity fields.

Missing scalar values resolve to `null`. Missing list sources resolve to `[]`.
Bindings should name the read-model fields the UI expects. If a route needs
derived labels, status tones, counts, or navigation paths, publish those fields
on the entity record instead of teaching one renderer how to reconstruct them
from private tables.

UI screens must not read plugin repositories during render to fetch dynamic
rows that are already model state. Render-time repo reads are allowed only for
structural scaffolding that is not yet represented as entities, and those
exceptions must be named in the plugin contract with a migration source. Once a
section is entity-backed, keep it entity-backed: mutators publish
`entity_upsert`, `entity_patch`, `entity_remove`, or targeted snapshots instead
of forcing `ui_tree_snapshot` refreshes for data-only changes.

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

Modal field values that are only browser presentation state follow the same
boundary. Keep draft text, selected radio values, temporary filters, and other
not-yet-submitted modal controls in browser-local state or native form state
until the user submits an action. Persist only the submitted workflow fact
through the plugin repo and publish the resulting entity delta. Do not mirror
draft modal fields into plugin entities or `plugin.db` so another client can
see half-authored local input.

## Removing Old Paths

Entity-backed migrations are cold-turkey at the section boundary. When a screen
section moves to plugin entities, remove the old repo-rendered data path,
browser-only store, custom refresh command, legacy snapshot dependency, stale
doc example, and test allowance in the same slice. Do not leave v1/v2 names,
compatibility shims, or "temporary" dual read paths unless the ticket has an
explicit human-approved compatibility requirement.

For Project Pipelines, the desired direction is one canonical entity-backed
path per migrated section. `repo_rendered_screens` exists to name remaining
exceptions, not to normalize permanent mixed rendering.

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
- Keep entity type names singular, namespaced, and owned by the registering
  plugin; keep published records flat and renderer-ready.
- Register every entity family before publishing snapshots or deltas.
- Mark large/detail/history families `default = false` so broad baselines stay
  bounded; hydrate them through explicit type or scoped `query()` requests.
- Keep `all()` callbacks array-shaped and resilient; bad records are skipped.
- Add `query(request, context)` providers for concrete id bindings and filtered
  `bind_list` scopes; return the same read-model shape as `all()`.
- Publish snapshots for baselines and targeted deltas for mutators.
- Use `entity_patch` only for top-level sparse changes.
- Keep `ui_tree_snapshot` focused on route structure, stable node ids, and
  controls; bind durable values from entity stores.
- Do not read plugin repos at UI render time for dynamic model rows once an
  entity family owns that section.
- Use `ui.local_state` and `botster.presentation.*` for per-browser modal,
  disclosure, focus state, and not-yet-submitted modal field values.
- Use `ui.bind_list{ where = { ... } }` for filtered child and overview
  collections only when the filtered fields are explicit top-level read-model
  fields and the entity family has a matching `query` provider.
- Give repeated submitters stable node `id` values so `ui_action_result`
  feedback scopes to the clicked control.
- Remove dead dual paths cold-turkey when a section migrates to entities.
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
./test.sh --integration -- project_pipelines_entity_contract
```

Do not run raw `cargo test` for CLI verification; `cli/test.sh` sets the test
environment expected by Botster.
