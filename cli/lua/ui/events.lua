-- ui/events.lua — Hub event handler for TUI.
--
-- Called from Rust: events.on_hub_event(event_type, event_data, context)
-- Returns: list of ops | nil
--   nil   -> Rust ignores the event (already logged)
--   ops   -> Rust executes each op in sequence
--
-- This module handles all hub lifecycle events that were previously
-- hardcoded in Rust's handle_lua_message(). Lua decides what state
-- changes to make; Rust executes them mechanically.
--
-- Supported operations (same as actions.lua, plus new data ops):
--   set_mode          { op, mode }
--   store_field       { op, key, value }
--   clear_field       { op, key }
--   send_msg          { op, data }
--   focus_terminal    { op, agent_id, pty_index }  - focus agent+pty (nil agent_id clears)
--   upsert_agent      { op, agent }                - add or update agent in cache
--   remove_agent      { op, agent_id }             - remove agent from cache
--   set_agents        { op, agents }               - full replace agent cache
--   set_worktrees     { op, worktrees }            - full replace worktree list
--   set_connection_code { op, url, qr_ascii }      - set connection code display
--   clear_connection_code { op }                    - clear connection code
--
-- Note: context fields:
--   mode, selected_agent, selected_agent_index, active_pty_index,
--   agents (array of {id, session_count}), pending_fields

local M = {}

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

    local ops = {
      { op = "clear_field", key = "creating_agent_id" },
      { op = "clear_field", key = "creating_agent_stage" },
    }

    -- Add the new agent to cache
    table.insert(ops, { op = "upsert_agent", agent = agent })

    -- Focus the new agent's terminal and enter insert mode
    if agent.id then
      table.insert(ops, { op = "focus_terminal", agent_id = agent.id, pty_index = 0 })
      table.insert(ops, { op = "set_mode", mode = "insert" })
    end

    return ops
  end

  if event_type == "agent_deleted" then
    local agent_id = event_data.agent_id
    if not agent_id then return nil end

    local ops = {
      { op = "remove_agent", agent_id = agent_id },
    }

    -- Clear focus if the deleted agent was selected
    if context.selected_agent == agent_id then
      table.insert(ops, { op = "focus_terminal" })  -- nil agent_id clears selection
      table.insert(ops, { op = "set_mode", mode = "normal" })
    end

    return ops
  end

  if event_type == "agent_status_changed" then
    local agent_id = event_data.agent_id
    local status = event_data.status
    if not agent_id or not status then return nil end

    local ops = {}

    -- Update creation progress display based on lifecycle status
    if status == "creating_worktree" then
      table.insert(ops, { op = "store_field", key = "creating_agent_id", value = agent_id })
      table.insert(ops, { op = "store_field", key = "creating_agent_stage", value = "creating_worktree" })
    elseif status == "spawning_ptys" then
      table.insert(ops, { op = "store_field", key = "creating_agent_id", value = agent_id })
      table.insert(ops, { op = "store_field", key = "creating_agent_stage", value = "spawning_agent" })
    elseif status == "running" or status == "failed" then
      table.insert(ops, { op = "clear_field", key = "creating_agent_id" })
      table.insert(ops, { op = "clear_field", key = "creating_agent_stage" })
    elseif status == "stopping" or status == "removing_worktree" or status == "deleted" then
      local pending = context.pending_fields or {}
      if pending.creating_agent_id == agent_id then
        table.insert(ops, { op = "clear_field", key = "creating_agent_id" })
        table.insert(ops, { op = "clear_field", key = "creating_agent_stage" })
      end
    end

    -- Update agent status in cache (upsert with just id + status)
    table.insert(ops, { op = "update_agent_status", agent_id = agent_id, status = status })

    if #ops == 0 then return nil end
    return ops
  end

  if event_type == "agent_list" then
    local agents = event_data.agents
    if not agents then return nil end
    return {
      { op = "set_agents", agents = agents },
    }
  end

  if event_type == "worktree_list" then
    local worktrees = event_data.worktrees
    if not worktrees then return nil end
    return {
      { op = "set_worktrees", worktrees = worktrees },
    }
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
