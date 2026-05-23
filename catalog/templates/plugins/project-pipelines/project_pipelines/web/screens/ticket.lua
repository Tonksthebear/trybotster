-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/ticket.lua
-- @scope device
-- @version 1.1.0

local repo = require("project_pipelines.repo")
local util = require("project_pipelines.util")
local view = require("project_pipelines.web.ui")

local M = {}

local function actor_label(session_uuid, fallback)
    if util.is_blank(session_uuid) then
        return fallback or "human"
    end
    local info = view.session_info(session_uuid)
    if info then
        return info.label or info.display_name or info.title or info.agent_name or session_uuid
    end
    return session_uuid
end

local function current_agent_session_uuid(run, overview)
    if not run or util.is_blank(run.current_run_step_id) then
        return nil
    end
    local current = nil
    if overview and overview.run_steps then
        for _, step in ipairs(overview.run_steps) do
            if step.id == run.current_run_step_id then
                current = step
                break
            end
        end
    end
    current = current or repo.get_run_step_visit(run.current_run_step_id)
    return current and current.agent_session_uuid or nil
end

local function pipeline_start_controls(ticket, ctx, overview)
    local children = {}
    local open_run = overview and overview.open_run or repo.open_ticket_run(ticket.id)
    local latest_run = overview and overview.latest_run or repo.latest_ticket_run(ticket.id)

    if ticket.status == "closed" then
        return { ui.text{ text = "This ticket is closed.", size = "sm", tone = "muted" } }
    end

    if open_run and latest_run and open_run.id == latest_run.id then
        local pipeline = overview and overview.pipelines_by_id and overview.pipelines_by_id[open_run.pipeline_id] or repo.get_pipeline(open_run.pipeline_id)
        local step = open_run.current_step_id and overview and overview.steps_by_id and overview.steps_by_id[open_run.current_step_id] or nil
        if not step and open_run.current_step_id then
            step = repo.get_step(open_run.current_step_id)
        end
        return {
            view.panel{
                ui.stack{ direction = "vertical", gap = "2", children = {
                    view.row{
                        view.badge(open_run.status == "blocked" and "blocked" or "in progress", open_run.status == "blocked" and "danger" or "accent"),
                        ui.text{ text = pipeline and pipeline.name or open_run.pipeline_id, size = "sm", weight = "semibold" },
                    },
                    ui.text{ text = step and ("Current step: " .. step.name) or "Pipeline is running.", size = "sm", tone = "muted" },
                    ui.button{
                        id = "ticket-" .. ticket.id .. "-open-run-" .. open_run.id,
                        label = "Open run",
                        icon = "queue-list",
                        variant = "solid",
                        tone = "accent",
                        action = ui.action("botster.nav.open", { path = ctx.path("/runs/" .. open_run.id) }),
                    },
                } },
            },
        }
    end

    if open_run then
        return {
            view.panel{
                ui.stack{ direction = "vertical", gap = "2", children = {
                    view.row{
                        view.badge("blocked", "danger"),
                        ui.text{ text = "This ticket has an older open run.", size = "sm", weight = "semibold" },
                    },
                    ui.text{ text = "Close or resolve the existing run before starting another pipeline.", size = "sm", tone = "muted" },
                    ui.button{
                        id = "ticket-" .. ticket.id .. "-open-blocked-run-" .. open_run.id,
                        label = "Open run",
                        icon = "queue-list",
                        variant = "solid",
                        tone = "accent",
                        action = ui.action("botster.nav.open", { path = ctx.path("/runs/" .. open_run.id) }),
                    },
                } },
            },
        }
    end

    if not ticket.target_id or ticket.target_id == "" then
        return { ui.text{ text = "This older ticket has no spawn target. Create a target-bound ticket to start a pipeline.", size = "sm", tone = "danger" } }
    end

    for _, pipeline in ipairs(overview and overview.pipelines or repo.list_pipelines()) do
        table.insert(children, view.panel{
            ui.stack{
                direction = "vertical",
                gap = "3",
                children = {
                    ui.stack{ direction = "vertical", gap = "1", children = {
                        view.row{
                            ui.text{ text = pipeline.name, size = "sm", weight = "semibold" },
                            view.badge(pipeline.id, "muted"),
                        },
                        ui.text{ text = pipeline.description or "", size = "xs", tone = "muted" },
                    } },
                    view.row{
                        view.badge(view.target_label(ticket.target_id), "accent"),
                        ui.button{
                            id = "ticket-" .. ticket.id .. "-start-pipeline-" .. pipeline.id,
                            label = "Start pipeline",
                            icon = "play",
                            variant = "solid",
                            tone = "accent",
                            action = ui.action("project_pipelines.start_ticket_pipeline", {
                                ticket_id = ticket.id,
                                pipeline_id = pipeline.id,
                            }),
                        },
                    },
                },
            },
        })
    end

    return children
