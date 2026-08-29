-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/init.lua
-- @scope device
-- @version 1.1.0

for _, module_name in ipairs({
    "project_pipelines.util",
    "project_pipelines.db",
    "project_pipelines.entity_contract",
    "project_pipelines.repo",
    "project_pipelines.entities",
    "project_pipelines.engine",
    "project_pipelines.stack_delivery",
    "project_pipelines.github_integration",
    "project_pipelines.notification_policy",
    "project_pipelines.mcp",
    "project_pipelines.web.ui",
    "project_pipelines.web.actions",
    "project_pipelines.web.screens.home",
    "project_pipelines.web.screens.new",
    "project_pipelines.web.screens.project",
    "project_pipelines.web.screens.ticket",
    "project_pipelines.web.screens.pipelines",
    "project_pipelines.web.screens.run",
    "project_pipelines.web.surface",
}) do
    package.loaded[module_name] = nil
end

local repo = require("project_pipelines.repo")
local engine = require("project_pipelines.engine")
local stack_delivery = require("project_pipelines.stack_delivery")
local github_integration = require("project_pipelines.github_integration")
local notification_policy = require("project_pipelines.notification_policy")
local mcp_tools = require("project_pipelines.mcp")
local surface = require("project_pipelines.web.surface")

local M = {}

repo.prune_legacy_seed_data()
local ok_prompts, prompt_result = pcall(stack_delivery.reconcile)
if ok_prompts and prompt_result and prompt_result.refreshed then
    log.info("[project-pipelines] refreshed botster_stack_delivery prompts to " .. tostring(prompt_result.revision))
elseif not ok_prompts then
    log.warn("[project-pipelines] stack delivery prompt refresh failed: " .. tostring(prompt_result))
end
engine.register_entities()
notification_policy.register()
mcp_tools.register()
surface.register()

if events and events.on then
    if _G.__project_pipelines_command_gate_sub and events.off then
        pcall(events.off, _G.__project_pipelines_command_gate_sub)
    end
    _G.__project_pipelines_command_gate_sub = events.on("command_gate_completed", function(data)
        local ok, err = pcall(engine.handle_command_gate_completed, data)
        if not ok then
            log.warn("[project-pipelines] command_gate_completed handler failed: " .. tostring(err))
        end
    end)

    if _G.__project_pipelines_pr_merged_sub and events.off then
        pcall(events.off, _G.__project_pipelines_pr_merged_sub)
    end
    _G.__project_pipelines_pr_merged_sub = events.on("pr_merged", function(data)
        local ok, err = pcall(github_integration.handle_pr_merged, data)
        if not ok then
            log.warn("[project-pipelines] pr_merged handler failed: " .. tostring(err))
        end
    end)

    if _G.__project_pipelines_pr_review_submitted_sub and events.off then
        pcall(events.off, _G.__project_pipelines_pr_review_submitted_sub)
    end
    _G.__project_pipelines_pr_review_submitted_sub = events.on("pr_review_submitted", function(data)
        local ok, err = pcall(github_integration.handle_pr_review_submitted, data)
        if not ok then
            log.warn("[project-pipelines] pr_review_submitted handler failed: " .. tostring(err))
        end
    end)

    if _G.__project_pipelines_pr_comment_sub and events.off then
        pcall(events.off, _G.__project_pipelines_pr_comment_sub)
    end
    _G.__project_pipelines_pr_comment_sub = events.on("pr_comment", function(data)
        local ok, err = pcall(github_integration.handle_pr_comment, data)
        if not ok then
            log.warn("[project-pipelines] pr_comment handler failed: " .. tostring(err))
        end
    end)
end

if hooks and hooks.on then
    hooks.on("agent_created", "project_pipelines.agent_created", function(info)
        local ok, err = pcall(engine.handle_agent_created, info)
        if not ok then
            log.warn("[project-pipelines] agent_created handler failed: " .. tostring(err))
        end
        pcall(engine.reconcile_pending_agent_notifications)
    end)
    hooks.on("agent_lifecycle", "project_pipelines.agent_lifecycle", function(info)
        local ok, err = pcall(engine.handle_agent_lifecycle, info)
        if not ok then
            log.warn("[project-pipelines] agent_lifecycle handler failed: " .. tostring(err))
        end
    end)
end

pcall(function()
    engine.reconcile_agent_sessions({ full = true })
end)

if timer and type(timer.every) == "function" then
    -- Retry pending prompts after a parent write timeout without waiting for
    -- reload. 30 s is longer than the 5 s hook and shorter than operator wait.
    if _G.__project_pipelines_notify_timer and type(timer.cancel) == "function" then
        pcall(timer.cancel, _G.__project_pipelines_notify_timer)
    end
    _G.__project_pipelines_notify_timer = timer.every(30, function()
        pcall(engine.reconcile_agent_sessions)
    end)
end

log.info("[project-pipelines] loaded")

return M
