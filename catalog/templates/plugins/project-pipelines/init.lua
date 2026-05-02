-- @template Project Pipelines
-- @description Project and ticket pipeline management with gates, reviews, questions, and agent handoffs
-- @category plugins
-- @dest plugins/project-pipelines/init.lua
-- @scope device
-- @version 1.0.0

for _, module_name in ipairs({
    "project_pipelines.util",
    "project_pipelines.db",
    "project_pipelines.repo",
    "project_pipelines.entities",
    "project_pipelines.engine",
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
local mcp_tools = require("project_pipelines.mcp")
local surface = require("project_pipelines.web.surface")

local M = {}

repo.prune_legacy_seed_data()
engine.register_entities()
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
end

if hooks and hooks.on then
    hooks.on("agent_created", "project_pipelines.agent_created", function(info)
        local ok, err = pcall(engine.handle_agent_created, info)
        if not ok then
            log.warn("[project-pipelines] agent_created handler failed: " .. tostring(err))
        end
    end)
    hooks.on("agent_lifecycle", "project_pipelines.agent_lifecycle", function(info)
        local ok, err = pcall(engine.handle_agent_lifecycle, info)
        if not ok then
            log.warn("[project-pipelines] agent_lifecycle handler failed: " .. tostring(err))
        end
    end)
end

pcall(engine.reconcile_agent_sessions)

log.info("[project-pipelines] loaded")

return M
