# @template Project Pipelines
# @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
# @category plugins
# @dest plugins/project-pipelines/README.md
# @scope device
# @version 1.0.0

# Project Pipelines

Device-level Botster development plugin for project/ticket workflows.

## Shape

The plugin is intentionally split across modules:

- `init.lua` clears hot-reload module cache, registers tools, surfaces, and events.
- `project_pipelines/db.lua` owns the plugin SQLite schema.
- `project_pipelines/repo.lua` owns persistence and audit events.
- `project_pipelines/entities.lua` owns plugin entity read models and publishes dynamic state to clients.
- `project_pipelines/engine.lua` owns run advancement, gates, agent creation, and command gates.
- `project_pipelines/mcp.lua` exposes the agent-facing API.
- `project_pipelines/web/surface.lua` registers routes and sidebar navigation.
- `project_pipelines/web/screens/home.lua` renders the overview.
- `project_pipelines/web/screens/pipelines.lua` renders the pipeline index and edit form.
- `project_pipelines/web/screens/run.lua` renders run details.
- `project_pipelines/web/actions.lua` handles Catalyst UI action envelopes.
- `project_pipelines/web/ui.lua` contains shared UI helpers.
- `project_pipelines/util.lua` contains shared helpers.

## Concepts

- Ticket: durable work item.
- Pipeline: reusable ordered workflow definition.
- Step: one stage in a pipeline. Steps can be `agent`, `command`, or future plugin-owned kinds.
- Gate: requirement that must be satisfied before a step advances.
- Run: one ticket moving through one pipeline.
- Run step: one visit to a pipeline step. A run may visit the same step many
  times when review sends work back for changes.
- Review: structured reviewer output.
- Finding: durable review issue, visible to all agents through context.
- Artifact: durable evidence or external reference.
- Event: append-only audit record of workflow changes.

## Pipeline Definitions

The plugin does not seed default pipelines or sample tickets. Users and agents
create reusable pipeline definitions explicitly through the GUI and MCP tools.
Projects are the durable container for multi-phase or cross-target work; tickets
remain one concrete unit of work in one spawn target.

Pipeline MCP CRUD:

- `project_pipelines_create_pipeline`
- `project_pipelines_list_pipelines`
- `project_pipelines_get_pipeline`
- `project_pipelines_update_pipeline`
- `project_pipelines_delete_pipeline`
- `project_pipelines_create_step`
- `project_pipelines_update_step`
- `project_pipelines_delete_step`
- `project_pipelines_create_gate`
- `project_pipelines_update_gate`
- `project_pipelines_delete_gate`
- `project_pipelines_update_step_agent`

Ticket and project MCP management:

- `project_pipelines_create_ticket`
- `project_pipelines_list_tickets`
- `project_pipelines_get_ticket`
- `project_pipelines_update_ticket`
- `project_pipelines_delete_ticket`
- `project_pipelines_create_project`
- `project_pipelines_list_projects`
- `project_pipelines_get_project`
- `project_pipelines_update_project`
- `project_pipelines_delete_project`
- `project_pipelines_add_project_target`
- `project_pipelines_remove_project_target`
- `project_pipelines_start_run`

`project_pipelines_get_ticket` is the agent-facing ticket status view. It
returns the ticket, project, runs, latest run, latest run steps, associated
sessions, and open findings so agents can see where the work is in the pipeline.
Tickets and projects can only be deleted while they have no history that would
be orphaned.

Deletes are guarded once run history exists, so past runs keep stable pipeline,
step, and gate references.

`project_pipelines_create_pipeline` requires an explicit stable `id` slug. Step
positions are ordered by `position`, then creation time, then ID, so duplicate
positions remain deterministic but should be avoided in authored definitions.
Attestation gates must declare at least one `required_fields` entry. Command
gates must declare a command. Every gate includes a prompt because unmet gates
surface that prompt back to agents; for `review_clear` gates, use the prompt to
explain that blocker/high review findings must be resolved or waived.

Runs keep durable references to pipeline, step, and gate IDs. The current UI and
MCP context render live definition names/prompts for those IDs rather than a
full historical snapshot of the definition text.

Review loops are first-class. A step can define:

- `next_step_id` for normal progression.
- `on_approved_step_id` for the next step after the latest review for the
  current step visit is `approved`.
- `on_changes_requested_step_id` for sending work back after
  `changes_required`.
- `on_blocked_step_id` for explicit blocked review outcomes.

Each activation appends a new run-step visit and stores it as
`runs.current_run_step_id`, so gates, reviews, artifacts, sessions, and the UI
can distinguish repeated visits to the same pipeline step.

Gate evidence is scoped to the current run-step visit: when a loop returns to a
step, attestation and command gates must be satisfied again for that visit.
Review findings are intentionally run-wide until resolved or waived; a
`review_clear` gate blocks on open blocker/high findings from any prior visit so
carryover review issues cannot be bypassed by looping.

Feature decomposition uses projects plus tickets. Agents can call
`project_pipelines_create_child_run` to create child tickets/runs for parallel
slices when a parent run exists. Child runs inherit target/workspace settings
from the parent unless overridden.

