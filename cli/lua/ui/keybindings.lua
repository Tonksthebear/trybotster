-- ui/keybindings.lua — Declarative keybinding tables for the TUI.
--
-- Maps key descriptors (built by Rust from crossterm events) to action names.
-- Rust calls handle_key() for every keypress; the return value tells Rust
-- what to do:
--
--   { action = "open_menu" }     -- Rust maps to TuiAction
--   { action = "input_char", char = "a" }  -- action with extra data
--   nil                          -- normal mode: forward to PTY; modal: ignore
--
-- Key descriptor format (built by Rust):
--   Modifiers prefix-sorted: ctrl+shift+alt+<key>
--   Examples: "a", "enter", "ctrl+p", "shift+enter", "shift+pageup"
--
-- Safety: Ctrl+Q is hardcoded in Rust and never reaches Lua.

local M = {}

-- =============================================================================
-- Binding Tables (one per app mode)
-- =============================================================================

M.normal = {
  ["ctrl+p"]         = "open_menu",
  ["ctrl+j"]         = "select_next",
  ["ctrl+k"]         = "select_previous",
  ["ctrl+]"]         = "toggle_pty",
  ["shift+pageup"]   = "scroll_half_up",
  ["shift+pagedown"] = "scroll_half_down",
  ["shift+home"]     = "scroll_top",
  ["shift+end"]      = "scroll_bottom",
  -- Everything else falls through to PTY forwarding
}

M.menu = {
  ["escape"]  = "close_modal",
  ["q"]       = "close_modal",
  ["up"]      = "menu_up",
  ["k"]       = "menu_up",
  ["down"]    = "menu_down",
  ["j"]       = "menu_down",
  ["enter"]   = "menu_select",
  ["space"]   = "menu_select",
  -- 1-9 number shortcuts handled in fallback logic below
}

M.worktree_select = {
  ["escape"] = "close_modal",
  ["q"]      = "close_modal",
  ["up"]     = "worktree_up",
  ["k"]      = "worktree_up",
  ["down"]   = "worktree_down",
  ["j"]      = "worktree_down",
  ["enter"]  = "worktree_select",
  ["space"]  = "worktree_select",
}

M.text_input = {
  ["escape"] = "close_modal",
  ["enter"]  = "input_submit",
  -- Characters and backspace handled in fallback logic below
}

M.close_confirm = {
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
-- @param mode string Current app mode (matches Lua binding table names)
-- @param context table { menu_selected, menu_count, worktree_selected, terminal_rows }
-- @return table|nil Action table or nil for default PTY forwarding
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
  if mode == "text_input" then
    if key == "backspace" then
      return { action = "input_backspace" }
    end
    -- Space is encoded as "space" descriptor
    if key == "space" then
      return { action = "input_char", char = " " }
    end
    -- Single printable character -> input_char
    if #key == 1 then
      return { action = "input_char", char = key }
    end
    return nil  -- ignore unbound keys in text input
  end

  if mode == "menu" or mode == "worktree_select" then
    -- Number shortcuts 1-9 for menu selection
    if mode == "menu" and #key == 1 and key:match("%d") then
      local idx = tonumber(key) - 1
      if idx < (context.menu_count or 0) then
        return { action = "menu_select", index = idx }
      end
    end
    return nil  -- ignore unbound keys in modals
  end

  if mode == "close_confirm" or mode == "connection_code" or mode == "error" then
    return nil  -- ignore unbound keys in these modals
  end

  -- Normal mode: unbound keys -> PTY forwarding (return nil)
  -- Rust will convert the key to ANSI bytes and send to PTY
  return nil
end

return M
