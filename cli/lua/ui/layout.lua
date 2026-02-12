-- ui/layout.lua — Declarative TUI layout definition.
--
-- Defines the structure (splits, sizes, widget placement) of the TUI.
-- Rust interprets the returned tables into ratatui rendering calls.
-- Rust is a generic UI toolkit with zero application knowledge —
-- all content, styling, and behavior wiring lives here.
--
-- Available node types:
--   hsplit     — horizontal split with constraints and children
--   vsplit     — vertical split with constraints and children
--   centered   — centered overlay with width/height percentages
--   list       — generic selectable list with optional headers
--   paragraph  — static styled text block
--   input      — text input with prompt lines
--   terminal   — PTY output panel
--   connection_code — QR code display (special rendering)
--   empty      — renders just the block border/title
--
-- Constraint formats: "30%" (percentage), "20" (fixed), "min:10" (min), "max:80" (max)
--
-- Styled span syntax:
--   Plain string:  "title text"
--   Styled spans:  { { text = "bold", style = "bold" }, { text = " dim", style = "dim" } }
--   Style table:   { text = "colored", style = { fg = "cyan", bold = true } }
--   Shorthand:     "bold" = { bold = true }, "dim" = { dim = true }

-- =============================================================================
-- Helper: Build agent list items from state
-- =============================================================================
local function build_agent_items(state)
  local items = {}

  -- Creating indicator at top
  if state.creating_agent then
    local stages = {
      creating_worktree = "Creating worktree...",
      copying_config = "Copying config...",
      spawning_agent = "Starting agent...",
      ready = "Ready",
    }
    table.insert(items, {
      text = string.format("-> %s (%s)", state.creating_agent.identifier,
             stages[state.creating_agent.stage] or "..."),
      style = { fg = "cyan" },
    })
  end

  -- Existing agents
  for _, agent in ipairs(state.agents or {}) do
    local name = agent.display_name or agent.branch_name
    local si = ""
    if agent.port then
      local icon = agent.server_running and ">" or "o"
      si = string.format(" %s:%d", icon, agent.port)
    end
    table.insert(items, { text = name .. si })
  end

  return items
end

-- =============================================================================
-- Helper: Build menu items from state
-- =============================================================================
local function build_menu_items(state)
  local items = {}
  local sa = state.selected_agent

  -- Agent section (only if agent selected)
  if sa then
    table.insert(items, { text = "── Agent ──", header = true })
    if (sa.session_count or 0) > 1 then
      table.insert(items, { text = "Next Session (Ctrl+])" })
    end
    table.insert(items, { text = "Close Agent" })
  end

  -- Hub section (always shown)
  table.insert(items, { text = "── Hub ──", header = true })
  table.insert(items, { text = "New Agent" })
  table.insert(items, { text = "Show Connection Code" })

  return items
end

-- =============================================================================
-- Helper: Build worktree items from state
-- =============================================================================
local function build_worktree_items(state)
  local items = { { text = "[Create New Worktree]" } }
  for _, wt in ipairs(state.available_worktrees or {}) do
    table.insert(items, { text = string.format("%s (%s)", wt.branch, wt.path) })
  end
  return items
end

