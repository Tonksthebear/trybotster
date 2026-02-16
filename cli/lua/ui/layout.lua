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
-- Client-side state helpers
-- =============================================================================

--- Get creating_agent indicator from client-side pending_fields.
local function get_creating_agent()
  local pf = _tui_state and _tui_state.pending_fields
  if not pf then return nil end
  if pf.creating_agent_id and pf.creating_agent_stage then
    return { identifier = pf.creating_agent_id, stage = pf.creating_agent_stage }
  end
  return nil
end

--- Get selected agent info from client-side agent cache.
local function get_selected_agent()
  local agents = _tui_state and _tui_state.agents or {}
  local idx = _tui_state and _tui_state.selected_agent_index
  if idx == nil then return nil end
  return agents[idx + 1]  -- Lua 1-based
end

-- =============================================================================
-- Helper: Build agent list items from state
-- =============================================================================
local function build_agent_items(state)
  local items = {}

  -- Creating indicator at top
  local creating = get_creating_agent()
  if creating then
    local stages = {
      creating_worktree = "Creating worktree...",
      copying_config = "Copying config...",
      spawning_agent = "Starting agent...",
      spawning = "Starting agent...",
      ready = "Ready",
    }
    table.insert(items, {
      text = string.format("-> %s (%s)", creating.identifier,
             stages[creating.stage] or "..."),
      style = { fg = "cyan" },
    })
  end

  -- Existing agents from client-side cache
  for _, agent in ipairs(_tui_state and _tui_state.agents or {}) do
    local name = agent.display_name or agent.branch_name
    table.insert(items, { text = name })
  end

  return items
end

-- =============================================================================
-- Helper: Build menu items from state
-- =============================================================================
local function build_menu_items(state)
  local items = {}
  local sa = get_selected_agent()

  -- Agent section (only if agent selected)
  if sa then
    table.insert(items, { text = "── Agent ──", header = true })
    local sessions = sa.sessions or {}
    if #sessions > 1 then
      for idx, session in ipairs(sessions) do
        local name = type(session) == "table" and session.name or session
        local label = string.upper(name)
        if (idx - 1) == _tui_state.active_pty_index then
          label = label .. " *"
        end
        table.insert(items, { text = label, action = "switch_session:" .. (idx - 1) })
      end
    end
    table.insert(items, { text = "Close Agent", action = "close_agent" })
  end

  -- Hub section (always shown)
  table.insert(items, { text = "── Hub ──", header = true })
  table.insert(items, { text = "New Agent", action = "new_agent" })
  table.insert(items, { text = "Show Connection Code", action = "show_connection_code" })

  return items
end

-- =============================================================================
-- Helper: Build worktree items from state
-- =============================================================================
local function build_worktree_items()
  local items = {
    { text = "[Use Main Branch]" },
    { text = "[Create New Worktree]" },
  }
  for _, wt in ipairs(_tui_state and _tui_state.available_worktrees or {}) do
    table.insert(items, { text = string.format("%s (%s)", wt.branch, wt.path) })
  end
  return items
end

