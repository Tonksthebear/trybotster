-- ui/events.lua — Hub event handler for TUI.
--
-- Called from Rust: events.on_hub_event(event_type, event_data, context)
-- Returns: list of ops | nil
--   nil   -> Rust ignores the event (already logged)
--   ops   -> Rust executes each op in sequence
--
-- The TUI is a client consuming hub events (same as the browser).
-- Client-side state lives in _tui_state; events update it directly.
-- Only primitive ops (send_msg, focus_terminal, quit) are returned to Rust.
--
-- Supported operations:
--   set_mode          { op, mode }                   - Update Rust's mode shadow
--   send_msg          { op, data }
--   focus_terminal    { op, agent_id, agent_index }   (agent_index = display_index for Rust TUI)
--   set_connection_code { op, url, qr_ascii }
--   clear_connection_code { op }
--   osc_alert           { op, title, body }            - Write OSC 777/9 to outer terminal

local ws_helpers = require("ui.workspace_helpers")

local M = {}

--- Set mode in _tui_state and return the set_mode op for Rust's shadow.
local function set_mode_ops(mode)
  _tui_state.mode = mode
  _tui_state.list_selected = 0
  _tui_state.input_buffer = ""
  return { op = "set_mode", mode = mode }
end

-- =============================================================================
-- Client-side agent state helpers (same pattern as browser JS)
-- =============================================================================

