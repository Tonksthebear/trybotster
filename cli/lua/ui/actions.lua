-- ui/actions.lua — Application workflow dispatch for compound actions.
--
-- Keybindings.lua stays a pure lookup table (key -> action string).
-- This module handles *workflow* dispatch: when an action needs multiple
-- state changes (mode transitions, messages, field storage), it returns
-- a list of operations that Rust executes generically.
--
-- Called from Rust: actions.on_action(action, context)
-- Returns: list of ops | nil
--   nil   -> Rust handles action generically (scroll, list_up, input_char, etc.)
--   ops   -> Rust executes each op in sequence
--
-- Supported operations:
--   set_mode         { op, mode }         - Set UI mode string, reset list selection
--   send_msg         { op, data }         - Send JSON message to Hub via Lua protocol
--   store_field      { op, key, value }   - Store key-value in pending_fields
--   clear_field      { op, key }          - Remove key from pending_fields
--   clear_input      { op }               - Clear text input buffer
--   reset_list       { op }               - Reset overlay list selection to 0
--   toggle_pty       { op }               - Cycle to next PTY session
--   switch_pty       { op, index }        - Switch to specific PTY by index
--   select_next      { op }               - Select next agent
--   select_previous  { op }               - Select previous agent
--   quit             { op }               - Request application quit (Lua should send_msg first)
--
-- Note: context.list_selected is 0-based (from Rust). Lua tables are 1-based,
-- so add 1 when indexing into Lua arrays (e.g., overlay_actions[list_selected + 1]).

local M = {}

