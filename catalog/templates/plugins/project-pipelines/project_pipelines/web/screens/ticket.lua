-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/project_pipelines/web/screens/ticket.lua
-- @scope device
-- @version 1.0.0

local repo = require("project_pipelines.repo")
local util = require("project_pipelines.util")
local view = require("project_pipelines.web.ui")
local actions = require("project_pipelines.web.actions")

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

local function pipeline_start_controls(ticket, ctx)
    local children = {}
    local open_run = repo.open_ticket_run(ticket.id)
    local latest_run = repo.latest_ticket_run(ticket.id)

    if ticket.status == "closed" then
        return { ui.text{ text = "This ticket is closed.", size = "sm", tone = "muted" } }
    end

    if open_run and latest_run and open_run.id == latest_run.id then
        local pipeline = repo.get_pipeline(open_run.pipeline_id)
        local step = open_run.current_step_id and repo.get_step(open_run.current_step_id) or nil
        return {
            view.panel{
                ui.stack{ direction = "vertical", gap = "2", children = {
                    view.row{
                        view.badge(open_run.status == "blocked" and "blocked" or "in progress", open_run.status == "blocked" and "danger" or "accent"),
                        ui.text{ text = pipeline and pipeline.name or open_run.pipeline_id, size = "sm", weight = "semibold" },
                    },
                    ui.text{ text = step and ("Current step: " .. step.name) or "Pipeline is running.", size = "sm", tone = "muted" },
                    ui.button{
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

    for _, pipeline in ipairs(repo.list_pipelines()) do
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
                        view.badge(view.target_label(ticket.target_id, ticket.target_path), "accent"),
                        ui.button{
                            label = "Start pipeline",
                            icon = "play",
                            variant = "solid",
                            tone = "accent",
                            action = ui.action("project_pipelines.start_ticket_pipeline", {
                                ticket_id = ticket.id,
                                pipeline_id = pipeline.id,
                                workspace_name = "Pipelines",
                            }),
                        },
                    },
                },
            },
        })
    end

    return children
end

local function session_rows(ticket_id, ctx)
    local children = {}
    local seen = {}

    local function add_session(uuid, label, status)
        if uuid and uuid ~= "" and not seen[uuid] then
            seen[uuid] = true
            local info = view.session_info(uuid)
            local alive = info ~= nil
            local notified = view.session_has_notification(uuid)
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
                ui.text{ text = uuid, size = "xs", tone = "muted" },
            }
            if alive then
                table.insert(panel_children, ui.button{
                    label = "Open terminal",
                    icon = "command-line",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("botster.nav.open", {
                        path = ctx.path("/tickets/" .. ticket_id .. "/sessions/" .. uuid),
                    }),
                })
            else
                table.insert(panel_children, ui.text{ text = "Terminal session is closed.", size = "xs", tone = "muted" })
            end
            table.insert(children, view.panel{
                ui.stack{ direction = "vertical", gap = "2", children = panel_children },
            })
        end
    end

    for _, step in ipairs(repo.ticket_run_steps(ticket_id)) do
        add_session(step.agent_session_uuid, step.name .. " - " .. (step.agent_name or step.agent_session_uuid), step.status)
    end
    for _, event in ipairs(repo.ticket_events(ticket_id, nil, 100)) do
        if event.kind == "ticket.merge_requested" or event.kind == "ticket.merge_agent_linked" then
            local payload = util.decode(event.payload, {})
            add_session(payload.session_uuid, "Merge agent", "merge")
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

local function active_session_button(run, ctx)
    if not run or not run.current_run_step_id then
        return nil
    end
    local current = repo.get_run_step_visit(run.current_run_step_id)
    if not current or not current.agent_session_uuid or current.agent_session_uuid == "" then
        return nil
    end
    if not view.session_info(current.agent_session_uuid) then
        return nil
    end
    return ui.button{
        label = "Open current terminal",
        icon = "command-line",
        variant = "solid",
        tone = "accent",
        action = ui.action("botster.nav.open", {
            path = ctx.path("/tickets/" .. run.ticket_id .. "/sessions/" .. current.agent_session_uuid),
        }),
    }
end

