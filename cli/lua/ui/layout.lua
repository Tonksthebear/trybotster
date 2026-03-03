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
-- Helper: Workspace status display (Phase 3)
-- =============================================================================

local WORKSPACE_STATUS_ICON = {
  -- Agent lifecycle statuses (client-side derived)
  running           = { text = "●", style = { fg = "green" } },
  failed            = { text = "●", style = { fg = "red" } },
  creating_worktree = { text = "○", style = { fg = "yellow" } },
  spawning_ptys     = { text = "○", style = { fg = "yellow" } },
  stopping          = { text = "○", style = { fg = "yellow" } },
  removing_worktree = { text = "○", style = { fg = "yellow" } },
  exited            = { text = "●", style = "dim" },
  idle              = { text = "·", style = "dim" },
  -- Server-provided workspace statuses (workspace_store manifest)
  active            = { text = "●", style = { fg = "green" } },
  suspended         = { text = "○", style = { fg = "yellow" } },
  orphaned          = { text = "●", style = "dim" },
  closed            = { text = "·", style = "dim" },
}

-- =============================================================================
-- Helper: Build list items from workspace-grouped state (Phase 3)
-- =============================================================================

--- Build display items from _tui_state.flat_list for the left panel.
-- Each item corresponds to a flat_list entry (creating indicator, workspace
-- header, or agent row). Returns nil when flat_list is not yet populated.
local function build_list_items()
  local flat = _tui_state and _tui_state.flat_list
  if not flat then return nil end

  local agents_by_id = {}
  for _, a in ipairs(_tui_state and _tui_state.agents or {}) do
    agents_by_id[a.id] = a
  end

  local items = {}
  local pf = _tui_state and _tui_state.pending_fields

  for _, entry in ipairs(flat) do
    if entry.type == "creating" then
      -- In-progress agent creation indicator
      local stages = {
        creating_worktree = "Creating worktree...",
        copying_config    = "Copying config...",
        spawning_agent    = "Starting agent...",
        spawning          = "Starting agent...",
        ready             = "Ready",
      }
      local c_id    = pf and pf.creating_agent_id
      local c_stage = pf and pf.creating_agent_stage
      if c_id then
        items[#items+1] = {
          text = {
            { text = "-> " },
            { text = string.format("%s (%s)", c_id, stages[c_stage] or "..."),
              style = { fg = "cyan" } },
          },
        }
      end

    elseif entry.type == "workspace_header" then
      -- Workspace group header: arrow + title + status + count
      local icon    = WORKSPACE_STATUS_ICON[entry.status] or WORKSPACE_STATUS_ICON.idle
      local arrow   = entry.collapsed and "▶ " or "▼ "
      local count   = string.format("  %d session%s",
        entry.agent_count, entry.agent_count == 1 and "" or "s")
      items[#items+1] = {
        text = {
          { text = arrow, style = "dim" },
          { text = entry.title },
        },
        secondary = {
          icon,
          { text = count, style = "dim" },
        },
      }

    elseif entry.type == "agent" then
      -- Agent row: indented under workspace header
      local agent = agents_by_id[entry.agent_id]
      if agent then
        local name         = agent.display_name or agent.branch_name or entry.agent_id
        local notification = agent.notification
        local text
        if notification then
          text = {
            { text = "  " },
            { text = "● ", style = { fg = "yellow" } },
            { text = name },
          }
        else
          text = {
            { text = "  " },
            { text = name },
          }
        end

        local item = { text = text }
        -- Secondary: profile · branch (when branch differs from display name)
        local parts = {}
        if agent.profile_name then parts[#parts+1] = agent.profile_name end
        if agent.branch_name and agent.branch_name ~= name then
          parts[#parts+1] = agent.branch_name
        end
        if #parts > 0 then
          item.secondary = { { text = "  " .. table.concat(parts, " · "), style = "dim" } }
        end
        items[#items+1] = item
      else
        -- Agent not in cache yet; show placeholder
        items[#items+1] = {
          text = { { text = "  " }, { text = entry.agent_id, style = "dim" } },
        }
      end
    end
  end

  return items
end

-- =============================================================================
-- Helper: Build agent list items from state (legacy / fallback)
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
    local parts = {}
    if agent.profile_name then table.insert(parts, agent.profile_name) end
    if agent.branch_name then table.insert(parts, agent.branch_name) end
    local secondary = #parts > 0 and table.concat(parts, " · ") or nil
    local item
    if agent.notification then
      item = { text = {
        { text = "● ", style = { fg = "yellow" } },
        { text = name },
      } }
    else
      item = { text = name }
    end
    if secondary then
      item.secondary = { { text = secondary, style = "dim" } }
    end
    table.insert(items, item)
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
    table.insert(items, { text = "Add Session", action = "add_session" })
    -- Only show Remove Session when there are sessions beyond the primary (index 0)
    if #sessions > 1 then
      table.insert(items, { text = "Remove Session", action = "remove_session" })
    end
    table.insert(items, { text = "Close Agent", action = "close_agent" })
  end

  -- Hub section (always shown)
  table.insert(items, { text = "── Hub ──", header = true })
  table.insert(items, { text = "New Agent", action = "new_agent" })
  table.insert(items, { text = "Show Connection Code", action = "show_connection_code" })
  table.insert(items, { text = "Restart Hub", action = "restart_hub" })
  table.insert(items, { text = "Dev Rebuild & Restart", action = "dev_rebuild" })

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

--- Main layout: workspace/agent list + terminal panel.
function render(state)
  local agents = _tui_state and _tui_state.agents or {}
  local agent_count = #agents
  local creating = get_creating_agent()
  local sa = get_selected_agent()

  -- List title: session count + poll indicator
  local poll_icon = state.seconds_since_poll < 1 and "*" or "o"
  local list_title = {
    { text = string.format(" Sessions (%d) ", agent_count) },
    { text = poll_icon .. " ", style = { fg = "cyan" } },
  }

  -- Determine list items and cursor position.
  -- Phase 3: use workspace-grouped flat_list when available.
  -- Fallback: legacy flat agent list (before workspace events arrive).
  local list_items
  local list_cursor
  if _tui_state and _tui_state.flat_list then
    list_items  = build_list_items() or {}
    list_cursor = _tui_state.list_cursor_pos
  else
    list_items = build_agent_items(state)
    -- Legacy cursor: selected_agent_index offset by creating indicator
    list_cursor = _tui_state and _tui_state.selected_agent_index
    if list_cursor and creating then
      list_cursor = list_cursor + 1
    end
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
    term_title = {
      { text = string.format(" %s %s%s ", sa.branch_name or "main", view, scroll) },
    }
  end

  -- Build terminal panel: always show selected agent only
  local terminal_props = {}
  if _tui_state and _tui_state.selected_agent_index then
    terminal_props.agent_index = _tui_state.selected_agent_index
    terminal_props.pty_index = _tui_state.active_pty_index or 0
  end
  local terminal_panel = {
    type = "terminal",
    props = terminal_props,
    block = { title = term_title, borders = "all" },
  }

  return {
    type = "hsplit",
    constraints = { "15%", "85%" },
    children = {
      {
        type = "list",
        block = { title = list_title, borders = "all" },
        props = {
          items = list_items,
          selected = list_cursor,
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
  elseif _tui_state.mode == "new_agent_select_profile" then
    local profile_items = {}
    for _, p in ipairs(_tui_state.available_profiles or {}) do
      table.insert(profile_items, { text = p })
    end
    if #profile_items == 0 then
      profile_items = { { text = "Loading profiles...", style = "dim" } }
    end
    return {
      type = "centered", width = 50, height = 30,
      child = {
        type = "list",
        id = "profile_list",
        block = { title = " Select Profile [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = profile_items,
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
    local in_worktree = sa and sa.in_worktree
    local lines
    if not in_worktree then
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
  elseif _tui_state.mode == "add_session_select_type" then
    local type_items = {}
    for _, t in ipairs(_tui_state.available_session_types or {}) do
      local label = t.label or t.name
      table.insert(type_items, { text = label, secondary = { { text = t.description or "", style = "dim" } } })
    end
    if #type_items == 0 then
      type_items = { { text = "Loading...", style = "dim" } }
    end
    return {
      type = "centered", width = 50, height = 30,
      child = {
        type = "list",
        id = "session_type_list",
        block = { title = " Add Session [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = type_items,
        },
      },
    }
  elseif _tui_state.mode == "remove_session_select" then
    local sa = get_selected_agent()
    local session_items = {}
    if sa and sa.sessions then
      for idx, session in ipairs(sa.sessions) do
        -- Skip index 0 (primary agent session)
        if idx > 1 then
          local name = type(session) == "table" and session.name or session
          local label = string.upper(name)
          table.insert(session_items, { text = label })
        end
      end
    end
    if #session_items == 0 then
      session_items = { { text = "No removable sessions", style = "dim" } }
    end
    return {
      type = "centered", width = 50, height = 30,
      child = {
        type = "list",
        id = "remove_session_list",
        block = { title = " Remove Session [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = session_items,
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