local function upsert_agent(agent)
  for i, a in ipairs(_tui_state.agents) do
    if a.id == agent.id then
      _tui_state.agents[i] = agent
      return
    end
  end
  _tui_state.agents[#_tui_state.agents + 1] = agent
end

local function remove_agent(agent_id)
  for i, a in ipairs(_tui_state.agents) do
    if a.id == agent_id then
      table.remove(_tui_state.agents, i)
      return
    end
  end
end

local function update_agent_status(agent_id, status)
  for _, a in ipairs(_tui_state.agents) do
    if a.id == agent_id then
      a.status = status
      return
    end
  end
end

local function agent_index_for(agent_id)
  for i, a in ipairs(_tui_state.agents) do
    if a.id == agent_id then return i - 1 end -- 0-based for Rust
  end
  return nil
end

-- =============================================================================
-- Workspace grouping helpers (Phase 3)
-- =============================================================================

local function rebuild_flat_list()
  ws_helpers.rebuild_nav_flat_list(_tui_state)
end

--- Rebuild workspace groups from the flat agents list.
-- Groups agents by workspace_id; falls back to per-agent pseudo-workspace
-- for agents without workspace_id (older hub versions).
-- Uses server-provided workspace metadata when available (title, status).
local function rebuild_workspaces()
  local workspace_meta = _tui_state._workspace_meta or {}

  -- Index agents by id for O(1) lookup
  local agents_by_id = {}
  for _, agent in ipairs(_tui_state.agents or {}) do
    agents_by_id[agent.id] = agent
  end

  -- If server provided workspace groups, use that ordering
  local has_server_groups = false
  for _ in pairs(workspace_meta) do has_server_groups = true; break end

  local workspaces = {}

  if has_server_groups then
    -- Server-provided workspace metadata: use titles, status, and agent ordering
    local ws_list = {}
    for _, meta in pairs(workspace_meta) do
      ws_list[#ws_list+1] = meta
    end
    table.sort(ws_list, function(a, b)
      if a.created_at and b.created_at and a.created_at ~= b.created_at then
        return a.created_at < b.created_at
      end
      return tostring(a.id) < tostring(b.id)
    end)

    local seen_agent = {}
    for _, meta in ipairs(ws_list) do
      local ws_agents = {}
      for _, agent_id in ipairs(meta.agents or {}) do
        local agent = agents_by_id[agent_id]
        if agent then
          ws_agents[#ws_agents+1] = agent
          seen_agent[agent_id] = true
        end
      end
      if #ws_agents > 0 then
        workspaces[#workspaces+1] = {
          id = meta.id,
          title = meta.title or meta.id,
          status = meta.status,
          agents = meta.agents or {},       -- ID array for ws_helpers
          agent_objects = ws_agents,         -- full objects for rendering
        }
      end
    end

    -- Any agents not in a server workspace get their own implicit group
    for _, agent in ipairs(_tui_state.agents or {}) do
      if not seen_agent[agent.id] then
        workspaces[#workspaces+1] = {
          id = "implicit-" .. agent.id,
          title = agent.branch_name or agent.id,
          agents = { agent.id },
          agent_objects = { agent },
        }
      end
    end
  else
    -- No server metadata: group by workspace_id from agent data
    local groups = {}
    local order = {}
    for _, agent in ipairs(_tui_state.agents or {}) do
      local ws_id = agent.workspace_id or ("implicit-" .. agent.id)
      if not groups[ws_id] then
        groups[ws_id] = { agents = {}, agent_objects = {} }
        order[#order+1] = ws_id
      end
      groups[ws_id].agents[#groups[ws_id].agents+1] = agent.id
      groups[ws_id].agent_objects[#groups[ws_id].agent_objects+1] = agent
    end

    for _, ws_id in ipairs(order) do
      local group = groups[ws_id]
      local first = group.agent_objects[1]
      local title
      if first then
        local repo_short = first.repo and first.repo:match("/(.+)$") or first.repo
        local issue = first.metadata and first.metadata.issue_number
        if issue then
          title = string.format("%s #%s", repo_short or "?", issue)
        else
          title = first.branch_name or ws_id
        end
      else
        title = ws_id
      end
      workspaces[#workspaces+1] = {
        id = ws_id,
        title = title,
        agents = group.agents,
        agent_objects = group.agent_objects,
      }
    end
  end

  _tui_state.workspaces = workspaces
  if not _tui_state._ws_collapsed then
    _tui_state._ws_collapsed = {}
  end
  rebuild_flat_list()
end

--- Find the flat_list cursor position for a given agent_id.
-- Returns 0-based index, or nil if not found.
local function find_agent_cursor_pos(agent_id)
  for i, item in ipairs(_tui_state.flat_list or {}) do
    if item.type == "agent" and item.agent_id == agent_id then
      return i - 1  -- 0-based
    end
  end
  return nil
end

--- Dispatch a hub event, returning compound ops or nil.
-- @param event_type string  Event type from hub message
-- @param event_data table   Full event message data
-- @param context table      Current TUI state
-- @return table|nil List of op tables, or nil for no action
function M.on_hub_event(event_type, event_data, context)

  if event_type == "agent_created" then
    local agent = event_data.agent
    if not agent then return nil end

    -- Update client state
    _tui_state.pending_fields.creating_agent_id = nil
    _tui_state.pending_fields.creating_agent_stage = nil
    upsert_agent(agent)
    rebuild_workspaces()

    -- Focus the new agent's terminal and enter insert mode
    if agent.id then
      local idx = agent_index_for(agent.id)
      _tui_state.selected_session_uuid = agent.session_uuid
      _tui_state.list_cursor_pos = find_agent_cursor_pos(agent.id)
      return {
        { op = "focus_terminal", agent_id = agent.id, agent_index = idx },
        set_mode_ops("insert"),
      }
    end

    return {}
  end

  if event_type == "agent_deleted" then
    local agent_id = event_data.agent_id
    if not agent_id then return nil end

    -- Update client state (removes from _tui_state.agents)
    remove_agent(agent_id)
    rebuild_workspaces()

    -- If the deleted agent was selected, move to next available or clear
    if context.selected_agent == agent_id then
      local agents = _tui_state.agents
      if #agents > 0 then
        -- Pick the last agent (most recently added), or clamp to end of list
        local next = agents[#agents]
        local idx = agent_index_for(next.id)
        _tui_state.selected_session_uuid = next.session_uuid
        _tui_state.list_cursor_pos = find_agent_cursor_pos(next.id)
        return {
          { op = "focus_terminal", agent_id = next.id, agent_index = idx },
          set_mode_ops("insert"),
        }
      else
        _tui_state.selected_session_uuid = nil
        _tui_state.list_cursor_pos = nil
        return {
          { op = "focus_terminal" },  -- nil agent_id clears selection
          set_mode_ops("normal"),
        }
      end
    end

    return {}
  end

  if event_type == "agent_status_changed" then
    local agent_id = event_data.agent_id
    local status = event_data.status
    if not agent_id or not status then return nil end

    -- Update creation progress display
    if status == "creating_worktree" then
      _tui_state.pending_fields.creating_agent_id = agent_id
      _tui_state.pending_fields.creating_agent_stage = "creating_worktree"
    elseif status == "spawning_ptys" then
      _tui_state.pending_fields.creating_agent_id = agent_id
      _tui_state.pending_fields.creating_agent_stage = "spawning_agent"
    elseif status == "running" or status == "failed" then
      _tui_state.pending_fields.creating_agent_id = nil
      _tui_state.pending_fields.creating_agent_stage = nil
    elseif status == "stopping" or status == "removing_worktree" or status == "deleted" then
      if _tui_state.pending_fields.creating_agent_id == agent_id then
        _tui_state.pending_fields.creating_agent_id = nil
        _tui_state.pending_fields.creating_agent_stage = nil
      end
    end

    -- Update agent status in client cache and refresh workspace status indicators
    update_agent_status(agent_id, status)
    rebuild_workspaces()
    return {}
  end

  if event_type == "agent_list" then
    local agents = event_data.agents
    if not agents then return nil end
    _tui_state.agents = agents
    -- agent_list now includes workspace metadata from AgentListPayload.
    -- Always reset: if workspaces is absent (older hub), clear stale metadata.
    _tui_state._workspace_meta = {}
    if event_data.workspaces then
      for _, ws in ipairs(event_data.workspaces) do
        _tui_state._workspace_meta[ws.id] = ws
      end
    end
    rebuild_workspaces()
    return {}
  end

  -- Phase 3: workspace metadata broadcast from hub
  if event_type == "workspace_list" then
    local workspaces = event_data.workspaces
    if not workspaces then return nil end
    _tui_state._workspace_meta = {}
    for _, ws in ipairs(workspaces) do
      _tui_state._workspace_meta[ws.id] = ws
    end
    rebuild_workspaces()
    return {}
  end

  if event_type == "pty_notification" then
    -- Emit OSC alert only when the TUI terminal does NOT have focus.
    -- When focused, the user can already see the dot in the agent list.
    if not context.terminal_focused then
      return {{ op = "osc_alert", title = event_data.title, body = event_data.body }}
    end
    return {}
  end

  if event_type == "worktree_list" then
    local worktrees = event_data.worktrees
    if not worktrees then return nil end
    _tui_state.available_worktrees = worktrees
    return {}
  end

  if event_type == "profiles" then
    local profiles = event_data.profiles
    if not profiles then return nil end
    _tui_state.available_profiles = profiles
    if #profiles <= 1 then
      -- Single or no profile: auto-select and skip to worktree selection
      _tui_state.pending_fields.profile = profiles[1]
      return {
        set_mode_ops("new_agent_select_worktree"),
        { op = "send_msg", data = {
          subscriptionId = "tui_hub",
          data = { type = "list_worktrees" },
        }},
      }
    end
    -- Multiple profiles: populate list for user selection (mode already set)
    return {}
  end

  if event_type == "connection_code" then
    local url = event_data.url
    local qr_ascii = event_data.qr_ascii
    if not url or not qr_ascii then return nil end
    return {
      { op = "set_connection_code", url = url, qr_ascii = qr_ascii },
    }
  end

  if event_type == "connection_code_error" then
    return {
      { op = "clear_connection_code" },
    }
  end

  -- subscribed, error — just logging, no state changes needed
  return nil
end

return M
