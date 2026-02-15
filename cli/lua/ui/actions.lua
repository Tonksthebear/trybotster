-- ui/actions.lua — Application workflow dispatch for compound actions.
--
-- Keybindings.lua stays a pure lookup table (key -> action string).
-- This module handles *workflow* dispatch: when an action needs multiple
-- state changes (mode transitions, messages, field storage), it returns
-- a list of operations that Rust executes generically.
--
-- The TUI is a client consuming hub events (same as the browser).
-- Client-side state lives in _tui_state; actions read/write it directly.
-- Only primitive ops (send_msg, focus_terminal, quit) are returned to Rust.
--
-- Called from Rust: actions.on_action(action, context)
-- Returns: list of ops | nil
--   nil   -> Rust ignores (no action needed)
--   ops   -> Rust executes each op in sequence
--
-- Supported operations:
--   set_mode         { op, mode }                   - Update Rust's mode shadow
--   send_msg         { op, data }                   - Send JSON message to Hub via Lua protocol
--   focus_terminal   { op, agent_id, pty_index, agent_index }
--   quit             { op }                         - Request application quit
--
-- Context fields (Rust-owned):
--   overlay_actions, selected_agent
--
-- Client-side state (_tui_state):
--   mode, input_buffer, list_selected, agents, pending_fields, available_worktrees
--
-- Note: _tui_state.list_selected is 0-based. Lua tables are 1-based,
-- so add 1 when indexing into Lua arrays (e.g., overlay_actions[list_selected + 1]).

local M = {}

--- Set mode in _tui_state and return the set_mode op for Rust's shadow.
--- Also resets list_selected and clears input_buffer (mode change side effects).
local function set_mode_ops(mode)
  _tui_state.mode = mode
  _tui_state.list_selected = 0
  _tui_state.input_buffer = ""
  return { op = "set_mode", mode = mode }
end

--- Return the appropriate base mode: "insert" if an agent is selected, "normal" otherwise.
local function base_mode(context)
  return context.selected_agent and "insert" or "normal"
end

--- Check if the currently selected agent is on the main branch.
local function selected_agent_on_main(context)
  local agent_id = context.selected_agent
  if not agent_id then return false end
  for _, a in ipairs(_tui_state.agents) do
    if a.id == agent_id then return a.branch_name == "main" end
  end
  return false
end

--- Look up 0-based agent index from agent_id in client state.
local function agent_index_for(agent_id)
  for i, a in ipairs(_tui_state.agents) do
    if a.id == agent_id then return i - 1 end
  end
  return nil
end