end

local function session_rows(ticket_id, ctx, overview)
    local children = {}
    local seen = {}
    local removed_manual_sessions = {}
    local current_uuid = current_agent_session_uuid(overview and overview.open_run, overview)

    for _, event in ipairs(overview and overview.events or repo.ticket_events(ticket_id, nil, 100)) do
        if event.kind == "ticket.manual_session_removed" then
            local payload = util.decode(event.payload, {})
            if not util.is_blank(payload.session_uuid) then
                removed_manual_sessions[payload.session_uuid] = true
            end
        end
    end

    local function add_session(uuid, label, status, opts)
        opts = opts or {}
        if uuid and uuid ~= "" and not seen[uuid] then
            seen[uuid] = true
            local info = view.session_info(uuid)
            local alive = info ~= nil
            local notified = uuid == current_uuid and view.session_has_notification(uuid)
            local header = {
                ui.text{ text = label, size = "sm", weight = "semibold" },
                view.badge(alive and "running" or "closed", alive and "accent" or "muted"),
            }
            if status then
                table.insert(header, view.badge(status, "muted"))
            end
            if notified then
                table.insert(header, view.badge("needs attention", "danger"))
            end
            local panel_children = {
                view.row(header),
            }
            if alive then
                table.insert(panel_children, ui.session_row{
                    session_uuid = uuid,
                    density = "panel",
                })
                table.insert(panel_children, ui.button{
                    id = "ticket-" .. ticket_id .. "-terminal-" .. uuid,
                    label = "Open terminal",
                    icon = "command-line",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", {
                        path = ctx.path("/tickets/" .. ticket_id .. "/sessions/" .. uuid),
                    }),
                })
            else
                table.insert(panel_children, ui.text{ text = uuid, size = "xs", tone = "muted" })
                table.insert(panel_children, ui.text{ text = "Terminal session is closed.", size = "xs", tone = "muted" })
            end
            if opts.manual == true then
                table.insert(panel_children, ui.button{
                    id = "ticket-" .. ticket_id .. "-delete-manual-session-" .. uuid,
                    label = alive and "Close session" or "Remove session",
                    icon = "trash",
                    variant = "outline",
                    tone = "danger",
                    action = ui.action("project_pipelines.delete_manual_ticket_session", {
                        ticket_id = ticket_id,
                        session_uuid = uuid,
                    }),
                })
            end
            table.insert(children, view.panel{
                ui.stack{ direction = "vertical", gap = "2", children = panel_children },
            })
        end
    end

    for _, step in ipairs(overview and overview.run_steps or repo.ticket_run_steps(ticket_id)) do
        add_session(step.agent_session_uuid, step.name .. " - " .. (step.agent_name or step.agent_session_uuid), step.status)
    end
    for _, event in ipairs(overview and overview.events or repo.ticket_events(ticket_id, nil, 100)) do
        if event.kind == "ticket.merge_requested" or event.kind == "ticket.merge_agent_linked" then
            local payload = util.decode(event.payload, {})
            add_session(payload.session_uuid, "Merge agent", "merge")
        elseif event.kind == "ticket.manual_session_linked" then
            local payload = util.decode(event.payload, {})
            if not removed_manual_sessions[payload.session_uuid] then
                local session_type = payload.session_type or (payload.role == "manual-accessory" and "accessory" or "agent")
                local name = payload.agent_name or payload.accessory_name or session_type
                add_session(payload.session_uuid, "Manual " .. session_type .. " - " .. name, "manual", { manual = true })
            end
        elseif event.kind == "question.agent_linked" then
            local payload = util.decode(event.payload, {})
            add_session(payload.session_uuid, "Question advisor", "question")
        end
    end

    if #children == 0 then
        table.insert(children, ui.text{ text = "No agent sessions have interacted with this ticket yet.", size = "sm", tone = "muted" })
    end
    return children
