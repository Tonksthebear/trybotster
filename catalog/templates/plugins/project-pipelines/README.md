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

The overview's dynamic Projects, Tickets, Recent Runs, and Pipeline Definitions
sections render from plugin-owned entities through shared UI primitives:
Tickets and Pipeline Definitions use `ui.list` / `ui.list_item`, Projects use
`ui.tree` / `ui.tree_item`, and Recent Runs uses `ui.table` with rows bound from
`/project-pipelines.run`. Keep these collections entity-backed; mutators that
only change collection data should publish entity snapshots or deltas instead
of forcing a fresh `ui_tree_snapshot`. Detail screens still render presentation
snapshots until they are explicitly migrated.

Routes:

- `/pipelines` is the overview.
- `/pipelines/tickets/:ticket_id` is the ticket detail page for moving a ticket into a pipeline, viewing runs, and opening agent terminals that have touched the ticket.
- `/pipelines/tickets/:ticket_id/sessions/:session_uuid` opens a ticket-associated agent terminal.
- `/pipelines/pipelines` is the pipeline definition index.
- `/pipelines/pipelines/:pipeline_id/edit` edits pipeline name/description, selected agent per agent step, step prompts, command steps, and gate prompts/commands with Catalyst UI primitives.

Pipeline edits mutate the shared pipeline definition. They affect future runs, not already spawned agent sessions.

The plugin sidebar is ticket-first. It intentionally does not show active agents; terminals are reached through the ticket page so the user stays oriented around the work item.

Ticket closure closes every Botster session associated with every run for that ticket without deleting worktrees. Step completion does not close agents; they remain available for inspection and for future prompts if a run returns to their step.

Merge completion should leave a durable merge artifact on the ticket. Merge agents should include `merge_commit`, `pr_url`, or `merge_summary` when calling `project_pipelines_close_ticket` with `merge_confirmed=true`.

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
