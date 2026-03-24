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
  local uuid = _tui_state and _tui_state.selected_session_uuid
  if not uuid then return nil end
  for _, a in ipairs(_tui_state and _tui_state.agents or {}) do
    if a.session_uuid == uuid then return a end
  end
  return nil
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
          { text = entry.name or entry.workspace_id },
        },
        secondary = {
          { text = count, style = "dim" },
        },
      }

    elseif entry.type == "agent" then
      -- Agent row: indented under workspace header
      local agent = agents_by_id[entry.agent_id]
      if agent then
        -- Label is primary display name when present
        local has_label    = agent.label and agent.label:match("%S")
        local name         = has_label and agent.label
                             or (agent.display_name or agent.branch_name or entry.agent_id)
        local notification = agent.notification

        -- Activity indicator for agent sessions
        local is_active = agent.session_type == "agent" and not agent.is_idle

        local text
        if notification then
          text = {
            { text = "  " },
            { text = "● ", style = { fg = "yellow" } },
            { text = name },
          }
        elseif is_active then
          text = {
            { text = "  " },
            { text = "✦ ", style = { fg = "green" } },
            { text = name },
          }
        else
          text = {
            { text = "  " },
            { text = name },
          }
        end

        local item = { text = text }
        -- Secondary: spawn target name · branch · config name, plus task
        local parts = {}
        if agent.target_name then parts[#parts+1] = agent.target_name end
        if agent.branch_name then parts[#parts+1] = agent.branch_name end
        local config_name = agent.agent_name or agent.profile_name
        if config_name then parts[#parts+1] = config_name end
        if agent.task and agent.task ~= "" then parts[#parts+1] = agent.task end
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
    -- Label is primary display name when present
    local has_label = agent.label and agent.label:match("%S")
    local name = has_label and agent.label
                 or (agent.display_name or agent.branch_name)
    -- Activity indicator for agent sessions
    local is_active = agent.session_type == "agent" and not agent.is_idle

    local item
    if agent.notification then
      item = { text = {
        { text = "● ", style = { fg = "yellow" } },
        { text = name },
      } }
    elseif is_active then
      item = { text = {
        { text = "✦ ", style = { fg = "green" } },
        { text = name },
      } }
    else
      item = { text = name }
    end
    -- Secondary: spawn target name · branch · config name
    local secondary_parts = {}
    if agent.target_name then secondary_parts[#secondary_parts+1] = agent.target_name end
    if agent.branch_name then secondary_parts[#secondary_parts+1] = agent.branch_name end
    local config_name = agent.agent_name or agent.profile_name
    if config_name then secondary_parts[#secondary_parts+1] = config_name end
    if #secondary_parts > 0 then
      item.secondary = { { text = table.concat(secondary_parts, " · "), style = "dim" } }
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
    table.insert(items, { text = "Close Agent", action = "close_agent" })
  end

  -- Hub section (always shown)
  table.insert(items, { text = "── Hub ──", header = true })
  table.insert(items, { text = "New Agent", action = "new_agent" })
  table.insert(items, { text = "New Accessory", action = "new_accessory" })
  table.insert(items, { text = "Spawn Targets", action = "spawn_targets_info" })
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
    -- Legacy cursor: find selected agent's position in list, offset by creating indicator
    local uuid = _tui_state and _tui_state.selected_session_uuid
    if uuid then
      for i, a in ipairs(_tui_state and _tui_state.agents or {}) do
        if a.session_uuid == uuid then
          list_cursor = i - 1  -- 0-based
          break
        end
      end
      if list_cursor and creating then
        list_cursor = list_cursor + 1
      end
    end
  end

  -- Terminal title: branch name, session type, scroll indicator
  local term_title = " Terminal [No agent selected] "
  if sa then
    local session_label = string.upper(sa.session_type or sa.session_name or "agent")
    -- Show forwarded port if applicable
    if sa.port then
      session_label = session_label .. " :" .. sa.port
    end
    local scroll = ""
    if state.is_scrolled then
      scroll = string.format(" [SCROLLBACK +%d | Shift+End: live]", state.scroll_offset)
    end
    local term_name = (sa.label and sa.label ~= "") and sa.label or (sa.branch_name or "main")
    term_title = {
      { text = string.format(" %s [%s]%s ", term_name, session_label, scroll) },
    }
  end

  -- Build terminal panel: always show selected agent only (single PTY per agent)
  local terminal_props = {}
  if sa and sa.session_uuid then
    terminal_props.session_uuid = sa.session_uuid
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
  elseif _tui_state.mode == "new_agent_select_target" then
    local target_items = {}
    for _, target in ipairs(_tui_state.available_targets or {}) do
      local branch = target.current_branch and string.format(" (%s)", target.current_branch) or ""
      table.insert(target_items, {
        text = {
          { text = target.name or target.path or target.id or "target" },
          { text = branch, style = "dim" },
        },
      })
    end
    if #target_items == 0 then
      target_items = { { text = "No admitted spawn targets", style = "dim" } }
    end
    return {
      type = "centered", width = 64, height = 30,
      child = {
        type = "list",
        id = "spawn_target_list",
        block = { title = " Select Spawn Target [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = target_items,
        },
      },
    }
  elseif _tui_state.mode == "spawn_target_path_input" then
    return {
      type = "centered", width = 68, height = 24,
      child = {
        type = "input",
        id = "spawn_target_path_input",
        block = { title = " Admit Spawn Target [Enter confirm | Esc cancel] ", borders = "all" },
        props = {
          lines = {
            "Enter an absolute path to admit as a spawn target:",
            "",
            "Examples:",
            "/Users/exampleuser/Rails/trybotster",
            "/Users/exampleuser/projects/scratch",
          },
          placeholder = "/absolute/path",
        },
      },
    }
  elseif _tui_state.mode == "new_agent_select_agent" then
    local agent_items = {}
    for _, a in ipairs(_tui_state.available_agents or {}) do
      table.insert(agent_items, { text = a })
    end
    if #agent_items == 0 then
      agent_items = { { text = "Loading agent configs...", style = "dim" } }
    end
    return {
      type = "centered", width = 50, height = 30,
      child = {
        type = "list",
        id = "agent_config_list",
        block = { title = " Select Agent [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = agent_items,
        },
      },
    }
  elseif _tui_state.mode == "new_accessory_select_target" then
    local target_items = {}
    for _, target in ipairs(_tui_state.available_targets or {}) do
      local branch = target.current_branch and string.format(" (%s)", target.current_branch) or ""
      table.insert(target_items, {
        text = {
          { text = target.name or target.path or target.id or "target" },
          { text = branch, style = "dim" },
        },
      })
    end
    if #target_items == 0 then
      target_items = { { text = "No admitted spawn targets", style = "dim" } }
    end
    return {
      type = "centered", width = 64, height = 30,
      child = {
        type = "list",
        id = "accessory_spawn_target_list",
        block = { title = " Select Spawn Target [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = target_items,
        },
      },
    }
  elseif _tui_state.mode == "new_accessory_select" then
    local acc_items = {}
    for _, a in ipairs(_tui_state.available_accessories or {}) do
      table.insert(acc_items, { text = a })
    end
    if #acc_items == 0 then
      acc_items = { { text = "Loading accessories...", style = "dim" } }
    end
    return {
      type = "centered", width = 50, height = 30,
      child = {
        type = "list",
        id = "accessory_config_list",
        block = { title = " Select Accessory [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = acc_items,
        },
      },
    }
  elseif _tui_state.mode == "new_accessory_select_workspace" then
    local ws_items = {
      { text = "[No Workspace]" },
    }
    for _, ws in ipairs(_tui_state.available_workspaces or {}) do
      local count_label = string.format(" (%d session%s)",
        ws.agent_count, ws.agent_count == 1 and "" or "s")
      ws_items[#ws_items + 1] = {
        text = {
          { text = ws.name or ws.id },
          { text = count_label, style = "dim" },
        },
      }
    end
    return {
      type = "centered", width = 55, height = 30,
      child = {
        type = "list",
        id = "accessory_workspace_list",
        block = { title = " Select Workspace [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = ws_items,
        },
      },
    }
  elseif _tui_state.mode == "new_agent_select_workspace" then
    local ws_items = {
      { text = "[Create New Workspace]" },
    }
    for _, ws in ipairs(_tui_state.available_workspaces or {}) do
      local count_label = string.format(" (%d session%s)",
        ws.agent_count, ws.agent_count == 1 and "" or "s")
      ws_items[#ws_items + 1] = {
        text = {
          { text = ws.name or ws.id },
          { text = count_label, style = "dim" },
        },
      }
    end
    return {
      type = "centered", width = 55, height = 30,
      child = {
        type = "list",
        id = "workspace_list",
        block = { title = " Select Workspace [Up/Down navigate | Enter select | Esc cancel] ", borders = "all" },
        props = {
          items = ws_items,
        },
      },
    }
  elseif _tui_state.mode == "new_workspace_name_input" then
    return {
      type = "centered", width = 55, height = 20,
      child = {
        type = "input",
        id = "workspace_name_input",
        block = { title = " New Workspace [Enter confirm | Esc cancel] ", borders = "all" },
        props = {
          lines = {
            "Enter a name for the new workspace:",
            "",
            "Leave empty for auto-generated name.",
          },
          placeholder = "e.g. auth-feature, bug-fixes",
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
  elseif _tui_state.mode == "rename_workspace_input" then
    local current_name = _tui_state.pending_fields and _tui_state.pending_fields.workspace_name or ""
    return {
      type = "centered", width = 62, height = 24,
      child = {
        type = "input",
        id = "rename_workspace_input",
        block = { title = " Rename Workspace [Enter confirm | Esc cancel] ", borders = "all" },
        props = {
          lines = {
            "Rename selected workspace:",
            "",
            "Current: " .. (current_name ~= "" and current_name or "(unnamed)"),
          },
          placeholder = "Enter new workspace name",
        },
      },
    }
  elseif _tui_state.mode == "move_workspace_input" then
    return {
      type = "centered", width = 68, height = 26,
      child = {
        type = "input",
        id = "move_workspace_input",
        block = { title = " Move Session Workspace [Enter confirm | Esc cancel] ", borders = "all" },
        props = {
          lines = {
            "Move selected session to workspace:",
            "",
            "Type an existing workspace name/id, or a new workspace name.",
          },
          placeholder = "Workspace name or workspace id",
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
  elseif _tui_state.mode == "spawn_targets_info" then
    local target_items = {}
    for _, target in ipairs(_tui_state.available_targets or {}) do
      local branch = target.current_branch and (" [" .. target.current_branch .. "]") or ""
      local status = target.enabled == false and " disabled" or ""
      table.insert(target_items, {
        text = {
          { text = (target.name or target.path or target.id or "target") .. branch .. status },
          { text = target.path or "", style = "dim" },
        },
      })
    end
    if #target_items == 0 then
      target_items = {
        { text = "No admitted spawn targets", style = "dim" },
      }
    end
    return {
      type = "centered", width = 76, height = 28,
      child = {
        type = "list",
        id = "spawn_target_manage_list",
        block = { title = " Spawn Targets [Up/Down navigate | a add | d remove | n rename | r refresh | Esc close] ", borders = "all" },
        props = {
          items = target_items,
        },
      },
    }
  elseif _tui_state.mode == "rename_spawn_target_input" then
    local current_name = _tui_state.pending_fields and _tui_state.pending_fields.rename_target_id or ""
    return {
      type = "centered", width = 62, height = 24,
      child = {
        type = "input",
        id = "rename_spawn_target_input",
        block = { title = " Rename Spawn Target [Enter confirm | Esc cancel] ", borders = "all" },
        props = {
          lines = {
            "Enter a new name for the spawn target:",
          },
          placeholder = "Enter new name",
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
  elseif _tui_state.mode == "restarting" then
    return {
      type = "centered", width = 42, height = 20,
      child = {
        type = "paragraph",
        block = { title = " Hub Restart ", borders = "all" },
        props = {
          lines = {
            "",
            { { text = "◌ Rebooting...", style = { fg = "yellow", bold = true } } },
            "",
            { { text = "Waiting for hub to reconnect", style = "dim" } },
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