end

local function active_session_button(run, ctx, overview)
    local session_uuid = current_agent_session_uuid(run, overview)
    if util.is_blank(session_uuid) then
        return nil
    end
    if not view.session_info(session_uuid) then
        return nil
    end
    return ui.button{
        id = "ticket-" .. run.ticket_id .. "-current-terminal-" .. session_uuid,
        label = "Open current terminal",
        icon = "command-line",
        variant = "solid",
        tone = "accent",
        action = ui.action("botster.nav.open", {
            path = ctx.path("/tickets/" .. run.ticket_id .. "/sessions/" .. session_uuid),
        }),
    }
end

local function current_state_panel(ticket, ctx, overview)
    local ticket_path = "/project-pipelines.ticket/" .. ticket.id
    return view.panel{
        ui.stack{ direction = "vertical", gap = "2", children = {
        view.row{
            view.badge(ui.bind(ticket_path .. "/latest_run_badge"), ui.bind(ticket_path .. "/latest_run_tone")),
            ui.text{ text = ui.bind(ticket_path .. "/active_work_label"), size = "md", weight = "semibold" },
        },
        ui.text{ text = ui.bind(ticket_path .. "/active_work_detail"), size = "sm", tone = "muted" },
        } },
    }
end

local function handoff_rows(run, ctx, overview)
    if not run then
        return { ui.text{ text = "No timeline yet.", size = "sm", tone = "muted" } }
    end

    local children = {}
    local steps = overview and overview.run_steps_by_run and overview.run_steps_by_run[run.id] or repo.run_steps(run.id)
    local questions_by_run_step = {}
    for _, question in ipairs(overview and overview.questions or repo.ticket_questions(run.ticket_id)) do
        local key = question.run_step_id
        if not util.is_blank(key) then
            questions_by_run_step[key] = questions_by_run_step[key] or {}
            table.insert(questions_by_run_step[key], question)
        end
    end
    for index, step in ipairs(steps) do
        local header = {
            ui.text{ text = tostring(index) .. ". " .. step.name, size = "sm", weight = "semibold" },
            view.badge(step.status),
        }
        if step.agent_name and step.agent_name ~= "" then
            table.insert(header, view.badge(step.agent_name, "muted"))
        end

        local step_children = {
            view.row(header),
        }
        if step.agent_session_uuid and step.agent_session_uuid ~= "" then
            local notified = step.id == run.current_run_step_id and view.session_has_notification(step.agent_session_uuid)
            if notified then
                table.insert(step_children, view.badge("notification", "danger"))
            end
            table.insert(step_children, ui.button{
                id = "ticket-" .. run.ticket_id .. "-timeline-terminal-" .. step.id,
                label = "Open terminal",
                icon = "command-line",
                variant = "ghost",
                action = ui.action("botster.nav.open", {
                    path = ctx.path("/tickets/" .. run.ticket_id .. "/sessions/" .. step.agent_session_uuid),
                }),
            })
        end
        table.insert(children, view.panel{ ui.stack{ direction = "vertical", gap = "2", children = step_children } })

        local handoffs = {}
        local gate_results = overview and overview.gate_results_by_run_step and overview.gate_results_by_run_step[step.id] or repo.run_step_gate_results(step.id)
        for _, result in ipairs(gate_results) do
            table.insert(handoffs, {
                label = "Gate",
                status = result.status,
                summary = result.summary,
                evidence = util.decode(result.evidence, {}),
            })
        end
        local reviews = overview and overview.reviews_by_run_step and overview.reviews_by_run_step[step.id] or repo.run_step_reviews(step.id)
        for _, review in ipairs(reviews) do
            table.insert(handoffs, {
                label = "Review",
                status = review.verdict,
                summary = review.summary,
            })
        end

        if #handoffs > 0 then
            for _, handoff in ipairs(handoffs) do
                local summary = handoff.summary
                if util.is_blank(summary) and type(handoff.evidence) == "table" then
                    summary = handoff.evidence.summary or handoff.evidence.evidence
                end
                table.insert(children, ui.stack{ direction = "vertical", gap = "1", children = {
                    view.row{
                        ui.text{ text = handoff.label .. " handoff", size = "xs", weight = "semibold", tone = "muted" },
                        view.badge(handoff.status),
                    },
                    ui.text{ text = util.is_blank(summary) and "No handoff note attached." or tostring(summary), size = "xs", tone = "muted" },
                } })
            end
        elseif step.status == "done" then
            table.insert(children, ui.text{ text = "Handed off without an attached note.", size = "xs", tone = "muted" })
        end

        for _, question in ipairs(questions_by_run_step[step.id] or {}) do
            local header = {
                ui.text{ text = "Question asked", size = "xs", weight = "semibold", tone = "muted" },
                view.badge(question.kind == "agent" and "agent" or "human", question.kind == "agent" and "accent" or "muted"),
                view.badge(question.status, question.status == "open" and "danger" or "success"),
            }
            if question.blocking == 1 then
                table.insert(header, view.badge("blocking", "danger"))
            end
            local details = {
                view.row(header),
                ui.text{ text = question.question, size = "xs", tone = "muted" },
                ui.text{ text = "Asked by " .. actor_label(question.asked_by_session_uuid, "pipeline agent"), size = "xs", tone = "muted" },
            }
            if question.status ~= "open" then
                table.insert(details, ui.text{
                    text = "Answered by " .. actor_label(question.answered_by_session_uuid, "human"),
                    size = "xs",
                    tone = "muted",
                })
                table.insert(details, ui.text{
                    text = util.is_blank(question.answer) and "No answer text recorded." or tostring(question.answer),
                    size = "xs",
                    tone = "muted",
                })
            end
            table.insert(children, ui.stack{ direction = "vertical", gap = "1", children = details })
        end
    end

    if #children == 0 then
        table.insert(children, ui.text{ text = "No timeline yet.", size = "sm", tone = "muted" })
    end
    return children
