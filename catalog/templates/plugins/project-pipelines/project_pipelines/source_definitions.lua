-- @template Project Pipelines
-- @description Checked-in package-owned pipeline definitions
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/source_definitions.lua
-- @scope device
-- @version 1.1.0

local M = {}

local SOURCE_PATH = "catalog/templates/plugins/project-pipelines/project_pipelines/source_definitions.lua"

local ROUTES = {
    { target = "botster-core", playbook = "botster-core-playbook" },
    { target = "botster-hub", playbook = "botster-hub-playbook" },
    { target = "botster-hub-client", playbook = "botster-hub-client-playbook" },
    { target = "botster-web", playbook = "botster-web-playbook" },
    { target = "botster-tui", playbook = "botster-tui-playbook" },
    { target = "botster-tui-kit", playbook = "botster-tui-kit-playbook" },
    { target = "botster-terminal-ghostty", playbook = "botster-terminal-ghostty-playbook" },
    { target = "Project Pipelines package/plugin paths", playbook = "project-pipelines-playbook", prefix = "also load " },
}

local route_lines = {
    "Repository routing (use the ticket target repository, never the ambient directory):",
}
for _, route in ipairs(ROUTES) do
    route_lines[#route_lines + 1] = string.format(
        "- %s -> %s[[%s]]",
        route.target,
        route.prefix or "",
        route.playbook)
end
local ROUTING_PROMPT = table.concat(route_lines, "\n")

local function routed_prompt(before_routing, after_routing)
    return before_routing .. "\n\n" .. ROUTING_PROMPT .. "\n\n" .. after_routing
end

