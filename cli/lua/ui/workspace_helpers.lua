-- ui/workspace_helpers.lua — shared workspace grouping helpers for TUI modules.

local M = {}

M.WORKSPACE_STATUS_PRIORITY = {
  active = 4,
  orphaned = 3,
  suspended = 2,
  closed = 1,
}

local function classify_agent_status(status)
  if status == "active" or status == "running" or status == "spawning_ptys" or status == "creating_worktree" then
    return "active"
  end
  if status == "orphaned" or status == "ghost" then
    return "orphaned"
  end
  if status == "closed" or status == "deleted" or status == "exited" then
    return "closed"
  end
  if status == "suspended" then
    return "suspended"
  end
  return "suspended"
end

--- Derive workspace status from member agent statuses.
-- @param workspace table  Workspace entry with `agents = { agent_id... }`
-- @param agent_by_id table map: agent_id -> agent table
-- @return string one of: active, orphaned, suspended, closed
function M.derive_workspace_status(workspace, agent_by_id)
  local ids = (workspace and workspace.agents) or {}
  if #ids == 0 then
    return "suspended"
  end

  local has_active = false
  local has_orphaned = false
  local all_closed = true

  for _, id in ipairs(ids) do
    local agent = agent_by_id and agent_by_id[id] or nil
    local s = classify_agent_status(agent and agent.status or nil)
    if s == "active" then
      has_active = true
      all_closed = false
    elseif s == "orphaned" then
      has_orphaned = true
      all_closed = false
    elseif s == "closed" then
      -- keep all_closed true only if all are closed
    else
      all_closed = false
    end
  end

  if has_active then return "active" end
  if has_orphaned then return "orphaned" end
  if all_closed then return "closed" end
  return "suspended"
end

--- Rebuild flat agent list from grouped workspaces.
-- Preserves workspace ordering and each workspace's agent ordering.
-- @param workspaces table array of workspace objects with `agents` id arrays
-- @param agents table array of agent objects
-- @return table flattened agent array
function M.rebuild_flat_list(workspaces, agents)
  local out = {}
  local seen = {}
  local by_id = {}

  for _, a in ipairs(agents or {}) do
    by_id[a.id] = a
  end

  for _, ws in ipairs(workspaces or {}) do
    for _, id in ipairs(ws.agents or {}) do
      local agent = by_id[id]
      if agent and not seen[id] then
        out[#out + 1] = agent
        seen[id] = true
      end
    end
  end

  -- Keep ungrouped/unknown agents visible at the end.
  for _, a in ipairs(agents or {}) do
    if a.id and not seen[a.id] then
      out[#out + 1] = a
      seen[a.id] = true
    end
  end

  return out
end

--- Build the TUI navigation flat_list from workspace state.
-- Produces an array of typed items: "creating", "workspace_header", "agent".
-- Used by both events.lua and actions.lua for cursor navigation and layout rendering.
-- @param tui_state table  The global _tui_state table
-- @return table  Array of flat_list items (also stored in tui_state.flat_list)
function M.rebuild_nav_flat_list(tui_state)
  local flat = {}

  -- Creating indicator at top
  local pf = tui_state.pending_fields
  if pf and pf.creating_agent_id then
    flat[#flat+1] = { type = "creating", id = "_creating" }
  end

  -- Build agent_by_id for status derivation
  local agent_by_id = {}
  for _, agent in ipairs(tui_state.agents or {}) do
    agent_by_id[agent.id] = agent
  end

  for _, ws in ipairs(tui_state.workspaces or {}) do
    local collapsed = tui_state._ws_collapsed and tui_state._ws_collapsed[ws.id]
    -- Derive status: use server-provided or compute from agents via shared helper
    local ws_status = ws.status
    if not ws_status then
      ws_status = M.derive_workspace_status(ws, agent_by_id)
    end
    flat[#flat+1] = {
      type = "workspace_header",
      workspace_id = ws.id,
      title = ws.title,
      collapsed = collapsed or false,
      agent_count = #(ws.agent_objects or {}),
      status = ws_status,
    }
    if not collapsed then
      for _, agent in ipairs(ws.agent_objects or {}) do
        flat[#flat+1] = {
          type = "agent",
          workspace_id = ws.id,
          agent_id = agent.id,
        }
      end
    end
  end

  tui_state.flat_list = flat
  return flat
end

return M