end

local function question_rows(ticket, _ctx, _overview)
    return {
        ui.bind_list{
            source = "/project-pipelines.question",
            where = { ticket_id = ticket.id, status = "open" },
            item_template = view.panel{
            ui.stack{ direction = "vertical", gap = "2", children = {
                view.row{
                    view.badge(ui.bind("@/kind_label"), "accent"),
                    view.badge(ui.bind("@/blocking_label"), ui.bind("@/blocking_tone")),
                },
                ui.text{ text = ui.bind("@/question"), size = "sm", weight = "semibold" },
                ui.textarea{
                    label = "Answer",
                    placeholder = "Answer this question",
                    on_change = view.field_action("project_pipelines.update_question_answer", { question_id = ui.bind("@/id") }),
                },
                view.row{
                    ui.button{
                        label = "Answer",
                        icon = "check",
                        variant = "solid",
                        tone = "accent",
                        action = ui.action("project_pipelines.answer_question", { question_id = ui.bind("@/id") }),
                    },
                    ui.button{
                        label = "Dismiss",
                        icon = "x-mark",
                        variant = "ghost",
                        action = ui.action("project_pipelines.answer_question", {
                            question_id = ui.bind("@/id"),
                            answer = "Dismissed by human.",
                            status = "dismissed",
                        }),
                    },
                },
            } },
            },
        },
    }
