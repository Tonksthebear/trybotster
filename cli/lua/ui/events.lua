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
--   focus_terminal    { op, agent_id, pty_index, agent_index }
--   set_connection_code { op, url, qr_ascii }
--   clear_connection_code { op }
--   osc_alert           { op, title, body }            - Write OSC 777/9 to outer terminal

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

--- Return the appropriate base mode: "insert" if an agent is selected, "normal" otherwise.
local function base_mode(context)
  return context.selected_agent and "insert" or "normal"
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

    -- Focus the new agent's terminal and enter insert mode
    if agent.id then
      local idx = agent_index_for(agent.id)
      _tui_state.selected_agent_index = idx
      _tui_state.active_pty_index = 0
      return {
        { op = "focus_terminal", agent_id = agent.id, pty_index = 0, agent_index = idx },
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

    -- If the deleted agent was selected, move to next available or clear
    if context.selected_agent == agent_id then
      local agents = _tui_state.agents
      if #agents > 0 then
        -- Pick the last agent (most recently added), or clamp to end of list
        local next = agents[#agents]
        local idx = agent_index_for(next.id)
        _tui_state.selected_agent_index = idx
        _tui_state.active_pty_index = 0
        return {
          { op = "focus_terminal", agent_id = next.id, pty_index = 0, agent_index = idx },
          set_mode_ops("insert"),
        }
      else
        _tui_state.selected_agent_index = nil
        _tui_state.active_pty_index = 0
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

    -- Update agent status in client cache
    update_agent_status(agent_id, status)
    return {}
  end

  if event_type == "agent_list" then
    local agents = event_data.agents
    if not agents then return nil end
    _tui_state.agents = agents
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