local definition = {
    id = "botster_stack_delivery",
    name = "Botster Stack Delivery",
    description = "Repository-aware Botster delivery pipeline. Every run binds its ticket target to the exact Botster repository ownership charter, composes the generic and Botster role playbooks, and adds only the runtime, web, package, or Project Pipelines overlays implicated by the task and changed files. Replaces the generic Botster Delivery definition.",
    merge_policy = "pr",
    version_label = "Repository playbooks — 2026-07-28",
    archived_at = nil,
    replacement_pipeline_id = nil,
    supersedes_pipeline_id = "botster_delivery",
    steps = {
        {
            id = "botster_stack_plan",
            position = 1,
            kind = "agent",
            name = "Plan",
            agent_name = "codex",
            prompt = routed_prompt(
                [=[You are the Plan agent for a repository-specific Botster Stack Delivery run.

Start with project_pipelines_current_context. Resolve the ticket target_id to the authoritative target repository before planning; do not infer the repository from the process working directory.

Load, in order:
1. [[planner-playbook]]
2. [[botster-planner-playbook]]
3. The exact repository ownership charter from the routing map below.
4. Targeted atomic notes and any task surface guidance implicated by the ticket.
5. [[project-pipelines-playbook]] only when Project Pipelines package/plugin paths or workflow policy are in scope.]=],
                [=[If the target repository cannot be mapped confidently, ask the human and do not substitute a generic Botster checklist or load every repository charter.

Build the plan from the target repository's code, instructions, CI, and vault context. Keep repository ownership and cross-repository seams explicit. Register cross-repository prerequisites as dependencies against the dependency repository target rather than silently broadening this run.

Your output must include:
- Target repository and target_id
- Repository playbook loaded
- Other role/surface playbooks and atomic notes loaded
- Context loaded
- Scope and non-scope
- Repository ownership boundaries and cross-repo dependencies
- Assumptions and unknowns
- Affected surfaces/files
- Risks
- Acceptance checks/tests, including downstream proof where the charter requires it
- Vault gaps worth capturing]=]),
            next_step_id = "botster_stack_plan_review",
            gates = {
                {
                    id = "botster_stack_plan_gate",
                    kind = "attestation",
                    prompt = "Attach a repository-routed plan that identifies target repository/target_id, exact repository charter, additional playbooks/notes, scope and non-scope, ownership boundaries and cross-repo dependencies, assumptions/unknowns, affected surfaces/files, risks, acceptance checks/tests, and vault gaps.",
                    required_fields = {
                        "target_repository",
                        "target_id",
                        "repository_playbook",
                        "playbooks_notes_loaded",
                        "context_loaded",
                        "scope",
                        "ownership_boundaries_dependencies",
                        "assumptions_unknowns",
                        "affected_surfaces_files",
                        "risks",
                        "acceptance_checks_tests",
                        "vault_gaps",
                    },
                },
            },
        },
        {
            id = "botster_stack_plan_review",
            position = 2,
            kind = "agent",
            name = "Plan Review",
            agent_name = "claude",
            prompt = routed_prompt(
                [=[You are the Plan Review agent for a repository-specific Botster Stack Delivery run.

Start with project_pipelines_current_context. Independently resolve target_id to the authoritative target repository and compare it with the planner's routing. Review the latest plan artifact and gate evidence.

Load, in order:
1. [[plan-reviewer-playbook]]
2. [[botster-plan-reviewer-playbook]]
3. The exact repository ownership charter from the routing map below.
4. The same task surface guidance and targeted notes the plan should have used.
5. [[project-pipelines-playbook]] only when Project Pipelines package/plugin paths or workflow policy are in scope.]=],
                [=[If the target repository cannot be mapped confidently, ask the human and do not substitute a generic Botster checklist or load every repository charter.

Reject a plan that targets the wrong repository, violates the charter's owns/does-not-own boundary, omits required downstream consumer proof, hides cross-repository dependencies, cites absent paths, or substitutes generic Botster guidance for the repository charter. Verify dependencies are registered against the correct repository target and available through the actual consumed artifact where relevant.

Your output must include:
- Approved, changes required, or blocked
- Independently resolved target repository and target_id
- Required repository charter and whether the plan loaded it
- Missing context/playbooks
- Architecture, ownership, scope, or cross-repo dependency issues
- Missing risks/assumptions
- Missing or weak acceptance checks and downstream proof]=]),
            on_approved_step_id = "botster_stack_implement",
            on_changes_requested_step_id = "botster_stack_plan",
            on_blocked_step_id = "botster_stack_plan",
            gates = {
                {
                    id = "botster_stack_plan_review_gate",
                    kind = "review_clear",
                    prompt = "Approve only when target routing is independently verified, the exact repository charter was applied, repository ownership and cross-repo dependencies are correct, and acceptance checks include every local and downstream gate required by the charter. Changes required or blocked returns the run to Plan.",
                    required_fields = {},
                },
            },
        },
        {
            id = "botster_stack_implement",
            position = 3,
            kind = "agent",
            name = "Implement",
            agent_name = "codex",
            prompt = routed_prompt(
                [=[You are the Implement agent for a repository-specific Botster Stack Delivery run.

Start with project_pipelines_current_context. Resolve target_id to the authoritative target repository and verify the approved plan used the same routing before editing.

Load, in order:
1. [[implementer-playbook]]
2. [[botster-implementer-playbook]]
3. The exact repository ownership charter from the routing map below.
4. Targeted atomic notes for the approved files/symbols and affected surfaces.
5. [[project-pipelines-playbook]] only when Project Pipelines package/plugin paths or workflow policy are in scope.]=],
                [=[If the target repository cannot be mapped confidently, ask the human and do not substitute a generic Botster checklist or load every repository charter.

State the repository and playbook constraints you are applying before edits. Work only in the run worktree for the routed target. Follow the approved plan and keep code inside the charter's ownership boundary. Cross-repository work requires separately registered tickets/runs against those repository targets. Use repository-owned test wrappers and add downstream-shaped proof wherever the charter requires it.

Before requesting Review, commit the work, link the PR when required, and persist an implementation report artifact.

Your report must include:
- Target repository and target_id
- Repository playbook and other playbooks/notes applied
- Files changed
- Ownership boundaries preserved
- Cross-repo dependencies or separately routed work
- Deviations from plan
- Tests and downstream proof run
- Unverified behavior or residual risk
- Missing vault guidance discovered]=]),
            next_step_id = "botster_stack_review",
            gates = {
                {
                    id = "botster_stack_implement_gate",
                    kind = "attestation",
                    prompt = "Attach implementation evidence with target repository/target_id, repository charter and other guidance applied, files changed, ownership boundaries preserved, cross-repo routing, deviations, tests/downstream proof, unverified behavior/residual risk, and missing vault guidance.",
                    required_fields = {
                        "target_repository",
                        "target_id",
                        "repository_playbook",
                        "playbooks_notes_applied",
                        "files_changed",
                        "ownership_boundaries_preserved",
                        "cross_repo_routing",
                        "deviations_from_plan",
                        "tests_downstream_proof",
                        "unverified_behavior_or_residual_risk",
                        "missing_vault_guidance",
                    },
                },
            },
        },
        {
            id = "botster_stack_review",
            position = 4,
            kind = "agent",
            name = "Review",
            agent_name = "claude",
            prompt = routed_prompt(
                [=[You are the Review agent for a repository-specific Botster Stack Delivery run.

Start with project_pipelines_current_context. Resolve target_id independently, inspect the linked PR/branch and full changed-file set, and review the actual diff.

Load, in order:
1. [[reviewer-playbook]]
2. [[botster-reviewer-playbook]]
3. The exact repository ownership charter from the routing map below.
4. Every changed-surface overlay that applies:
   - runtime/actors/lifecycle/PTY/transport -> [[botster-runtime-reviewer-playbook]]
   - Ionic React/browser/UiNode/Restty -> [[botster-web-reviewer-playbook]]
   - packages/plugins/manifests/capabilities -> [[botster-package-reviewer-playbook]]
   - Project Pipelines engine/schema/tools/prompts/surfaces -> [[botster-pipeline-reviewer-playbook]] and [[project-pipelines-playbook]]
5. Targeted atomic notes derived from changed files and symbols.]=],
                [=[If the target repository cannot be mapped confidently, ask the human and do not substitute a generic Botster checklist or load every repository charter.

Do not load unrelated surface overlays. Review repository ownership, public seams, downstream consumers, local gates, and cross-repo dependency registration. Reject changes that put policy or contracts in the wrong repository, lack consumer proof, leave old paths alive, or use lighter checks than the charter requires.

Submit structured findings first, ordered by severity. Your output must include:
- Independently resolved target repository and target_id
- Repository charter loaded
- Changed-surface overlays and targeted notes loaded
- Findings with file/line references where possible
- Ownership/public seam/downstream consumer assessment
- Open questions or assumptions
- Test gaps and residual risk
- Vault coverage and missing repeated guidance]=]),
            on_approved_step_id = "botster_stack_verify",
            on_changes_requested_step_id = "botster_stack_implement",
            on_blocked_step_id = "botster_stack_implement",
            gates = {
                {
                    id = "botster_stack_review_gate",
                    kind = "review_clear",
                    prompt = "Approve only when Review independently verifies repository routing, applies the exact ownership charter plus every implicated changed-surface overlay, reports findings first, checks public seams/downstream consumers and cross-repo routing, and leaves no blocker/high finding open.",
                    required_fields = {},
                },
            },
        },
        {
            id = "botster_stack_verify",
            position = 5,
            kind = "agent",
            name = "Verify",
            agent_name = "codex",
            prompt = routed_prompt(
                [=[You are the Verify agent for a repository-specific Botster Stack Delivery run.

Start with project_pipelines_current_context. Independently resolve target_id, inspect the committed diff and implementation report, and recheck every review finding against the live run worktree.

Load, in order:
1. [[verifier-playbook]]
2. [[botster-verifier-playbook]]
3. The exact repository ownership charter from the routing map below.
4. Every changed-surface overlay that applies:
   - runtime/actors/lifecycle/PTY/transport -> [[botster-runtime-verifier-playbook]]
   - Ionic React/browser/UiNode/Restty -> [[botster-web-verifier-playbook]]
   - packages/plugins/manifests/capabilities -> [[botster-package-verifier-playbook]]
   - Project Pipelines engine/schema/tools/prompts/surfaces -> [[botster-pipeline-verifier-playbook]] and [[project-pipelines-playbook]]
5. Targeted atomic notes derived from changed files, symbols, and review findings.]=],
                [=[If the target repository cannot be mapped confidently, ask the human and do not substitute a generic Botster checklist or load every repository charter.

Run the exact repository-owned gates from the charter and applicable overlays. Prove public contract changes through real downstream consumers, subprocess/live harnesses, generated artifacts, or conformance evidence as required. Do not treat crate-local tests, source regexes, unrun commands, stale artifacts, or status-only finding resolution as verification.

If verification exposes a gap, submit a changes-required/blocked review with a durable finding and route back to Implement.

Your output must include:
- Independently resolved target repository and target_id
- Repository charter, surface overlays, and targeted notes loaded
- Exact commands and results
- Behavior and production/downstream path each command proves
- Review findings resolved or still open
- Cross-repository consumer proof
- Unverified behavior
- Remaining risk
- Vault gaps worth capturing]=]),
            on_changes_requested_step_id = "botster_stack_implement",
            on_blocked_step_id = "botster_stack_implement",
            gates = {
                {
                    id = "botster_stack_verify_gate",
                    kind = "attestation",
                    prompt = "Attach verification evidence with target repository/target_id, repository charter and overlays loaded, exact commands/results, behavior and production/downstream proof, finding status, cross-repo consumer proof, unverified behavior, remaining risk, and vault gaps.",
                    required_fields = {
                        "target_repository",
                        "target_id",
                        "repository_playbook",
                        "surface_overlays_notes",
                        "commands_run",
                        "results",
                        "behavior_production_path_proved",
                        "review_findings_status",
                        "cross_repo_consumer_proof",
                        "unverified_behavior",
                        "remaining_risk",
                        "vault_gaps",
                    },
                },
            },
        },
    },
}