end

local function dependency_rows(ticket, ctx, overview)
    local children = {
        ui.bind_list{
            source = "/project-pipelines.ticket_dependency",
            where = { ticket_id = ticket.id },
            item_template = view.panel{
                view.row{
                    view.badge(ui.bind("@/depends_on_label"), ui.bind("@/depends_on_tone")),
                    ui.text{
                        text = ui.bind("@/depends_on_title"),
                        size = "sm",
                        weight = "semibold",
                    },
                    ui.button{
                        id = ui.bind("@/id"),
                        label = "Remove",
                        icon = "x-mark",
                        variant = "ghost",
                        action = ui.action("project_pipelines.remove_ticket_dependency", { dependency_id = ui.bind("@/id") }),
                    },
                },
            },
        },
    }
    local dependencies = overview and overview.dependencies or repo.ticket_dependencies(ticket.id)

    if ticket.status ~= "closed" then
        local existing = {}
        for _, dependency in ipairs(dependencies) do
            existing[dependency.depends_on_ticket_id] = true
        end
        local options = {}
        for _, candidate in ipairs(overview and overview.visible_tickets or repo.visible_tickets()) do
            if candidate.id ~= ticket.id and not existing[candidate.id] then
                table.insert(options, {
                    value = candidate.id,
                    label = candidate.title .. " (" .. tostring(candidate.status or "open") .. ")",
                })
            end
        end
        if #options > 0 then
            table.insert(children, view.panel{
                ui.stack{ direction = "vertical", gap = "2", children = {
                    ui.select{
                        id = "dependency-" .. ticket.id,
                        label = "Add dependency",
                        placeholder = "Select ticket",
                        options = options,
                        on_change = view.field_action("project_pipelines.update_dependency_draft", { ticket_id = ticket.id }),
                    },
                    ui.button{
                        id = "ticket-" .. ticket.id .. "-add-dependency",
                        label = "Add dependency",
                        icon = "link",
                        variant = "solid",
                        tone = "accent",
                        action = ui.action("project_pipelines.add_ticket_dependency", { ticket_id = ticket.id }),
                    },
                } },
            })
        elseif #dependencies == 0 then
            table.insert(children, ui.text{ text = "No available tickets to depend on.", size = "sm", tone = "muted" })
        end
    elseif #dependencies == 0 then
        table.insert(children, ui.text{ text = "No dependencies.", size = "sm", tone = "muted" })
    end

    return children
end

