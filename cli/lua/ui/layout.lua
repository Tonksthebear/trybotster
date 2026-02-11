-- ui/layout.lua — Declarative TUI layout definition.
--
-- Defines the structure (splits, sizes, widget placement) of the TUI.
-- Rust interprets the returned tables into ratatui rendering calls.
--
-- Available node types:
--   hsplit  — horizontal split with constraints and children
--   vsplit  — vertical split with constraints and children
--   centered — centered overlay with width/height percentages
--   <widget> — leaf widget rendered by Rust (agent_list, terminal, menu, etc.)
--
-- Constraint formats: "30%" (percentage), "20" (fixed), "min:10" (min), "max:80" (max)

--- Main layout: 30/70 horizontal split with agent list and terminal.
function render(state)
  return {
    type = "hsplit",
    constraints = { "20%", "80%" },
    children = {
      { type = "agent_list" },
      { type = "terminal" },
    },
  }
end

--- Overlay layout: returns a centered modal based on current mode, or nil.
function render_overlay(state)
  if state.mode == "menu" then
    return {
      type = "centered", width = 50, height = 40,
      child = {
        type = "menu",
        block = { title = " Menu [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
      },
    }
  elseif state.mode == "new_agent_select_worktree" then
    return {
      type = "centered", width = 70, height = 50,
      child = {
        type = "worktree_select",
        block = { title = " Select Worktree [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
      },
    }
  elseif state.mode == "new_agent_create_worktree" then
    return {
      type = "centered", width = 60, height = 30,
      child = {
        type = "text_input",
        block = { title = " Create Worktree [Enter confirm | Esc cancel] ", borders = "all" },
        lines = {
          "Enter branch name or issue number:",
          "",
          "Examples: 123, feature-auth, bugfix-login",
        },
      },
    }
  elseif state.mode == "new_agent_prompt" then
    return {
      type = "centered", width = 60, height = 20,
      child = {
        type = "text_input",
        block = { title = " Agent Prompt [Enter confirm | Esc cancel] ", borders = "all" },
        lines = {
          "Enter prompt for agent (leave empty for default):",
        },
      },
    }
  elseif state.mode == "close_agent_confirm" then
    return {
      type = "centered", width = 50, height = 20,
      child = {
        type = "close_confirm",
        block = { title = " Confirm Close ", borders = "all" },
        lines = {
          "Close selected agent?",
          "",
          "Y - Close agent (keep worktree)",
          "D - Close agent and delete worktree",
          "N/Esc - Cancel",
        },
      },
    }
  elseif state.mode == "connection_code" then
    -- Responsive sizing: fit QR code + header/footer/border, min 60x70%
    local need_w = (state.qr_width or 75) + 4
    local need_h = (state.qr_height or 40) + 8
    local cols = state.terminal_cols or 80
    local rows = state.terminal_rows or 24
    local w_pct = math.max(60, math.min(95, math.ceil(need_w / cols * 100)))
    local h_pct = math.max(70, math.min(95, math.ceil(need_h / rows * 100)))
    return {
      type = "centered", width = w_pct, height = h_pct,
      child = {
        type = "connection_code",
        block = { title = " Secure Connection ", borders = "all" },
        lines = {
          "Scan QR to connect securely",
          "Link used - [r] to pair new device",
          "[r] new link  [c] copy  [Esc] close",
        },
      },
    }
  elseif state.mode == "error" then
    return {
      type = "centered", width = 60, height = 30,
      child = {
        type = "error",
        block = { title = " Error ", borders = "all" },
        lines = {
          "",
          "Error",
          "",
          "{error}",
          "",
          "[Esc/Enter] dismiss",
        },
      },
    }
  end

  return nil
end