--- Dispatch an action string with context, returning compound ops or nil.
-- @param action string Action name from keybindings
-- @param context table { mode, input_buffer, list_selected, overlay_actions,
--                        pending_fields, selected_agent, available_worktrees }
-- @return table|nil List of op tables, or nil for generic Rust handling
function M.on_action(action, context)
  -- === Menu selection ===
  if action == "list_select" and context.mode == "menu" then
    local actions = context.overlay_actions or {}
    local selected = actions[context.list_selected + 1]  -- Lua 1-based

    if selected == "new_agent" then
      return {
        { op = "set_mode", mode = "new_agent_select_worktree" },
        { op = "reset_list" },
        { op = "send_msg", data = {
          subscriptionId = "tui_hub",
          data = { type = "list_worktrees" },
        }},
      }
    elseif selected == "close_agent" then
      if context.selected_agent then
        return { { op = "set_mode", mode = "close_agent_confirm" } }
      end
      return { { op = "set_mode", mode = "normal" } }
    elseif selected == "show_connection_code" then
      return {
        { op = "set_mode", mode = "connection_code" },
        { op = "send_msg", data = {
          subscriptionId = "tui_hub",
          data = { type = "get_connection_code" },
        }},
      }
    elseif selected == "toggle_pty" then
      return {
        { op = "toggle_pty" },
        { op = "set_mode", mode = "normal" },
      }
    else
      -- switch_session:N — switch to a specific PTY by index
      local session_idx = selected and string.match(selected, "^switch_session:(%d+)$")
      if session_idx then
        return {
          { op = "switch_pty", index = tonumber(session_idx) },
          { op = "set_mode", mode = "normal" },
        }
      end
    end
    -- Unknown or nil action: close menu
    return { { op = "set_mode", mode = "normal" } }
  end

  -- === Worktree selection ===
  if action == "list_select" and context.mode == "new_agent_select_worktree" then
    if context.list_selected == 0 then
      -- First item: "Create New Worktree"
      return {
        { op = "set_mode", mode = "new_agent_create_worktree" },
        { op = "clear_input" },
      }
    else
      -- list_selected is 0-based; index 0 is "Create New" (handled above).
      -- Indices 1+ map directly to available_worktrees (Lua 1-based).
      local wt_idx = context.list_selected
      local worktrees = context.available_worktrees or {}
      local wt = worktrees[wt_idx]
      if wt then
        return {
          { op = "send_msg", data = {
            subscriptionId = "tui_hub",
            data = { type = "reopen_worktree", path = wt.path, branch = wt.branch },
          }},
          { op = "store_field", key = "creating_agent_id", value = wt.branch },
          { op = "store_field", key = "creating_agent_stage", value = "creating_worktree" },
          { op = "set_mode", mode = "normal" },
        }
      end
    end
    return { { op = "set_mode", mode = "normal" } }
  end

  -- === Text input submit ===
  if action == "input_submit" then
    if context.mode == "new_agent_create_worktree" and (context.input_buffer or "") ~= "" then
      return {
        { op = "store_field", key = "pending_issue_or_branch", value = context.input_buffer },
        { op = "clear_input" },
        { op = "set_mode", mode = "new_agent_prompt" },
      }
    elseif context.mode == "new_agent_prompt" then
      local pending = context.pending_fields or {}
      local issue = pending.pending_issue_or_branch
      if issue then
        local prompt = nil
        if (context.input_buffer or "") ~= "" then
          prompt = context.input_buffer
        end
        return {
          { op = "send_msg", data = {
            subscriptionId = "tui_hub",
            data = { type = "create_agent", issue_or_branch = issue, prompt = prompt },
          }},
          { op = "store_field", key = "creating_agent_id", value = issue },
          { op = "store_field", key = "creating_agent_stage", value = "creating_worktree" },
          { op = "clear_field", key = "pending_issue_or_branch" },
          { op = "clear_input" },
          { op = "set_mode", mode = "normal" },
        }
      else
        return { { op = "set_mode", mode = "normal" }, { op = "clear_input" } }
      end
    end
  end

  -- === Confirm close agent ===
  if action == "confirm_close" and context.mode == "close_agent_confirm" then
    if context.selected_agent then
      return {
        { op = "send_msg", data = {
          subscriptionId = "tui_hub",
          data = { type = "delete_agent", agent_id = context.selected_agent, delete_worktree = false },
        }},
        { op = "set_mode", mode = "normal" },
      }
    end
    return { { op = "set_mode", mode = "normal" } }
  end

  if action == "confirm_close_delete" and context.mode == "close_agent_confirm" then
    if context.selected_agent then
      return {
        { op = "send_msg", data = {
          subscriptionId = "tui_hub",
          data = { type = "delete_agent", agent_id = context.selected_agent, delete_worktree = true },
        }},
        { op = "set_mode", mode = "normal" },
      }
    end
    return { { op = "set_mode", mode = "normal" } }
  end

  -- === Connection code actions ===
  if action == "regenerate_connection_code" then
    return {
      { op = "send_msg", data = {
        subscriptionId = "tui_hub",
        data = { type = "regenerate_connection_code" },
      }},
      { op = "clear_field", key = "connection_code" },
      { op = "send_msg", data = {
        subscriptionId = "tui_hub",
        data = { type = "get_connection_code" },
      }},
    }
  end

  if action == "copy_connection_url" then
    return {
      { op = "send_msg", data = {
        subscriptionId = "tui_hub",
        data = { type = "copy_connection_url" },
      }},
    }
  end

  -- === PTY session cycling (Ctrl+] in normal mode) ===
  if action == "toggle_pty" then
    return { { op = "toggle_pty" } }
  end

  -- === Modal state ===
  if action == "open_menu" then
    return { { op = "set_mode", mode = "menu" } }
  end

  if action == "close_modal" then
    return { { op = "set_mode", mode = "normal" } }
  end

  -- === Agent navigation ===
  if action == "select_next" then
    return { { op = "select_next" } }
  end

  if action == "select_previous" then
    return { { op = "select_previous" } }
  end

  -- === Application control ===
  if action == "quit" then
    return {
      { op = "send_msg", data = {
        subscriptionId = "tui_hub",
        data = { type = "quit" },
      }},
      { op = "quit" },
    }
  end

  -- Not handled: Rust handles generically
  return nil
end

return M
