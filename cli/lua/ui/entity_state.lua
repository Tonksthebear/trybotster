-- ui/entity_state.lua — Project entity-store snapshots into Lua workflow state.

local ws_helpers = require("ui.workspace_helpers")

local M = {}

local function session_id(session)
  return session and (session.id or session.session_uuid)
end

local function workspace_id(workspace)
  return workspace and (workspace.id or workspace.workspace_id)
end

function M.sync(entities)
  if type(entities) ~= "table" then return end

  if type(entities.sessions) == "table" then
    _tui_state.agents = {}
    for _, session in ipairs(entities.sessions) do
      if type(session) == "table" then
        session.id = session_id(session)
        _tui_state.agents[#_tui_state.agents + 1] = session
      end
    end
    local pf = _tui_state.pending_fields or {}
    if pf.creating_agent_id then
      for _, session in ipairs(_tui_state.agents) do
        local status = session.status
        if session_id(session) == pf.creating_agent_id
            or session.branch_name == pf.creating_agent_id
            or tostring(session.issue_number or "") == tostring(pf.creating_agent_id) then
          if status == "creating_worktree" then
            pf.creating_agent_stage = "creating_worktree"
          elseif status == "spawning_ptys" then
            pf.creating_agent_stage = "spawning_agent"
          elseif status == "running" or status == "failed" or status == "closed" or status == "deleted" then
            pf.creating_agent_id = nil
            pf.creating_agent_stage = nil
          end
          break
        end
      end
    end
  end

  if type(entities.spawn_targets) == "table" then
    _tui_state.available_targets = entities.spawn_targets
  end

  if type(entities.worktrees) == "table" then
    _tui_state.available_worktrees = entities.worktrees
  end

  if type(entities.workspaces) ~= "table" then return end

  _tui_state._workspace_meta = {}
  local agents_by_id = {}
  for _, agent in ipairs(_tui_state.agents or {}) do
    agents_by_id[session_id(agent)] = agent
  end

  local seen_agent = {}
  local workspaces = {}
  for _, workspace in ipairs(entities.workspaces) do
    if type(workspace) == "table" then
      local ws_id = workspace_id(workspace)
      if ws_id then
        workspace.id = ws_id
        _tui_state._workspace_meta[ws_id] = workspace
        local agent_ids = workspace.agents or {}
        if #agent_ids == 0 then
          for _, agent in ipairs(_tui_state.agents or {}) do
            if agent.workspace_id == ws_id then
              agent_ids[#agent_ids + 1] = session_id(agent)
            end
          end
        end

        local agent_objects = {}
        for _, agent_id in ipairs(agent_ids) do
          local agent = agents_by_id[agent_id]
          if agent then
            agent_objects[#agent_objects + 1] = agent
            seen_agent[agent_id] = true
          end
        end

        if #agent_objects > 0 then
          workspaces[#workspaces + 1] = {
            id = ws_id,
            name = workspace.name or ws_id,
            status = workspace.status,
            agents = agent_ids,
            agent_objects = agent_objects,
          }
        end
      end
    end
  end

  for _, agent in ipairs(_tui_state.agents or {}) do
    local id = session_id(agent)
    if id and not seen_agent[id] then
      workspaces[#workspaces + 1] = {
        id = "implicit-" .. id,
        name = agent.branch_name or id,
        agents = { id },
        agent_objects = { agent },
      }
    end
  end

  _tui_state.workspaces = workspaces
  if not _tui_state._ws_collapsed then
    _tui_state._ws_collapsed = {}
  end
  ws_helpers.rebuild_nav_flat_list(_tui_state)
end

return M