local function current_state_panel(ticket, ctx)
    local run = repo.latest_ticket_run(ticket.id)
    if not run then
        return view.panel{
            ui.stack{ direction = "vertical", gap = "2", children = {
                view.row{
                    view.badge("ready", "muted"),
                    ui.text{ text = "Ready for pipeline", size = "md", weight = "semibold" },
                },
                ui.text{ text = "No run has started for this ticket.", size = "sm", tone = "muted" },
            } },
        }
    end

    local pipeline = repo.get_pipeline(run.pipeline_id)
    local step = run.current_step_id and repo.get_step(run.current_step_id) or nil
    local status_label = run.status == "blocked" and "blocked" or (run.status == "done" and "complete" or "in progress")
    local tone = run.status == "blocked" and "danger" or (run.status == "done" and "success" or "accent")
    local children = {
        view.row{
            view.badge(status_label, tone),
            ui.text{ text = step and step.name or (pipeline and pipeline.name or "Pipeline"), size = "md", weight = "semibold" },
        },
        ui.text{ text = pipeline and pipeline.name or run.pipeline_id, size = "sm", tone = "muted" },
    }
    if step and step.prompt and step.prompt ~= "" then
        table.insert(children, ui.text{ text = step.prompt, size = "xs", tone = "muted" })
    end
    local terminal = active_session_button(run, ctx)
    if terminal then
        table.insert(children, terminal)
    end
    return view.panel{ ui.stack{ direction = "vertical", gap = "2", children = children } }
end

local function handoff_rows(run, ctx)
    if not run then
        return { ui.text{ text = "No timeline yet.", size = "sm", tone = "muted" } }
    end

    local children = {}
    local steps = repo.run_steps(run.id)
    local questions_by_run_step = {}
    for _, question in ipairs(repo.ticket_questions(run.ticket_id)) do
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
            local notified = view.session_has_notification(step.agent_session_uuid)
            if notified then
                table.insert(step_children, view.badge("notification", "danger"))
            end
            table.insert(step_children, ui.button{
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
        for _, result in ipairs(repo.run_step_gate_results(step.id)) do
            table.insert(handoffs, {
                label = "Gate",
                status = result.status,
                summary = result.summary,
                evidence = util.decode(result.evidence, {}),
            })
        end
        for _, review in ipairs(repo.run_step_reviews(step.id)) do
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

local function question_rows(ticket, ctx)
    local questions = repo.ticket_questions(ticket.id, "open")
    local children = {}
    for _, question in ipairs(questions) do
        local header = {
            view.badge(question.kind == "agent" and "agent question" or "human question", question.blocking == 1 and "danger" or "accent"),
        }
        if question.blocking == 1 then
            table.insert(header, view.badge("blocking", "danger"))
        end
        table.insert(children, view.panel{
            ui.stack{ direction = "vertical", gap = "2", children = {
                view.row(header),
                ui.text{ text = question.question, size = "sm", weight = "semibold" },
                ui.textarea{
                    id = "question-answer-" .. question.id,
                    label = "Answer",
                    placeholder = "Answer this question",
                    on_change = view.field_action("project_pipelines.update_question_answer", { question_id = question.id }),
                },
                view.row{
                    ui.button{
                        label = "Answer",
                        icon = "check",
                        variant = "solid",
                        tone = "accent",
                        action = ui.action("project_pipelines.answer_question", { question_id = question.id }),
                    },
                    ui.button{
                        label = "Dismiss",
                        icon = "x-mark",
                        variant = "ghost",
                        action = ui.action("project_pipelines.answer_question", {
                            question_id = question.id,
                            answer = "Dismissed by human.",
                            status = "dismissed",
                        }),
                    },
                },
            } },
        })
    end
    if #children == 0 then
        table.insert(children, ui.text{ text = "No open questions.", size = "sm", tone = "muted" })
    end
    return children
end

local function dependency_rows(ticket, ctx)
    local children = {}
    local dependencies = repo.ticket_dependencies(ticket.id)
    for _, dependency in ipairs(dependencies) do
        table.insert(children, view.panel{
            view.row{
                view.badge(dependency.depends_on_status or "missing", dependency.depends_on_status == "closed" and "success" or "danger"),
                ui.text{
                    text = dependency.depends_on_title or dependency.depends_on_ticket_id,
                    size = "sm",
                    weight = "semibold",
                },
                ui.button{
                    label = "Remove",
                    icon = "x-mark",
                    variant = "ghost",
                    action = ui.action("project_pipelines.remove_ticket_dependency", { dependency_id = dependency.id }),
                },
            },
        })
    end

    if ticket.status ~= "closed" then
        local existing = {}
        for _, dependency in ipairs(dependencies) do
            existing[dependency.depends_on_ticket_id] = true
        end
        local options = {}
        for _, candidate in ipairs(repo.visible_tickets()) do
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
                        label = "Add dependency",
                        icon = "link",
                        variant = "solid",
                        tone = "accent",
                        action = ui.action("project_pipelines.add_ticket_dependency", { ticket_id = ticket.id }),
                    },
                } },
            })
        elseif #children == 0 then
            table.insert(children, ui.text{ text = "No available tickets to depend on.", size = "sm", tone = "muted" })
        end
    elseif #children == 0 then
        table.insert(children, ui.text{ text = "No dependencies.", size = "sm", tone = "muted" })
    end

    return children