## Agent Contract

Agents should call `project_pipelines_current_context` first. The context includes the ticket, run, current step, current run-step visit, gate prompts, run steps, reviews, findings, artifacts, questions, question answers for the calling session, and recent events.

Agents submit evidence with `project_pipelines_submit_gate`, reviews with `project_pipelines_submit_review`, artifacts with `project_pipelines_add_artifact`, and move the run with `project_pipelines_request_step_advance`. If gates are not satisfied, advancement returns structured unmet gate prompts.

Review agents leave findings through `project_pipelines_submit_review`. Blocker and high findings keep `review_clear` gates blocked until each finding is resolved or waived with `project_pipelines_resolve_finding`.

Agents ask for help through project-pipelines tools, not the generic Botster inbox. `project_pipelines_ask_human` creates a durable human question visible in the sidebar and ticket page. `project_pipelines_ask_agent` creates the same durable question and spawns an advisor agent. Answers are read with `project_pipelines_receive_question_answers` or from `project_pipelines_current_context`; question answers wake the asking session with a Project Pipelines notification, not a generic `receive_messages()` inbox doorbell.

Review agents must reject dead code, deprecated code paths, unwired implementation, missing focused tests, and broad "pre-existing failure" excuses. If a failure is not fixed, the reviewer should require exact evidence that it is unrelated to the ticket.

When a run returns to an existing step agent, Project Pipelines sends both a structured task message and a plugin notification pointing the agent back to `project_pipelines_current_context`.

## GUI

The `Pipelines` surface shows tickets, runs, pipeline definitions, selected agent per step, reviews, findings, artifacts, recent events, questions, and plugin-owned sessions. When any question is open, the workspace plugin nav entry for Pipelines shows a notification marker.

### Plugin Entity Case Study

Project Pipelines is the reference plugin for Botster's entity-backed UI model.
`project_pipelines/entities.lua` registers every dynamic workflow record family
under the `project-pipelines.*` namespace, publishes targeted recovery baselines
with `publish_snapshots()`, and exposes targeted `upsert` / `remove` helpers so
repo mutators can update clients after persistence changes. Plugin-owned entity
families are not part of the initial browser/TUI hub baseline; surfaces should
request the specific plugin data they need. The browser does this by inspecting
the opened surface tree for `ui.bind` / `ui.bind_list` sources and requesting
those entity families on demand.

Dynamic state is published as plugin-owned entities:

- `/project-pipelines.ticket`
- `/project-pipelines.project`
- `/project-pipelines.project_target`
- `/project-pipelines.ticket_dependency`
- `/project-pipelines.pipeline`
- `/project-pipelines.pipeline_step`
- `/project-pipelines.pipeline_gate`
- `/project-pipelines.run`
- `/project-pipelines.run_step`
- `/project-pipelines.gate_result`
- `/project-pipelines.review`
- `/project-pipelines.finding`
- `/project-pipelines.artifact`
- `/project-pipelines.question`
- `/project-pipelines.event`

The overview and detail pages render dynamic rows from plugin-owned entities
through shared UI primitives. Tickets, Projects, Pipeline Definitions, Recent
Runs, ticket handoffs, ticket questions, run steps, run reviews, findings,
artifacts, events, pipeline steps, and pipeline gates use `ui.bind_list` /
`ui.bind` against the entity families above. Detail subsections scope child rows
with `ui.bind_list{ where = { ... } }` instead of pre-rendering per-run or
per-step collections into the tree snapshot. `/project-pipelines.ticket`
publishes standalone and project-scoped tickets; standalone lists and project
timelines filter the shared entity family in the view layer. Route scaffolding
and form controls that depend on the current path or available actions still
render structurally.
Pipeline steps and gates are first-class entities, not embedded arrays on the
pipeline entity, because the editor mutates individual step and gate fields and
detail screens need row-level handoff updates. Snapshot publishing is reserved
for registration/recovery paths; mutators publish targeted entity deltas instead
of forcing fresh `ui_tree_snapshot` frames for data-only changes.

Stable submitter `id` values are required in repeated rows so the generic
`ui_action` lifecycle can scope pending, success, and error feedback to the
clicked control. `project_pipelines/web/actions.lua` returns `action.HANDLED`
for draft/local updates and `action.result{ message = ..., navigate = ... }` or
`action.result{ ok = false, error = ... }` for submitters that need visible
feedback.

### GUI Implementation Contract

Project Pipelines is a Botster Lua plugin surface rendered through the shared
`ui_contract`, not a Rails ERB, Turbo, Hotwire, or Elements surface. Author UI
in `project_pipelines/web/*.lua` with shared primitives and let the browser
render those nodes through `app/frontend/ui_contract/registry.tsx` and the
existing Catalyst components in `app/frontend/components/catalyst/*`.

For visible controls, use the public Lua primitives instead of plugin-specific
browser components:

- Forms use `ui.form`, `ui.text_input`, `ui.textarea`, `ui.select`, and
  `ui.checkbox`; the web renderer supplies Catalyst inputs and native
  validation.
