-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/db.lua
-- @scope device
-- @version 1.1.0

-- target_path is intentionally absent from every model. A target's filesystem
-- root is derived on demand from target_id via the spawn target registry
-- (see repo.resolve_target_path / ui.target_repo_path). Storing it denormalized
-- let it drift and leaked a raw path into agent-facing context, where agents
-- mistook it for their working directory. target_id is the sole stored handle.
local db = plugin.db{
    -- Dogfood DBs already sit at user_version 11 (prior local install drift).
    -- Catalog must not declare below that; plugin.db refuses downgrades.
    version = 11,
    migrations = {
        -- v9: drop the denormalized target_path columns. target_path is now
        -- fully derived from target_id at point of use. Guarded so it is a
        -- no-op on fresh databases (column never created) and only drops on
        -- databases upgrading from v8 or earlier.
        [9] = function(migration_db)
            local function drop_target_path(tbl)
                local info = migration_db:eval(string.format("PRAGMA table_info(%s)", tbl))
                if type(info) ~= "table" then
                    return
                end
                for _, col in ipairs(info) do
                    if col.name == "target_path" then
                        migration_db:eval(string.format(
                            "ALTER TABLE %s DROP COLUMN target_path", tbl))
                        return
                    end
                end
            end
            drop_target_path("tickets")
            drop_target_path("project_targets")
            drop_target_path("runs")
        end,
        -- v10: add pipeline lifecycle metadata. These columns are nullable so
        -- existing definitions and fresh installs stay compatible while
        -- archived definitions remain directly addressable by id.
        [10] = function(migration_db)
            local function add_column_if_missing(tbl, name, definition)
                local info = migration_db:eval(string.format("PRAGMA table_info(%s)", tbl))
                if type(info) == "table" then
                    for _, col in ipairs(info) do
                        if col.name == name then
                            return
                        end
                    end
                end
                migration_db:eval(string.format("ALTER TABLE %s ADD COLUMN %s %s", tbl, name, definition))
            end
            add_column_if_missing("pipelines", "version_label", "text")
            add_column_if_missing("pipelines", "archived_at", "integer")
            add_column_if_missing("pipelines", "replacement_pipeline_id", "text")
            add_column_if_missing("pipelines", "supersedes_pipeline_id", "text")
        end,
        -- v11: no schema change. Matches dogfood user_version already at 11.
        [11] = function(_migration_db)
        end,
    },
    models = {
        tickets = {
            id = { "text", required = true, primary = true },
            project_id = { "text" },
            target_id = { "text" },
            title = { "text", required = true },
            description = { "text" },
            status = { "text", required = true },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        ticket_dependencies = {
            id = { "text", required = true, primary = true },
            ticket_id = { "text", required = true },
            depends_on_ticket_id = { "text", required = true },
            created_at = { "integer", required = true },
        },
        projects = {
            id = { "text", required = true, primary = true },
            name = { "text", required = true },
            description = { "text" },
            status = { "text", required = true },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        project_targets = {
            id = { "text", required = true, primary = true },
            project_id = { "text", required = true },
            target_id = { "text", required = true },
            created_at = { "integer", required = true },
        },
        pipelines = {
            id = { "text", required = true, primary = true },
            name = { "text", required = true },
            description = { "text" },
            merge_policy = { "text" },
            version_label = { "text" },
            archived_at = { "integer" },
            replacement_pipeline_id = { "text" },
            supersedes_pipeline_id = { "text" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        pipeline_steps = {
            id = { "text", required = true, primary = true },
            pipeline_id = { "text", required = true },
            position = { "integer", required = true },
            kind = { "text", required = true },
            name = { "text", required = true },
            agent_name = { "text" },
            prompt = { "text" },
            command = { "text" },
            next_step_id = { "text" },
            on_approved_step_id = { "text" },
            on_changes_requested_step_id = { "text" },
            on_blocked_step_id = { "text" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        pipeline_gates = {
            id = { "text", required = true, primary = true },
            step_id = { "text", required = true },
            kind = { "text", required = true },
            prompt = { "text", required = true },
            required_fields = { "text" },
            command = { "text" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        runs = {
            id = { "text", required = true, primary = true },
            ticket_id = { "text", required = true },
            pipeline_id = { "text", required = true },
            status = { "text", required = true },
            current_step_id = { "text" },
            current_run_step_id = { "text" },
            parent_run_id = { "text" },
            target_id = { "text" },
            target_name = { "text" },
            workspace_id = { "text" },
            workspace_name = { "text" },
            base_ticket_id = { "text" },
            base_run_id = { "text" },
            base_ref = { "text" },
            base_target_path = { "text" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        run_steps = {
            id = { "text", required = true, primary = true },
            run_id = { "text", required = true },
            step_id = { "text", required = true },
            sequence = { "integer" },
            status = { "text", required = true },
            agent_session_uuid = { "text" },
            started_at = { "integer" },
            completed_at = { "integer" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        gate_results = {
            id = { "text", required = true, primary = true },
            run_id = { "text", required = true },
            run_step_id = { "text" },
            step_id = { "text", required = true },
            gate_id = { "text", required = true },
            status = { "text", required = true },
            summary = { "text" },
            evidence = { "text" },
            created_by_session_uuid = { "text" },
            created_at = { "integer", required = true },
        },
        reviews = {
            id = { "text", required = true, primary = true },
            run_id = { "text", required = true },
            run_step_id = { "text" },
            step_id = { "text", required = true },
            reviewer_session_uuid = { "text" },
            verdict = { "text", required = true },
            summary = { "text" },
            created_at = { "integer", required = true },
        },
        review_findings = {
            id = { "text", required = true, primary = true },
            review_id = { "text", required = true },
            run_id = { "text", required = true },
            step_id = { "text", required = true },
            severity = { "text", required = true },
            title = { "text", required = true },
            file = { "text" },
            line = { "integer" },
            details = { "text" },
            suggested_fix = { "text" },
            status = { "text", required = true },
            resolution = { "text" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        artifacts = {
            id = { "text", required = true, primary = true },
            run_id = { "text", required = true },
            run_step_id = { "text" },
            step_id = { "text" },
            kind = { "text", required = true },
            uri = { "text" },
            summary = { "text" },
            payload = { "text" },
            created_at = { "integer", required = true },
        },
        questions = {
            id = { "text", required = true, primary = true },
            ticket_id = { "text", required = true },
            run_id = { "text" },
            run_step_id = { "text" },
            step_id = { "text" },
            kind = { "text", required = true },
            status = { "text", required = true },
            question = { "text", required = true },
            answer = { "text" },
            asked_by_session_uuid = { "text" },
            answered_by_session_uuid = { "text" },
            advisor_session_uuid = { "text" },
            blocking = { "integer" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        -- Project-specific or global claim for who owns unanswered questions.
        question_orchestrators = {
            id = { "text", required = true, primary = true },
            scope = { "text", required = true },
            project_id = { "text" },
            session_uuid = { "text", required = true },
            session_label = { "text" },
            claimed_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        checklists = {
            id = { "text", required = true, primary = true },
            scope = { "text", required = true },
            owner_id = { "text", required = true },
            name = { "text", required = true },
            description = { "text" },
            source = { "text" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
        },
        checklist_items = {
            id = { "text", required = true, primary = true },
            checklist_id = { "text", required = true },
            position = { "integer", required = true },
            prompt = { "text", required = true },
            status = { "text", required = true },
            source_ref = { "text" },
            evidence = { "text" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
            completed_at = { "integer" },
        },
        pr_links = {
            id = { "text", required = true, primary = true },
            provider = { "text", required = true },
            repo = { "text", required = true },
            pr_number = { "integer", required = true },
            pr_url = { "text" },
            ticket_id = { "text", required = true },
            run_id = { "text" },
            status = { "text", required = true },
            head_branch = { "text" },
            base_branch = { "text" },
            merge_commit = { "text" },
            created_at = { "integer", required = true },
            updated_at = { "integer", required = true },
            merged_at = { "integer" },
        },
        events = {
            id = { "text", required = true, primary = true },
            run_id = { "text" },
            ticket_id = { "text" },
            kind = { "text", required = true },
            payload = { "text" },
            created_at = { "integer", required = true },
        },
    },
}

local indexes = {
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_steps_pipeline_position ON pipeline_steps(pipeline_id, position)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_tickets_project ON tickets(project_id, updated_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_tickets_target ON tickets(target_id, updated_at)",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_project_pipelines_ticket_dependencies_unique ON ticket_dependencies(ticket_id, depends_on_ticket_id)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_ticket_dependencies_ticket ON ticket_dependencies(ticket_id)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_ticket_dependencies_depends_on ON ticket_dependencies(depends_on_ticket_id)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_project_targets_project ON project_targets(project_id, target_id)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_gates_step ON pipeline_gates(step_id)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_runs_ticket ON runs(ticket_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_runs_base_ticket ON runs(base_ticket_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_runs_base_run ON runs(base_run_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_run_steps_run ON run_steps(run_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_run_steps_run_sequence ON run_steps(run_id, sequence)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_run_steps_session ON run_steps(agent_session_uuid, status)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_gate_results_run_gate ON gate_results(run_id, gate_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_gate_results_run_step ON gate_results(run_step_id, gate_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_gate_results_run_step_created ON gate_results(run_step_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_reviews_run_created ON reviews(run_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_reviews_run_step_created ON reviews(run_step_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_findings_run_status ON review_findings(run_id, status, severity)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_artifacts_run_created ON artifacts(run_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_events_run ON events(run_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_events_ticket_created ON events(ticket_id, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_events_ticket_kind_created ON events(ticket_id, kind, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_questions_ticket_status ON questions(ticket_id, status, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_questions_run_status ON questions(run_id, status, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_checklists_owner ON checklists(scope, owner_id, updated_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_checklist_items_checklist ON checklist_items(checklist_id, position)",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_project_pipelines_pr_links_unique ON pr_links(provider, repo, pr_number)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_pr_links_ticket ON pr_links(ticket_id, updated_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_pr_links_run ON pr_links(run_id, updated_at)",
    "CREATE INDEX IF NOT EXISTS idx_project_pipelines_pr_links_status ON pr_links(status, updated_at)",
}

for _, statement in ipairs(indexes) do
    local ok, err = pcall(function()
        db:eval(statement)
    end)
    if not ok then
        log.warn("[project-pipelines] Failed to create index: " .. tostring(err))
    end
end

return db