local function merge_controls(ticket, ctx, overview)
    local run = overview and overview.latest_run or repo.latest_ticket_run(ticket.id)
    if not run or run.status ~= "done" or ticket.status == "closed" then
        return {}
    end
    local merge_events = overview and overview.merge_events or repo.ticket_events(ticket.id, "ticket.merge_requested", 1)
    local failed_events = overview and overview.failed_merge_events or repo.ticket_events(ticket.id, "ticket.merge_request_failed", 1)
    local pipeline = overview and overview.pipelines_by_id and overview.pipelines_by_id[run.pipeline_id] or repo.get_pipeline(run.pipeline_id) or {}
    local merge_policy = pipeline.merge_policy or "direct"
    local policy_label = merge_policy == "pr" and "PR via Botster MCP" or "direct merge to main"
    local children = {
        view.panel{
            ui.stack{ direction = "vertical", gap = "2", children = {
                view.row{
                    view.badge(#failed_events > 0 and "merge blocked" or (#merge_events > 0 and "merge running" or "merge queued"), #failed_events > 0 and "danger" or "success"),
                    ui.text{ text = "Merge policy: " .. policy_label, size = "sm", weight = "semibold" },
                },
                ui.text{ text = "Completed runs automatically spawn a merge acceptance agent. The ticket closes only after merge confirmation is recorded.", size = "sm", tone = "muted" },
            } },
        },
        ui.bind_list{
            source = "/project-pipelines.pr_link",
            where = { ticket_id = ticket.id },
            item_template = view.panel{
                ui.stack{ direction = "vertical", gap = "2", children = {
                    view.row{
                        view.badge(ui.bind("@/status_label"), ui.bind("@/status_tone")),
                        ui.text{ text = ui.bind("@/label"), size = "sm", weight = "semibold" },
                    },
                } },
            },
        },
        ui.bind_list{
            source = "/project-pipelines.pr_link",
            where = { ticket_id = ticket.id, has_pr_url = true },
            item_template = ui.button{
                id = ui.bind("@/id"),
                label = "Open PR",
                icon = "external-link",
                variant = "solid",
                tone = "accent",
                action = ui.action("botster.url.open", { url = ui.bind("@/pr_url") }),
            },
        },
    }
    if #merge_events > 0 then
        local payload = util.decode(merge_events[1].payload, {})
        table.insert(children, ui.text{ text = payload.session_uuid and ("Merge agent running: " .. payload.session_uuid) or "Merge agent has been requested.", size = "sm", tone = "muted" })
    elseif #failed_events > 0 then
        local payload = util.decode(failed_events[1].payload, {})
        table.insert(children, ui.text{ text = "Automatic merge request failed: " .. tostring(payload.error or "unknown error"), size = "sm", tone = "danger" })
    end
    return children
end

local function merge_result_rows(_ticket, _ctx, overview)
    local run = overview and overview.latest_run
    local artifact = run and repo.latest_merge_pr_artifact(run.id) or nil
    if not artifact then
        return {}
    end
    local payload = util.decode(artifact.payload, {})
    local pr_url = artifact.uri or payload.pr_url
    local merge_commit = payload.merge_commit
    local summary = artifact.summary or payload.merge_summary or "Merge confirmed."
    local children = {
        view.panel{
            ui.stack{ direction = "vertical", gap = "2", children = {
                view.row{
                    view.badge("✓", "success"),
                    ui.text{ text = "Merge recorded", size = "sm", weight = "semibold" },
                },
                ui.text{ text = summary, size = "sm", tone = "muted" },
            } },
        },
    }
    if pr_url and pr_url ~= "" then
        table.insert(children, ui.button{
            id = "ticket-" .. tostring(run.ticket_id) .. "-merge-pr",
            label = "Open PR",
            icon = "external-link",
            variant = "solid",
            tone = "accent",
            action = ui.action("botster.url.open", { url = pr_url }),
        })
    elseif merge_commit and merge_commit ~= "" then
        table.insert(children, ui.text{ text = "Merge commit: " .. tostring(merge_commit), size = "xs", tone = "muted" })
    end
    return children
end

local function run_rows(ticket_id, ctx, overview)
    local children = {}
    for _, run in ipairs(overview and overview.runs or repo.ticket_runs(ticket_id)) do
        local pipeline = overview and overview.pipelines_by_id and overview.pipelines_by_id[run.pipeline_id] or repo.get_pipeline(run.pipeline_id)
        table.insert(children, ui.button{
            id = "ticket-" .. ticket_id .. "-run-" .. run.id,
            label = (pipeline and pipeline.name or run.pipeline_id) .. " - " .. run.status,
            icon = "queue-list",
            variant = "ghost",
            action = ui.action("botster.nav.open", { path = ctx.path("/runs/" .. run.id) }),
        })
    end
    if #children == 0 then
        table.insert(children, ui.text{ text = "This ticket is not in a pipeline yet.", size = "sm", tone = "muted" })
    end
    return children
end

-- Exported so the spawn-control wiring (target_id -> derived config scan path
-- -> agent/accessory option lists) can be exercised directly in tests without
-- standing up the full ticket render.
function M.spawn_session_controls(ticket, _ctx, overview)
    if ticket.status == "closed" then
        return { ui.text{ text = "Closed tickets cannot spawn new sessions.", size = "sm", tone = "muted" } }
    end
    if util.is_blank(ticket.target_id) then
        return { ui.text{ text = "This ticket has no spawn target.", size = "sm", tone = "danger" } }
    end

    local prefix = "ticket_session_" .. ticket.id .. "_"
    local agent_dialog_key = "ticket-" .. ticket.id .. "-spawn-agent-open"
    local accessory_dialog_key = "ticket-" .. ticket.id .. "-spawn-accessory-open"
    local agent_name_key = prefix .. "agent_name"
    local prompt_key = prefix .. "prompt"
    local accessory_name_key = prefix .. "accessory_name"
    local default_agent_name = "codex"
    local default_prompt = ""
    local default_accessory_name = "terminal"
    -- Resolve the repo-config scan root from the ticket's target_id. Prefer a
    -- live ticket session's worktree (it may carry branch-local .botster
    -- config); fall back to the target's repo root. target_path is never
    -- stored on the ticket — it is derived here.
    local config_scan_path = view.worktree_path_for_sessions(
        overview and overview.session_uuids or {},
        view.target_repo_path(ticket.target_id))
    local function spawn_context_row()
        return view.row{
            view.badge(view.target_label(ticket.target_id), "accent"),
            ui.text{ text = "Spawn in this ticket's worktree context.", size = "sm", weight = "semibold" },
        }
    end
    local agent_body = {
        spawn_context_row(),
        ui.select{
            id = "ticket-" .. ticket.id .. "-spawn-agent",
            label = "Agent",
            value = ui.local_state(agent_name_key, default_agent_name),
            options = view.agent_options(nil, config_scan_path),
            on_change = ui.action("botster.presentation.set", { key = agent_name_key }),
        },
        ui.textarea{
            id = "ticket-" .. ticket.id .. "-spawn-prompt",
            label = "Agent prompt",
            placeholder = "Optional prompt for this agent session",
            value = ui.local_state(prompt_key, default_prompt),
            on_change = ui.action("botster.presentation.set", { key = prompt_key }),
        },
    }
    local accessory_body = {
        spawn_context_row(),
        ui.select{
            id = "ticket-" .. ticket.id .. "-spawn-accessory",
            label = "Accessory",
            value = ui.local_state(accessory_name_key, default_accessory_name),
            options = view.accessory_options(nil, config_scan_path),
            on_change = ui.action("botster.presentation.set", { key = accessory_name_key }),
        },
    }

    local children = {
        view.action_row{
            ui.button{
                id = "ticket-" .. ticket.id .. "-open-spawn-agent",
                label = "Spawn agent",
                icon = "command-line",
                variant = "solid",
                tone = "accent",
                action = ui.action("botster.presentation.set", { key = agent_dialog_key, value = true }),
            },
            ui.button{
                id = "ticket-" .. ticket.id .. "-open-spawn-accessory",
                label = "Spawn accessory",
                icon = "wrench-screwdriver",
                variant = "ghost",
                action = ui.action("botster.presentation.set", { key = accessory_dialog_key, value = true }),
            },
        },
        ui.dialog{
            open = ui.local_state(agent_dialog_key, false),
            title = "Spawn agent",
            presentation = "auto",
            body = {
                ui.stack{ direction = "vertical", gap = "3", children = agent_body },
            },
            footer = {
                ui.button{
                    id = "ticket-" .. ticket.id .. "-cancel-spawn-agent",
                    label = "Cancel",
                    variant = "ghost",
                    action = ui.action("botster.presentation.clear", { key = agent_dialog_key }),
                },
                ui.button{
                    id = "ticket-" .. ticket.id .. "-spawn-agent-session",
                    label = "Spawn agent",
                    icon = "plus",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("project_pipelines.spawn_ticket_session", {
                        ticket_id = ticket.id,
                        session_type = "agent",
                        agent_name = ui.local_state(agent_name_key, default_agent_name),
                        prompt = ui.local_state(prompt_key, default_prompt),
                    }),
                },
            },
        },
        ui.dialog{
            open = ui.local_state(accessory_dialog_key, false),
            title = "Spawn accessory",
            presentation = "auto",
            body = {
                ui.stack{ direction = "vertical", gap = "3", children = accessory_body },
            },
            footer = {
                ui.button{
                    id = "ticket-" .. ticket.id .. "-cancel-spawn-accessory",
                    label = "Cancel",
                    variant = "ghost",
                    action = ui.action("botster.presentation.clear", { key = accessory_dialog_key }),
                },
                ui.button{
                    id = "ticket-" .. ticket.id .. "-spawn-accessory-session",
                    label = "Spawn accessory",
                    icon = "plus",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("project_pipelines.spawn_ticket_session", {
                        ticket_id = ticket.id,
                        session_type = "accessory",
                        accessory_name = ui.local_state(accessory_name_key, default_accessory_name),
                    }),
                },
            },
        },
    }
    return children
