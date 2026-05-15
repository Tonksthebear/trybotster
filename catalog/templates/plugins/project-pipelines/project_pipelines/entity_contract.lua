-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/entity_contract.lua
-- @scope device
-- @version 1.1.0

local M = {}

M.owner = "project-pipelines"

M.types = {
    ticket = M.owner .. ".ticket",
    project = M.owner .. ".project",
    project_target = M.owner .. ".project_target",
    ticket_dependency = M.owner .. ".ticket_dependency",
    pipeline = M.owner .. ".pipeline",
    pipeline_step = M.owner .. ".pipeline_step",
    pipeline_gate = M.owner .. ".pipeline_gate",
    run = M.owner .. ".run",
    run_step = M.owner .. ".run_step",
    gate_result = M.owner .. ".gate_result",
    review = M.owner .. ".review",
    finding = M.owner .. ".finding",
    artifact = M.owner .. ".artifact",
    question = M.owner .. ".question",
    checklist = M.owner .. ".checklist",
    checklist_item = M.owner .. ".checklist_item",
    pr_link = M.owner .. ".pr_link",
    event = M.owner .. ".event",
}

M.registered_entity_types = {
    M.types.ticket,
    M.types.project,
    M.types.project_target,
    M.types.ticket_dependency,
    M.types.pipeline,
    M.types.pipeline_step,
    M.types.pipeline_gate,
    M.types.run,
    M.types.run_step,
    M.types.gate_result,
    M.types.review,
    M.types.finding,
    M.types.artifact,
    M.types.question,
    M.types.checklist,
    M.types.checklist_item,
    M.types.pr_link,
    M.types.event,
}

M.sources = {}
for name, entity_type in pairs(M.types) do
    M.sources[name] = "/" .. entity_type
end

M.home_screen = {
    {
        name = "questions_to_answer",
        source = M.sources.question,
        where_fields = { "status" },
        fields = {
            "blocking_label",
            "blocking_tone",
            "id",
            "kind_label",
            "path",
            "question",
            "ticket_title",
        },
    },
    {
        name = "running_pipelines",
        source = M.sources.run,
        where_fields = { "status" },
        fields = {
            "current_step_name",
            "id",
            "path",
            "pipeline_name",
            "ticket_title",
        },
    },
    {
        name = "prs_and_merge",
        source = M.sources.ticket,
        where_fields = { "latest_run_status", "status" },
        fields = {
            "id",
            "merge_detail_label",
            "merge_status_label",
            "merge_status_tone",
            "path",
            "target_label",
            "title",
        },
    },
    {
        name = "projects",
        source = M.sources.project,
        where_fields = { "status" },
        fields = {
            "description",
            "id",
            "name",
            "path",
            "status_label",
            "status_state",
            "status_tone",
        },
    },
    {
        name = "standalone_tickets",
        source = M.sources.ticket,
        where_fields = { "standalone", "status" },
        fields = {
            "id",
            "latest_run_badge",
            "latest_run_tone",
            "path",
            "secondary_badge",
            "secondary_badge_tone",
            "tail_label",
            "title",
        },
    },
    {
        name = "pipeline_definitions",
        source = M.sources.pipeline,
        fields = {
            "edit_path",
            "id",
            "name",
            "step_count_label",
        },
    },
}

return M
