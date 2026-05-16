-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/entity_contract.lua
-- @scope device
-- @version 1.7.0

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

M.home_screen = {
    {
        name = "questions_to_answer",
        source = "/" .. M.types.question,
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
        source = "/" .. M.types.run,
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
        source = "/" .. M.types.ticket,
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
        source = "/" .. M.types.project,
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
        source = "/" .. M.types.ticket,
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
        source = "/" .. M.types.pipeline,
        fields = {
            "edit_path",
            "id",
            "name",
            "step_count_label",
        },
    },
}

-- `M.screens` is the canonical screen registry; `M.home_screen` remains as a
-- named alias for older tests and docs that predate non-home screen contracts.
M.screens = {
    home = M.home_screen,
    pipelines = {
        {
            name = "pipeline_definitions",
            source = "/" .. M.types.pipeline,
            fields = {
                "description",
                "edit_path",
                "id",
                "name",
                "step_count_label",
                "step_summary",
            },
        },
    },
    project = {
        {
            name = "project_targets",
            source = "/" .. M.types.project_target,
            where_fields = { "project_id" },
            fields = {
                "target_label",
            },
        },
        {
            name = "tickets",
            source = "/" .. M.types.ticket,
            where_fields = { "project_id" },
            fields = {
                "dependency_summary",
                "id",
                "latest_run_badge",
                "latest_run_tone",
                "path",
                "project_stage_label",
                "tail_label",
                "target_label",
                "title",
            },
        },
    },
    ticket = {
        {
            name = "ticket_header",
            mode = "record",
            source = "/" .. M.types.ticket,
            where_fields = { "id" },
            fields = {
                "active_work_detail",
                "active_work_label",
                "description",
                "latest_run_badge",
                "latest_run_tone",
                "target_label",
                "title",
            },
        },
        {
            name = "questions",
            source = "/" .. M.types.question,
            where_fields = { "status", "ticket_id" },
            fields = {
                "blocking_label",
                "blocking_tone",
                "id",
                "kind_label",
                "question",
            },
        },
        {
            name = "dependencies",
            source = "/" .. M.types.ticket_dependency,
            where_fields = { "ticket_id" },
            fields = {
                "depends_on_label",
                "depends_on_title",
                "depends_on_tone",
                "id",
            },
        },
        {
            name = "pr_links",
            source = "/" .. M.types.pr_link,
            where_fields = { "ticket_id" },
            fields = {
                "label",
                "status_label",
                "status_tone",
            },
        },
        {
            name = "open_pr_links",
            source = "/" .. M.types.pr_link,
            where_fields = { "has_pr_url", "ticket_id" },
            fields = {
                "id",
                "pr_url",
            },
        },
    },
    run = {
        {
            name = "run_header",
            mode = "record",
            source = "/" .. M.types.run,
            where_fields = { "id" },
            fields = {
                "current_agent_button_id",
                "current_agent_path",
                "current_agent_session_uuid",
                "detail_label",
                "has_current_agent",
                "has_project",
                "has_ticket",
                "project_button_id",
                "project_path",
                "status",
                "ticket_button_id",
                "ticket_path",
                "ticket_title",
            },
        },
        {
            name = "steps",
            source = "/" .. M.types.run_step,
            where_fields = { "run_id" },
            fields = {
                "detail",
                "has_terminal",
                "name",
                "prompt_text",
                "sequence_label",
                "status_label",
                "terminal_button_id",
                "terminal_path",
            },
        },
        {
            name = "reviews",
            source = "/" .. M.types.review,
            where_fields = { "run_id" },
            fields = {
                "reviewer_session_uuid",
                "summary",
                "verdict",
            },
        },
        {
            name = "findings",
            source = "/" .. M.types.finding,
            where_fields = { "run_id" },
            fields = {
                "details",
                "location_label",
                "severity",
                "status",
                "title",
            },
        },
        {
            name = "artifacts",
            source = "/" .. M.types.artifact,
            where_fields = { "run_id" },
            fields = {
                "display_summary",
                "kind",
                "uri",
            },
        },
        {
            name = "recent_events",
            source = "/" .. M.types.event,
            where_fields = { "run_id" },
            fields = {
                "kind",
            },
        },
    },
}

M.repo_rendered_screens = {
    new = {
        reason = "form scaffolding plus recent rows and project options still render from repo reads",
        repo_calls = {
            "get_project",
            "open_projects",
            "open_ticket_run",
        },
        migration_sources = {
            "/" .. M.types.project,
            "/" .. M.types.ticket,
            "/" .. M.types.run,
        },
    },
}

return M
