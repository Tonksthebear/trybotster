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
--   set_mode         { op, mode }                   - Set UI mode string, reset list selection
--   send_msg         { op, data }                   - Send JSON message to Hub via Lua protocol
--   store_field      { op, key, value }             - Store key-value in pending_fields
--   clear_field      { op, key }                    - Remove key from pending_fields
--   clear_input      { op }                         - Clear text input buffer
--   reset_list       { op }                         - Reset overlay list selection to 0
--   focus_terminal   { op, agent_id, pty_index }    - Focus a specific agent+PTY (nil clears)
--   quit             { op }                         - Request application quit (Lua should send_msg first)
--
-- Context fields:
--   mode, input_buffer, list_selected, overlay_actions, pending_fields,
--   selected_agent, available_worktrees,
--   agents (array of {id, session_count}), selected_agent_index, active_pty_index
--
-- Note: context.list_selected is 0-based (from Rust). Lua tables are 1-based,
-- so add 1 when indexing into Lua arrays (e.g., overlay_actions[list_selected + 1]).

local M = {}

--- Return the appropriate base mode: "insert" if an agent is selected, "normal" otherwise.
local function base_mode(context)
  return context.selected_agent and "insert" or "normal"
end

--- Dispatch an action string with context, returning compound ops or nil.
-- @param action string Action name from keybindings
-- @param context table Action context with all TUI state
-- @return table|nil List of op tables, or nil for generic Rust handling
function M.on_action(action, context)

  -- === Mode transitions ===
  if action == "enter_normal_mode" then
    return { { op = "set_mode", mode = "normal" } }
  end

  if action == "enter_insert_mode" then
    if context.selected_agent then
      return { { op = "set_mode", mode = "insert" } }
    end
    return nil  -- no agent, can't insert
  end

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
      return { { op = "set_mode", mode = base_mode(context) } }
    elseif selected == "show_connection_code" then
      return {
        { op = "set_mode", mode = "connection_code" },
        { op = "send_msg", data = {
          subscriptionId = "tui_hub",
          data = { type = "get_connection_code" },
        }},
      }
    else
      -- switch_session:N — switch to a specific PTY by index
      local session_idx = selected and string.match(selected, "^switch_session:(%d+)$")
      if session_idx then
        return {
          { op = "focus_terminal", agent_id = context.selected_agent, pty_index = tonumber(session_idx) },
          { op = "set_mode", mode = base_mode(context) },
        }
      end
    end
    -- Unknown or nil action: close menu
    return { { op = "set_mode", mode = base_mode(context) } }
  end

  -- === Worktree selection ===
  -- List is 0-based: 0 = "Use Main Branch", 1 = "Create New Worktree", 2+ = existing worktrees
  if action == "list_select" and context.mode == "new_agent_select_worktree" then
    if context.list_selected == 0 then
      -- "Use Main Branch" — skip worktree, go straight to prompt
      return {
        { op = "clear_field", key = "pending_issue_or_branch" },
        { op = "store_field", key = "use_main_branch", value = "true" },
        { op = "clear_input" },
        { op = "set_mode", mode = "new_agent_prompt" },
      }
    elseif context.list_selected == 1 then
      -- "Create New Worktree"
      return {
        { op = "clear_field", key = "use_main_branch" },
        { op = "set_mode", mode = "new_agent_create_worktree" },
        { op = "clear_input" },
      }
    else
      -- Existing worktree. Index 2+ maps to available_worktrees[1+] (Lua 1-based).
      local wt_idx = context.list_selected - 1
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
          { op = "set_mode", mode = base_mode(context) },
        }
      end
    end
    return { { op = "set_mode", mode = base_mode(context) } }
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
      local use_main = pending.use_main_branch

      -- Main branch mode: issue is nil, handler spawns in repo root
      -- Worktree mode: issue is set, handler creates/finds worktree
      if issue or use_main then
        local prompt = nil
        if (context.input_buffer or "") ~= "" then
          prompt = context.input_buffer
        end
        return {
          { op = "send_msg", data = {
            subscriptionId = "tui_hub",
            data = { type = "create_agent", issue_or_branch = issue, prompt = prompt },
          }},
          { op = "store_field", key = "creating_agent_id", value = issue or "main" },
          { op = "store_field", key = "creating_agent_stage", value = use_main and "spawning" or "creating_worktree" },
          { op = "clear_field", key = "pending_issue_or_branch" },
          { op = "clear_field", key = "use_main_branch" },
          { op = "clear_input" },
          { op = "set_mode", mode = base_mode(context) },
        }
      else
        return { { op = "set_mode", mode = base_mode(context) }, { op = "clear_input" } }
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
    return { { op = "set_mode", mode = base_mode(context) } }
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
    return { { op = "set_mode", mode = base_mode(context) } }
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

  -- === PTY session cycling (Ctrl+] in normal/insert mode) ===
  if action == "toggle_pty" then
    if not context.selected_agent then return nil end
    local agents = context.agents or {}
    local idx = context.selected_agent_index
    if not idx then return nil end
    local agent = agents[idx + 1]  -- Lua 1-based
    if not agent then return nil end
    local session_count = agent.session_count or 1
    local next_pty = ((context.active_pty_index or 0) + 1) % session_count
    return {
      { op = "focus_terminal", agent_id = context.selected_agent, pty_index = next_pty },
    }
  end

  -- === Modal state ===
  if action == "open_menu" then
    return { { op = "set_mode", mode = "menu" } }
  end

  if action == "close_modal" then
    return { { op = "set_mode", mode = base_mode(context) } }
  end

  -- === Agent navigation ===
  if action == "select_next" then
    local agents = context.agents or {}
    if #agents == 0 then return nil end
    local current_idx = context.selected_agent_index  -- 0-based or nil
    local next_idx
    if current_idx then
      next_idx = (current_idx + 1) % #agents
    else
      next_idx = 0
    end
    local next_agent = agents[next_idx + 1]  -- Lua 1-based
    if not next_agent then return nil end
    return {
      { op = "focus_terminal", agent_id = next_agent.id, pty_index = 0 },
      { op = "set_mode", mode = "insert" },
    }
  end

  if action == "select_previous" then
    local agents = context.agents or {}
    if #agents == 0 then return nil end
    local current_idx = context.selected_agent_index  -- 0-based or nil
    local prev_idx
    if current_idx then
      prev_idx = (current_idx - 1 + #agents) % #agents
    else
      prev_idx = #agents - 1
    end
    local prev_agent = agents[prev_idx + 1]  -- Lua 1-based
    if not prev_agent then return nil end
    return {
      { op = "focus_terminal", agent_id = prev_agent.id, pty_index = 0 },
      { op = "set_mode", mode = "insert" },
    }
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