--- Main layout: agent list + terminal panel.
function render(state)
  -- Agent list title: count + poll indicator
  local poll_icon = state.seconds_since_poll < 1 and "*" or "o"
  local agent_title = {
    { text = string.format(" Agents (%d) ", state.agent_count) },
    { text = poll_icon .. " ", style = { fg = "cyan" } },
  }

  -- Agent list: selection offset accounts for creating indicator
  local agent_selected = state.selected_agent_index
  if state.creating_agent then agent_selected = agent_selected + 1 end

  -- Terminal title: branch name, session view, scroll indicator
  local term_title = " Terminal [No agent selected] "
  if state.selected_agent then
    local sa = state.selected_agent
    local session = sa.session_names[state.active_pty_index + 1] or "agent"
    local view = string.upper(session)
    if sa.session_count > 1 then
      view = "[" .. view .. " | Ctrl+]: next]"
    else
      view = "[" .. view .. "]"
    end
    local scroll = ""
    if state.is_scrolled then
      scroll = string.format(" [SCROLLBACK +%d | Shift+End: live]", state.scroll_offset)
    end
    term_title = string.format(" %s %s%s [Ctrl+P | Ctrl+J/K | Shift+PgUp/Dn scroll] ",
      sa.branch_name, view, scroll)
  end

  -- Build terminal panel: side-by-side if 2+ agents, single otherwise
  local terminal_panel
  if state.agent_count >= 2 then
    local function agent_label(idx)
      local a = state.agents and state.agents[idx + 1]
      if a then return " " .. (a.display_name or a.branch_name or "Agent " .. idx) .. " " end
      return string.format(" Agent %d ", idx)
    end
    local function terminal_block(idx, pty)
      local is_focused = (idx == state.selected_agent_index and pty == state.active_pty_index)
      return {
        title = agent_label(idx),
        borders = "all",
        border_style = is_focused and { fg = "cyan" } or nil,
      }
    end
    terminal_panel = {
      type = "vsplit",
      constraints = { "50%", "50%" },
      children = {
        { type = "terminal", props = { agent_index = 0, pty_index = 0 },
          block = terminal_block(0, 0) },
        { type = "terminal", props = { agent_index = 1, pty_index = 0 },
          block = terminal_block(1, 0) },
      },
    }
  else
    terminal_panel = { type = "terminal", block = { title = term_title, borders = "all" } }
  end

  return {
    type = "hsplit",
    constraints = { "10%", "90%" },
    children = {
      {
        type = "list",
        block = { title = agent_title, borders = "all" },
        props = {
          items = build_agent_items(state),
          selected = agent_selected,
        },
      },
      terminal_panel,
    },
  }
end

--- Overlay layout: returns a centered modal based on current mode, or nil.
function render_overlay(state)
  if state.mode == "menu" then
    return {
      type = "centered", width = 50, height = 40,
      child = {
        type = "list",
        block = { title = " Menu [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = build_menu_items(state),
          selected = state.menu_selected,
        },
      },
    }
  elseif state.mode == "new_agent_select_worktree" then
    return {
      type = "centered", width = 70, height = 50,
      child = {
        type = "list",
        block = { title = " Select Worktree [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = build_worktree_items(state),
          selected = state.worktree_selected,
        },
      },
    }
  elseif state.mode == "new_agent_create_worktree" then
    return {
      type = "centered", width = 60, height = 30,
      child = {
        type = "input",
        block = { title = " Create Worktree [Enter confirm | Esc cancel] ", borders = "all" },
        props = {
          lines = {
            "Enter branch name or issue number:",
            "",
            "Examples: 123, feature-auth, bugfix-login",
          },
          value = state.input_buffer or "",
        },
      },
    }
  elseif state.mode == "new_agent_prompt" then
    return {
      type = "centered", width = 60, height = 20,
      child = {
        type = "input",
        block = { title = " Agent Prompt [Enter confirm | Esc cancel] ", borders = "all" },
        props = {
          lines = {
            "Enter prompt for agent (leave empty for default):",
          },
          value = state.input_buffer or "",
        },
      },
    }
  elseif state.mode == "close_agent_confirm" then
    return {
      type = "centered", width = 50, height = 20,
      child = {
        type = "paragraph",
        block = { title = " Confirm Close ", borders = "all" },
        props = {
          lines = {
            "Close selected agent?",
            "",
            "Y - Close agent (keep worktree)",
            "D - Close agent and delete worktree",
            "N/Esc - Cancel",
          },
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
        type = "paragraph",
        block = { title = " Error ", borders = "all" },
        props = {
          lines = {
            "",
            { { text = "Error", style = "bold" } },
            "",
            state.error_message or "An error occurred",
            "",
            { { text = "[Esc/Enter] dismiss", style = "dim" } },
          },
          alignment = "center",
          wrap = true,
        },
      },
    }
  end

  return nil
end