local pipeline_fields = {
    "name",
    "description",
    "merge_policy",
    "version_label",
    "archived_at",
    "replacement_pipeline_id",
    "supersedes_pipeline_id",
}

local step_fields = {
    "pipeline_id",
    "position",
    "kind",
    "name",
    "prompt",
    "command",
    "next_step_id",
    "on_approved_step_id",
    "on_changes_requested_step_id",
    "on_blocked_step_id",
}

local gate_fields = {
    "step_id",
    "kind",
    "prompt",
    "required_fields",
    "command",
}

local sourced_steps = {}
local sourced_gates = {}
for _, step in ipairs(definition.steps) do
    sourced_steps[step.id] = definition.id
    for _, gate in ipairs(step.gates or {}) do
        sourced_gates[gate.id] = step.id
    end
end

local function copy(value)
    if type(value) ~= "table" then
        return value
    end
    local out = {}
    for key, item in pairs(value) do
        out[key] = copy(item)
    end
    return out
end

function M.definitions()
    return { copy(definition) }
end

function M.routes()
    return copy(ROUTES)
end

function M.routing_prompt()
    return ROUTING_PROMPT
end

function M.source_path()
    return SOURCE_PATH
end

function M.pipeline_fields()
    return copy(pipeline_fields)
end

function M.step_fields()
    return copy(step_fields)
end

function M.gate_fields()
    return copy(gate_fields)
end

function M.is_sourced_pipeline_id(pipeline_id)
    return pipeline_id == definition.id
end

function M.pipeline_id_for_step(step_id)
    return sourced_steps[step_id]
end

function M.step_id_for_gate(gate_id)
    return sourced_gates[gate_id]
end

return M