--- Dispatch an action string with context, returning compound ops or nil.
-- @param action string Action name from keybindings
-- @param context table Action context with all TUI state
-- @return table|nil List of op tables, or nil for generic Rust handling
function M.on_action(action, context)

  -- Note: Widget-intrinsic actions (list_up, list_down, input_char, input_backspace,
  -- cursor movement) are handled by Rust's WidgetStateStore and synced back to
  -- _tui_state.list_selected / _tui_state.input_buffer automatically.
  -- Only workflow actions (list_select, input_submit, mode transitions) remain here.

  -- === Mode transitions ===
  if action == "enter_normal_mode" then
    return { set_mode_ops("normal") }
  end

  if action == "enter_insert_mode" then
    if context.selected_agent then
      return { set_mode_ops("insert") }
    end
    return nil  -- no agent, can't insert
  end

  -- === Menu selection ===
  if action == "list_select" and _tui_state.mode == "menu" then
    local actions = context.overlay_actions or {}
    local selected = actions[_tui_state.list_selected + 1]  -- Lua 1-based

    if selected == "new_agent" then
      return {
        set_mode_ops("new_agent_select_worktree"),
        { op = "send_msg", data = {
          subscriptionId = "tui_hub",
          data = { type = "list_worktrees" },
        }},
      }
    elseif selected == "close_agent" then
      if context.selected_agent then
        return { set_mode_ops("close_agent_confirm") }
      end
      return { set_mode_ops(base_mode(context)) }
    elseif selected == "show_connection_code" then
      return {
        set_mode_ops("connection_code"),
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
          { op = "focus_terminal", agent_id = context.selected_agent, pty_index = tonumber(session_idx),
            agent_index = agent_index_for(context.selected_agent) },
          set_mode_ops(base_mode(context)),
        }
      end
    end
    -- Unknown or nil action: close menu
    return { set_mode_ops(base_mode(context)) }
  end

  -- === Worktree selection ===
  -- List is 0-based: 0 = "Use Main Branch", 1 = "Create New Worktree", 2+ = existing worktrees
  if action == "list_select" and _tui_state.mode == "new_agent_select_worktree" then
    local ls = _tui_state.list_selected
    if ls == 0 then
      -- "Use Main Branch" — skip worktree, go straight to prompt
      _tui_state.pending_fields.pending_issue_or_branch = nil
      _tui_state.pending_fields.use_main_branch = "true"
      return { set_mode_ops("new_agent_prompt") }
    elseif ls == 1 then
      -- "Create New Worktree"
      _tui_state.pending_fields.use_main_branch = nil
      return { set_mode_ops("new_agent_create_worktree") }
    else
      -- Existing worktree. Index 2+ maps to available_worktrees[1+] (Lua 1-based).
      local wt_idx = ls - 1
      local worktrees = _tui_state.available_worktrees or {}
      local wt = worktrees[wt_idx]
      if wt then
        _tui_state.pending_fields.creating_agent_id = wt.branch
        _tui_state.pending_fields.creating_agent_stage = "creating_worktree"
        return {
          { op = "send_msg", data = {
            subscriptionId = "tui_hub",
            data = { type = "reopen_worktree", path = wt.path, branch = wt.branch },
          }},
          set_mode_ops(base_mode(context)),
        }
      end
    end
    return { set_mode_ops(base_mode(context)) }
  end

  -- === Text input submit ===
  if action == "input_submit" then
    local input = _tui_state.input_buffer or ""
    if _tui_state.mode == "new_agent_create_worktree" and input ~= "" then
      _tui_state.pending_fields.pending_issue_or_branch = input
      return { set_mode_ops("new_agent_prompt") }
    elseif _tui_state.mode == "new_agent_prompt" then
      local pf = _tui_state.pending_fields
      local issue = pf.pending_issue_or_branch
      local use_main = pf.use_main_branch

      -- Main branch mode: issue is nil, handler spawns in repo root
      -- Worktree mode: issue is set, handler creates/finds worktree
      if issue or use_main then
        local prompt = nil
        if input ~= "" then
          prompt = input
        end
        _tui_state.pending_fields.creating_agent_id = issue or "main"
        _tui_state.pending_fields.creating_agent_stage = use_main and "spawning" or "creating_worktree"
        _tui_state.pending_fields.pending_issue_or_branch = nil
        _tui_state.pending_fields.use_main_branch = nil
        return {
          { op = "send_msg", data = {
            subscriptionId = "tui_hub",
            data = { type = "create_agent", issue_or_branch = issue, prompt = prompt },
          }},
          set_mode_ops(base_mode(context)),
        }
      else
        return { set_mode_ops(base_mode(context)) }
      end
    end
  end

  -- === Confirm close agent ===
  if action == "confirm_close" and _tui_state.mode == "close_agent_confirm" then
    if context.selected_agent then
      return {
        { op = "send_msg", data = {
          subscriptionId = "tui_hub",
          data = { type = "delete_agent", agent_id = context.selected_agent, delete_worktree = false },
        }},
        set_mode_ops("normal"),
      }
    end
    return { set_mode_ops(base_mode(context)) }
  end

  if action == "confirm_close_delete" and _tui_state.mode == "close_agent_confirm" then
    -- Don't allow deleting worktree when agent is on main branch
    if selected_agent_on_main(context) then return nil end
    if context.selected_agent then
      return {
        { op = "send_msg", data = {
          subscriptionId = "tui_hub",
          data = { type = "delete_agent", agent_id = context.selected_agent, delete_worktree = true },
        }},
        set_mode_ops("normal"),
      }
    end
    return { set_mode_ops(base_mode(context)) }
  end

  -- === Connection code actions ===
  if action == "regenerate_connection_code" then
    _tui_state.pending_fields.connection_code = nil
    return {
      { op = "send_msg", data = {
        subscriptionId = "tui_hub",
        data = { type = "regenerate_connection_code" },
      }},
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
    -- Read session count from client-side agent state
    local agent_id = context.selected_agent
    local session_count = 1
    for _, a in ipairs(_tui_state.agents) do
      if a.id == agent_id then
        session_count = a.sessions and #a.sessions or 1
        break
      end
    end
    local next_pty = ((_tui_state.active_pty_index or 0) + 1) % session_count
    _tui_state.active_pty_index = next_pty
    return {
      { op = "focus_terminal", agent_id = agent_id, pty_index = next_pty,
        agent_index = agent_index_for(agent_id) },
    }
  end

  -- === Modal state ===
  if action == "open_menu" then
    return { set_mode_ops("menu") }
  end

  if action == "close_modal" then
    return { set_mode_ops(base_mode(context)) }
  end

  -- === Agent navigation ===
  if action == "select_next" then
    local agents = _tui_state.agents
    if #agents == 0 then return nil end
    local current_idx = _tui_state.selected_agent_index  -- 0-based or nil
    local next_idx
    if current_idx then
      next_idx = (current_idx + 1) % #agents
    else
      next_idx = 0
    end
    local next_agent = agents[next_idx + 1]  -- Lua 1-based
    if not next_agent then return nil end
    _tui_state.selected_agent_index = next_idx
    _tui_state.active_pty_index = 0
    return {
      { op = "focus_terminal", agent_id = next_agent.id, pty_index = 0, agent_index = next_idx },
      set_mode_ops("insert"),
    }
  end

  if action == "select_previous" then
    local agents = _tui_state.agents
    if #agents == 0 then return nil end
    local current_idx = _tui_state.selected_agent_index  -- 0-based or nil
    local prev_idx
    if current_idx then
      prev_idx = (current_idx - 1 + #agents) % #agents
    else
      prev_idx = #agents - 1
    end
    local prev_agent = agents[prev_idx + 1]  -- Lua 1-based
    if not prev_agent then return nil end
    _tui_state.selected_agent_index = prev_idx
    _tui_state.active_pty_index = 0
    return {
      { op = "focus_terminal", agent_id = prev_agent.id, pty_index = 0, agent_index = prev_idx },
      set_mode_ops("insert"),
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