- Actions use `ui.button` or `ui.icon_button` with semantic `icon` names, so
  `IconGlyph` remains the single icon path. Do not inline SVG or add bespoke
  icon renderers.
- Submitters in repeated rows must carry stable node `id` values so the generic
  `ui_action` lifecycle can scope pending and result feedback to the clicked
  button.
- Dynamic collections publish plugin-owned entity records and bind with
  `ui.bind` or `ui.bind_list` from sources such as `/project-pipelines.ticket`.
  Publish filterable record supersets rather than per-view browser stores.
- Detail rows use `ui.bind_list{ where = { ... } }` for filtered children when
  they need dynamic entity-backed rows. Keep `ui_tree_snapshot` for route
  scaffolding, current-path controls, and other presentation structure.

Before restyling visible Catalyst primitives, inspect `tmp/tailwind_plus_preview`
if it exists in the worktree. If the directory is absent, use the vendored
Catalyst primitives and existing `ui_contract` registry styles as the design
source. State binding, entity projections, and action lifecycle work should not
invent Tailwind or Elements patterns just because no preview directory exists.

The overview's dynamic Projects, Tickets, Recent Runs, and Pipeline Definitions
sections render from plugin-owned entities through shared UI primitives:
Tickets and Pipeline Definitions use `ui.list` / `ui.list_item`, Projects use
`ui.tree` / `ui.tree_item`, and Recent Runs uses `ui.table` with rows bound from
`/project-pipelines.run`. Keep these collections entity-backed; mutators that
only change collection data should publish entity snapshots or deltas instead
of forcing a fresh `ui_tree_snapshot`. Detail screens render presentation
snapshots for route scaffolding and controls, but dynamic model rows are
entity-backed through `ui.bind` / `ui.bind_list`.

Routes:

- `/pipelines` is the overview.
- `/pipelines/tickets/:ticket_id` is the ticket detail page for moving a ticket into a pipeline, viewing runs, and opening agent terminals that have touched the ticket.
- `/pipelines/tickets/:ticket_id/sessions/:session_uuid` opens a ticket-associated agent terminal.
- `/pipelines/pipelines` is the pipeline definition index.
- `/pipelines/pipelines/:pipeline_id/edit` edits pipeline name/description, selected agent per agent step, step prompts, command steps, and gate prompts/commands with Catalyst UI primitives.

Pipeline edits mutate the shared pipeline definition. They affect future runs, not already spawned agent sessions.

The plugin sidebar is ticket-first. It intentionally does not show active agents; terminals are reached through the ticket page so the user stays oriented around the work item.

Ticket closure closes every Botster session associated with every run for that ticket without deleting worktrees. Step completion does not close agents; they remain available for inspection and for future prompts if a run returns to their step.

Pipelines declare a merge policy: `direct` merges accepted work directly to
main, while `pr` opens or updates a PR through Botster MCP PR tools. Completed
runs automatically spawn the merge agent; there is no separate merge approval
click. Merge completion should leave a durable merge artifact on the ticket.
Merge agents are final acceptance gates, not just Git operators: they must
verify the ticket intent, review findings, runtime wiring, docs, tests, and
removed/deprecated paths before merging. They should include `merge_commit`,
`pr_url`, or `merge_summary` when calling `project_pipelines_close_ticket` with
`merge_confirmed=true`.

Every pipeline handoff should bias toward disciplined, verifiable work:
assumptions are explicit, changes are surgical, speculative abstractions are
rejected, and success criteria are proven before advancement. For runtime,
async, permission, UI-routing, data-plane, control-plane, and architecture
migration tickets, gate evidence must prove the actual production path uses the
new behavior; code shape alone is not acceptance. Stub wiring that delegates
back to the old production path is incomplete unless the ticket is explicitly
scaffold-only or a human waiver is recorded.

Tickets can optionally depend on other tickets. A ticket with open dependencies cannot start a pipeline run until each dependency ticket is closed. Project tickets remain visible from the project page; the sidebar ticket list shows standalone tickets only, plus notification badges when their associated sessions need attention.

## Persistence And Evolution

State lives in the plugin database at the Botster device data root under `plugin-data/project-pipelines/db.sqlite`. The schema is additive-friendly, but non-additive changes need an explicit `version` bump and migration function in `project_pipelines_db.lua`.

During the first load after the seedless pipeline change, the plugin performs a
one-shot cleanup check for the old `massive_feature` seed pipeline and records a
`pipeline.legacy_prune_checked` event so hot reloads do not repeat the cleanup.

## Starting A Run

Create a ticket:

```json
{
  "title": "Implement saved filters",
  "description": "Users can save and reuse filters.",
  "target_id": "tgt_..."
}
```

Start a run:

```json
{
  "ticket_id": "ticket_...",
  "pipeline_id": "ticket_delivery",
  "target_id": "...",
  "target_path": "/path/to/repo",
  "workspace_name": "Pipelines"
}
```

Agent steps require `target_id` or `target_path`; command steps require `target_path`.