--- Main layout: agent list + terminal panel.
function render(state)
  local agents = _tui_state and _tui_state.agents or {}
  local agent_count = #agents
  local creating = get_creating_agent()
  local sa = get_selected_agent()

  -- Agent list title: count + poll indicator
  local poll_icon = state.seconds_since_poll < 1 and "*" or "o"
  local agent_title = {
    { text = string.format(" Agents (%d) ", agent_count) },
    { text = poll_icon .. " ", style = { fg = "cyan" } },
  }

  -- Agent list: selection offset accounts for creating indicator
  local agent_selected = _tui_state.selected_agent_index
  if agent_selected and creating then
    agent_selected = agent_selected + 1
  end

  -- Terminal title: branch name, session view, scroll indicator, mode
  local term_title = " Terminal [No agent selected] "
  if sa then
    local session_names = sa.sessions or {}
    local pty_idx = _tui_state.active_pty_index or 0
    local session_name = session_names[pty_idx + 1]
    if type(session_name) == "table" then session_name = session_name.name end
    session_name = session_name or "agent"
    local session_label = string.upper(session_name)
    -- Show forwarded port if this session has one
    local session_info = sa.sessions and sa.sessions[pty_idx + 1]
    if type(session_info) == "table" and session_info.port then
      session_label = session_label .. " :" .. session_info.port
    end
    local view
    local session_count = #session_names
    if session_count > 1 then
      view = "[" .. session_label .. " | Ctrl+]: next]"
    else
      view = "[" .. session_label .. "]"
    end
    local scroll = ""
    if state.is_scrolled then
      scroll = string.format(" [SCROLLBACK +%d | Shift+End: live]", state.scroll_offset)
    end
    local mode_hint = ""
    if _tui_state.mode == "normal" then
      mode_hint = " NORMAL [i: insert] "
    elseif _tui_state.mode == "insert" then
      mode_hint = " INSERT [Esc: normal] "
    end
    term_title = {
      { text = string.format(" %s %s%s ", sa.branch_name or "main", view, scroll) },
      { text = mode_hint, style = { fg = _tui_state.mode == "insert" and "green" or "yellow" } },
    }
  end

  -- Build terminal panel: always show selected agent only
  local terminal_panel = {
    type = "terminal",
    props = { agent_index = _tui_state.selected_agent_index or 0, pty_index = _tui_state.active_pty_index or 0 },
    block = { title = term_title, borders = "all" },
  }

  return {
    type = "hsplit",
    constraints = { "15%", "85%" },
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
  if _tui_state.mode == "menu" then
    return {
      type = "centered", width = 35, height = 30,
      child = {
        type = "list",
        id = "menu",
        block = { title = " Menu [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = build_menu_items(state),
        },
      },
    }
  elseif _tui_state.mode == "new_agent_select_worktree" then
    return {
      type = "centered", width = 70, height = 50,
      child = {
        type = "list",
        id = "worktree_list",
        block = { title = " Select Worktree [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = build_worktree_items(),
        },
      },
    }
  elseif _tui_state.mode == "new_agent_create_worktree" then
    return {
      type = "centered", width = 60, height = 30,
      child = {
        type = "input",
        id = "worktree_input",
        block = { title = " Create Worktree [Enter confirm | Esc cancel] ", borders = "all" },
        props = {
          lines = {
            "Enter branch name or issue number:",
            "",
            "Examples: 123, feature-auth, bugfix-login",
          },
          placeholder = "e.g. 123, feature-auth, bugfix-login",
        },
      },
    }
  elseif _tui_state.mode == "new_agent_prompt" then
    return {
      type = "centered", width = 60, height = 20,
      child = {
        type = "input",
        id = "prompt_input",
        block = { title = " Agent Prompt [Enter confirm | Esc cancel] ", borders = "all" },
        props = {
          lines = {
            "Enter prompt for agent (leave empty for default):",
          },
          placeholder = "Leave empty for default prompt",
        },
      },
    }
  elseif _tui_state.mode == "close_agent_confirm" then
    local sa = get_selected_agent()
    local on_main = sa and sa.branch_name == "main"
    local lines
    if on_main then
      lines = {
        "Close selected agent?",
        "",
        "Y - Close agent",
        "",
        "N/Esc - Cancel",
      }
    else
      lines = {
        "Close selected agent?",
        "",
        "Y - Close agent (keep worktree)",
        "D - Close agent and delete worktree",
        "N/Esc - Cancel",
      }
    end
    return {
      type = "centered", width = 50, height = 20,
      child = {
        type = "paragraph",
        block = { title = " Confirm Close ", borders = "all" },
        props = { lines = lines },
      },
    }
  elseif _tui_state.mode == "connection_code" then
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
  elseif _tui_state.mode == "error" then
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

--- Initial UI mode at boot. Rust calls this once during initialization.
function initial_mode()
  return "normal"
end
