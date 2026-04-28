-- ui/events.lua — Hub event handler for TUI.
--
-- Called from Rust: events.on_hub_event(event_type, event_data, context)
-- Returns: list of ops | nil
--   nil   -> Rust ignores the event (already logged)
--   ops   -> Rust executes each op in sequence
--
-- The TUI is a client consuming hub events (same as the browser).
-- Durable model state is delivered through entity_* frames and owned by the
-- Rust entity stores. This module handles only transient workflow events.
-- Only primitive ops (send_msg, focus_terminal, quit) are returned to Rust.
--
-- Supported operations:
--   set_mode          { op, mode }                   - Update Rust's mode shadow
--   send_msg          { op, data }
--   focus_terminal    { op, agent_id, session_uuid }
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

--- Resolve the currently selected agent from client-side state.
-- Prefer selected_session_uuid because it survives Rust-side transport resets.
-- Falls back to context.selected_agent when UUID is missing.
local function resolve_selected_agent(context)
  local selected_uuid = _tui_state and _tui_state.selected_session_uuid
  if selected_uuid then
    for _, a in ipairs(_tui_state.agents or {}) do
      if a.session_uuid == selected_uuid then
        return a
      end
    end
  end

  local selected_id = context and context.selected_agent
  if selected_id and selected_uuid then
    return { id = selected_id, session_uuid = selected_uuid }
  end
  if selected_id then
    for _, a in ipairs(_tui_state.agents or {}) do
      if a.id == selected_id then
        return a
      end
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
  if event_type == "pty_notification" then
    -- Emit OSC alert only when the TUI terminal does NOT have focus.
    -- When focused, the user can already see the dot in the agent list.
    if not context.terminal_focused then
      return {{ op = "osc_alert", title = event_data.title, body = event_data.body }}
    end
    return {}
  end

  if event_type == "spawn_target_feedback" then
    if event_data.tone == "error" then
      _tui_state.error_message = event_data.message or "Spawn target operation failed"
      return { set_mode_ops("error") }
    end
    return {}
  end

  if event_type == "agent_config" then
    local agents = event_data.agents
    if not agents then return nil end
    _tui_state.available_agents = agents
    _tui_state.available_accessories = event_data.accessories or {}

    -- Accessory creation flow: skip agent selection, go straight to accessory list
    if _tui_state.mode == "new_accessory_select" then
      return {}
    end

    if #agents <= 1 then
      -- Single or no agent config: auto-select and skip to workspace selection
      _tui_state.pending_fields.agent_name = agents[1]

      -- Build workspace choices from current state
      _tui_state.available_workspaces = {}
      for _, ws in ipairs(_tui_state.workspaces or {}) do
        _tui_state.available_workspaces[#_tui_state.available_workspaces + 1] = {
          id = ws.id,
          name = ws.name or ws.id,
          agent_count = ws.agents and #ws.agents or 0,
        }
      end

      return { set_mode_ops("new_agent_select_workspace") }
    end
    -- Multiple agent configs: populate list for user selection (mode already set)
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

  if event_type == "bridge_reconnected" then
    return {}
  end

  if event_type == "hub_recovery_state" or event_type == "hub_ready" then
    -- Only exit the restart overlay when we were explicitly waiting on a hub
    -- restart. Initial boot events should not force mode transitions.
    if _tui_state.mode ~= "restarting" then
      return {}
    end
    local state = event_data and event_data.state or nil
    if event_type == "hub_ready" or state == "ready" then
      local ops = {}
      local selected = resolve_selected_agent(context)
      if selected and selected.id and selected.session_uuid then
        ops[#ops + 1] = {
          op = "focus_terminal",
          agent_id = selected.id,
          session_uuid = selected.session_uuid,
        }
      end
      ops[#ops + 1] = set_mode_ops(selected and "terminal" or "list")
      return ops
    end
    return {}
  end

  -- subscribed, error — just logging, no state changes needed
  return nil
end

return M