end

local function merge_controls(ticket, ctx)
    local run = repo.latest_ticket_run(ticket.id)
    if not run or run.status ~= "done" or ticket.status == "closed" then
        return {}
    end
    local merge_events = repo.ticket_events(ticket.id, "ticket.merge_requested", 1)
    local children = {
        view.panel{
            ui.stack{ direction = "vertical", gap = "2", children = {
                view.row{
                    view.badge("ready to merge", "success"),
                    ui.text{ text = "Final signoff is complete.", size = "sm", weight = "semibold" },
                },
                ui.text{ text = "Approve a merge agent to perform the repo-specific merge or PR path. The ticket closes only after merge confirmation is recorded.", size = "sm", tone = "muted" },
                ui.button{
                    label = #merge_events > 0 and "Merge requested" or "Approve merge",
                    icon = "arrow-path",
                    variant = "solid",
                    tone = "accent",
                    action = ui.action("project_pipelines.request_merge", { ticket_id = ticket.id }),
                },
            } },
        },
    }
    if #merge_events > 0 then
        local payload = util.decode(merge_events[1].payload, {})
        table.insert(children, ui.text{ text = payload.session_uuid and ("Merge agent running: " .. payload.session_uuid) or "Merge agent has been requested.", size = "sm", tone = "muted" })
    end
    return children
end

local function run_rows(ticket_id, ctx)
    local children = {}
    for _, run in ipairs(repo.ticket_runs(ticket_id)) do
        local pipeline = repo.get_pipeline(run.pipeline_id)
        table.insert(children, ui.button{
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

function M.render(view_state, ctx)
    local params = view_state and view_state.params or {}
    local ticket = repo.get_ticket(params.ticket_id)
    if not ticket then
        return view.panel{ ui.text{ text = "Ticket not found", tone = "danger" } }
    end

    local latest_run = repo.latest_ticket_run(ticket.id)
    local open_run = latest_run and (latest_run.status == "active" or latest_run.status == "blocked") and latest_run or nil

    local header = {
        ui.button{
            label = "Back",
            icon = "arrow-left",
            variant = "ghost",
            action = ui.action("botster.nav.open", { path = ctx.path("/") }),
        },
        ui.text{ text = ticket.title, size = "lg", weight = "semibold" },
        view.badge(view.target_label(ticket.target_id, ticket.target_path), "accent"),
    }
    if ticket.status == "closed" then
        table.insert(header, view.badge("closed", "muted"))
    elseif open_run then
        table.insert(header, view.badge(open_run.status == "blocked" and "blocked" or "in progress", open_run.status == "blocked" and "danger" or "accent"))
    else
        table.insert(header, view.badge("ready", "muted"))
    end
    if ticket.status ~= "closed" and latest_run and latest_run.status == "done" then
        local merge_events = repo.ticket_events(ticket.id, "ticket.merge_requested", 1)
        table.insert(header, ui.button{
            label = #merge_events > 0 and "Merge requested" or "Approve merge",
            icon = "arrow-path",
            variant = "solid",
            tone = "accent",
            action = ui.action("project_pipelines.request_merge", { ticket_id = ticket.id }),
        })
    elseif ticket.status ~= "closed" then
        table.insert(header, ui.button{
            label = "Close ticket",
            variant = "solid",
            tone = "danger",
            action = ui.action("project_pipelines.close_ticket", { ticket_id = ticket.id }),
        })
    end

    local header_panel = {
        view.row(header),
        ui.text{ text = ticket.description or "", size = "sm", tone = "muted" },
    }
    local children = {
        view.panel{ ui.stack{ direction = "vertical", gap = "2", children = header_panel } },
        current_state_panel(ticket, ctx),
        view.section("Questions", question_rows(ticket, ctx)),
        view.section("Dependencies", dependency_rows(ticket, ctx)),
    }
    local merge_children = merge_controls(ticket, ctx)
    if #merge_children > 0 then
        table.insert(children, view.section("Merge", merge_children))
    end
    table.insert(children, view.section(open_run and "Pipeline" or "Move Into Pipeline", pipeline_start_controls(ticket, ctx)))
    table.insert(children, view.section("Timeline", handoff_rows(latest_run, ctx)))
    table.insert(children, view.section("Runs", run_rows(ticket.id, ctx)))
    table.insert(children, view.section("Agent Terminals", session_rows(ticket.id, ctx)))

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
