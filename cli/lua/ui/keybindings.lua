-- ui/keybindings.lua — Declarative keybinding tables for the TUI.
--
-- Maps key descriptors (built by Rust from crossterm events) to action names.
-- Rust calls handle_key() for every keypress; the return value tells Rust
-- what to do:
--
--   { action = "open_menu" }              -- generic: Rust maps to TuiAction directly
--   { action = "list_select", index = 2 } -- compound: Rust calls actions.on_action()
--   { action = "input_char", char = "a" } -- generic with extra data
--   nil                                   -- insert: forward raw bytes to PTY
--                                         -- modal: swallow key (no-op)
--                                         -- normal: swallow key (Rust gates PTY on mode)
--
-- Key descriptor format (built by Rust):
--   Modifiers prefix-sorted: ctrl+shift+alt+<key>
--   Examples: "a", "enter", "ctrl+p", "shift+enter", "shift+pageup"
--
-- Safety: Ctrl+Q is hardcoded in Rust and never reaches Lua.
--
-- Modes:
--   normal  - Command mode. Single-key bindings available. No PTY forwarding.
--   insert  - PTY mode. Keys forward to terminal. Only modifier combos bound.

local M = {}

-- =============================================================================
-- Shared modifier bindings (used in both normal and insert)
-- =============================================================================

local shared_bindings = {
  ["ctrl+p"]         = "open_menu",
  ["ctrl+j"]         = "select_next",
  ["ctrl+k"]         = "select_previous",
  ["ctrl+]"]         = "toggle_pty",
  ["shift+pageup"]   = "scroll_half_up",
  ["shift+pagedown"] = "scroll_half_down",
  ["shift+home"]     = "scroll_top",
  ["shift+end"]      = "scroll_bottom",
  ["ctrl+r"]         = "refresh_agents",
}

-- Normal mode: command mode, single-key bindings available
M.normal = {}
for k, v in pairs(shared_bindings) do M.normal[k] = v end
M.normal["i"] = "enter_insert_mode"

-- Insert mode: PTY forwarding, only modifier combos
M.insert = {}
for k, v in pairs(shared_bindings) do M.insert[k] = v end
-- Escape is NOT bound in insert mode — it forwards to PTY (vim, etc.)

M.menu = {
  ["escape"]  = "close_modal",
  ["q"]       = "close_modal",
  ["up"]      = "list_up",
  ["k"]       = "list_up",
  ["down"]    = "list_down",
  ["j"]       = "list_down",
  ["enter"]   = "list_select",
  ["space"]   = "list_select",
  -- 1-9 number shortcuts handled in fallback logic below
}

-- List navigation table (shared by worktree and similar list modes)
local list_nav = {
  ["escape"] = "close_modal",
  ["q"]      = "close_modal",
  ["up"]     = "list_up",
  ["k"]      = "list_up",
  ["down"]   = "list_down",
  ["j"]      = "list_down",
  ["enter"]  = "list_select",
  ["space"]  = "list_select",
}

-- Text input table (shared by text entry modes)
local text_input = {
  ["escape"]          = "close_modal",
  ["enter"]           = "input_submit",
  ["left"]            = "input_cursor_left",
  ["right"]           = "input_cursor_right",
  ["home"]            = "input_cursor_home",
  ["end"]             = "input_cursor_end",
  ["ctrl+left"]       = "input_word_left",
  ["ctrl+right"]      = "input_word_right",
  ["delete"]          = "input_delete",
  ["ctrl+backspace"]  = "input_word_backspace",
  -- Characters and backspace handled in fallback logic below
}

-- Mode table aliases — mode strings match Lua layout mode names
M.new_agent_select_worktree = list_nav
M.new_agent_select_profile = list_nav
M.new_agent_create_worktree = text_input
M.new_agent_prompt = text_input

M.close_agent_confirm = {
  ["escape"] = "close_modal",
  ["n"]      = "close_modal",
  ["q"]      = "close_modal",
  ["y"]      = "confirm_close",
  ["enter"]  = "confirm_close",
  ["d"]      = "confirm_close_delete",
}

M.connection_code = {
  ["escape"] = "close_modal",
  ["q"]      = "close_modal",
  ["enter"]  = "close_modal",
  ["c"]      = "copy_connection_url",
  ["r"]      = "regenerate_connection_code",
}

M.error = {
  ["escape"] = "close_modal",
  ["q"]      = "close_modal",
  ["enter"]  = "close_modal",
}

-- =============================================================================
-- Key Handler
-- =============================================================================

--- Handle a key event. Called by Rust for every keypress (except Ctrl+Q).
-- @param key string Key descriptor (e.g., "ctrl+p", "shift+enter")
-- @param mode string Current mode (matches Lua binding table names)
-- @param context table { list_selected, list_count, terminal_rows }
-- @return table|nil Action table or nil (insert: PTY forward; other: swallow)
function M.handle_key(key, mode, context)
  -- Mode-specific binding lookup
  local bindings = M[mode]
  if bindings then
    local action = bindings[key]
    if action then
      return { action = action }
    end
  end

  -- Mode-specific fallback logic
  if mode == "new_agent_create_worktree" or mode == "new_agent_prompt" then
    if key == "backspace" then
      return { action = "input_backspace" }
    end
    if key == "space" then
      return { action = "input_char", char = " " }
    end
    if #key == 1 then
      return { action = "input_char", char = key }
    end
    return nil
  end

  if mode == "menu" or mode == "new_agent_select_worktree" or mode == "new_agent_select_profile" then
    -- Number shortcuts 1-9 for list selection
    if mode == "menu" and #key == 1 and key:match("%d") then
      local idx = tonumber(key) - 1
      if idx < (context.list_count or 0) then
        return { action = "list_select", index = idx }
      end
    end
    return nil
  end

  if mode == "close_agent_confirm" or mode == "connection_code" or mode == "error" then
    return nil
  end

  -- Normal/insert: unbound keys return nil.
  -- Rust gates PTY forwarding on mode == "insert".
  return nil
end

return M
