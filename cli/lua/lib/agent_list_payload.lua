-- Shared builder for hub "agent_list" payloads.
--
-- Produces a backward-compatible payload with:
--   agents     -> flat list (existing clients)
--   workspaces -> grouped workspace metadata (Phase 3)

local Agent = require("lib.agent")

local M = {}

--- Build a normalized agent_list payload.
-- @param agents table|nil Optional precomputed Agent.all_info()-style array
-- @return table { agents = {...}, workspaces = {...} }
function M.build(agents)
    local list = agents
    if type(list) ~= "table" then
        list = Agent.all_info()
    end

    local workspaces = {}
    local data_dir = config.data_dir and config.data_dir() or nil
    if data_dir then
        local ws = require("lib.workspace_store")
        local ok, grouped = pcall(ws.build_workspace_groups, data_dir, list)
        if ok and type(grouped) == "table" then
            workspaces = grouped
        elseif not ok then
            log.warn(string.format("agent_list payload: workspace grouping failed: %s", tostring(grouped)))
        end
    end

    return {
        agents = list,
        workspaces = workspaces,
    }
end

return M