end

function M.render(view_state, ctx)
    local params = view_state and view_state.params or {}
    local ticket = repo.get_ticket(params.ticket_id)
    if not ticket then
        return view.panel{ ui.text{ text = "Ticket not found", tone = "danger" } }
    end

    local overview = repo.ticket_detail_overview(ticket.id)
    local latest_run = overview.latest_run
    local open_run = latest_run and (latest_run.status == "active" or latest_run.status == "blocked") and latest_run or nil
    local ticket_path = "/project-pipelines.ticket/" .. ticket.id

    local meta = {
        view.badge(ui.bind(ticket_path .. "/target_label"), "accent"),
        view.badge(ui.bind(ticket_path .. "/latest_run_badge"), ui.bind(ticket_path .. "/latest_run_tone")),
    }
    local notification = view.notification_badge(view.notification_count_for_uuids(overview.current_agent_session_uuids))
    if notification then
        table.insert(meta, notification)
    end
    local header_actions = {}
    if ticket.status ~= "closed" and latest_run and latest_run.status == "done" then
        local merge_events = overview.merge_events
        table.insert(header_actions, view.badge(#merge_events > 0 and "merge running" or "merge queued", #merge_events > 0 and "accent" or "muted"))
    elseif ticket.status ~= "closed" then
        table.insert(header_actions, ui.button{
            id = "ticket-" .. ticket.id .. "-close",
            label = "Close ticket",
            variant = "solid",
            tone = "danger",
            action = ui.action("project_pipelines.close_ticket", { ticket_id = ticket.id }),
        })
    end

    local children = {
        view.page_header{
            title = ui.bind(ticket_path .. "/title"),
            back_id = "ticket-" .. ticket.id .. "-back",
            back_path = ctx.path("/"),
            meta = meta,
            actions = header_actions,
            description = ui.bind(ticket_path .. "/description"),
        },
        current_state_panel(ticket, ctx, overview),
        view.section("Questions", question_rows(ticket, ctx, overview)),
        view.section("Dependencies", dependency_rows(ticket, ctx, overview)),
    }
    local merge_result_children = merge_result_rows(ticket, ctx, overview)
    if #merge_result_children > 0 then
        table.insert(children, view.section("Merge Result", merge_result_children))
    end
    local merge_children = merge_controls(ticket, ctx, overview)
    if #merge_children > 0 then
        table.insert(children, view.section("Merge", merge_children))
    end
    table.insert(children, view.section(open_run and "Pipeline" or "Move Into Pipeline", pipeline_start_controls(ticket, ctx, overview)))
    table.insert(children, view.section("Spawn Session", M.spawn_session_controls(ticket, ctx, overview)))
    table.insert(children, view.section("Timeline", handoff_rows(latest_run, ctx, overview)))
    table.insert(children, view.section("Runs", run_rows(ticket.id, ctx, overview)))
    table.insert(children, view.section("Agent Terminals", session_rows(ticket.id, ctx, overview)))

    return ui.stack{ direction = "vertical", gap = "4", children = children }
end

function M.session(view_state, ctx)
    local params = view_state and view_state.params or {}
    return ui.session_terminal{
        session_uuid = params.session_uuid,
        back = ctx.path("/tickets/" .. params.ticket_id),
    }
end

return M
