-- Embedded default web layout.
--
-- Entry point for `web_layout.render(surface, state)`. Produces a `UiNodeV1`
-- tree that mirrors the Phase-1 React composites (`WorkspaceList`,
-- `WorkspaceGroup`, `SessionRow`, `HostedPreviewIndicator`,
-- `HostedPreviewError`) so browsers can render the hub-composed tree today
-- without regressions when Phase 2b wires transport.
--
-- SessionActionsMenu: Menu / MenuItem are not Lua-public in v1 (see
-- `docs/specs/web-ui-primitives-runtime.md:179`). Phase 2a emits an
-- `ui.icon_button` placeholder with a `botster.session.menu.open` action;
-- Phase 2c decides how browsers realise the menu (promote Menu to v1, OR
-- render a Catalyst dropdown composite that listens for that action).
--
-- Density variants:
--   sidebar — xs text, xs icons, no workspace count, tighter gaps
--   panel   — sm text, sm icons, workspace count shown, standard gaps

local vm = require("web.workspace_view_model")

local M = {}

-- -------------------------------------------------------------------------
-- Small helpers
-- -------------------------------------------------------------------------

-- status_dot parity with composites.ts:activityDotForState.
local function activity_dot(state)
  if state == "idle" then
    return { ui.status_dot{ state = "idle", label = "Idle" } }
  elseif state == "active" then
    return { ui.status_dot{ state = "active", label = "Active" } }
  end
  -- "accessory" and "hidden" suppress the dot
  return nil
end

-- hosted_preview_indicator parity with composites.ts:hostedPreviewIndicator.
-- Returns nil, a button (running + url), or a badge.
local function hosted_preview_node(session, density)
  local status = vm.hosted_preview_status(session)
  if status == "inactive" or status == "unavailable" then
    return nil
  end

  local preview = vm.preview_state(session)

  if status == "running" and preview.url then
    return ui.button{
      label = "Running",
      variant = "ghost",
      tone = "default",
      icon = "external-link",
      action = ui.action("botster.session.preview.open", {
        sessionId = session.id,
        sessionUuid = session.session_uuid,
        url = preview.url,
      }),
    }
  end

  local label_map = {
    inactive = "Preview",
    starting = "Starting\u{2026}",
    running = "Running",
    error = "Error",
    unavailable = "Unavailable",
  }
  local tone_map = {
    inactive = "default",
    starting = "warning",
    running = "success",
    error = "danger",
    unavailable = "default",
  }
  return ui.badge{
    text = label_map[status] or "Preview",
    tone = tone_map[status] or "default",
    size = density == "sidebar" and "sm" or "md",
  }
end

-- actions_menu_trigger — Phase 2c handoff placeholder. See module header.
local function actions_menu_trigger(session)
  return ui.icon_button{
    icon = "ellipsis-vertical",
    label = "Session actions",
    action = ui.action("botster.session.menu.open", {
      sessionId = session.id,
      sessionUuid = session.session_uuid,
    }),
  }
end

-- new_session_button — always rendered in the workspace surface, matching
-- WorkspaceList.jsx:45-49 (populated) and the NewSessionButton inside the
-- EmptyState (:101-107). The action id is semantic — browsers realise it by
-- opening the `new-session-chooser-modal` Rails dialog; the Lua tree only
-- emits the intent.
local function new_session_button()
  return ui.button{
    label = "New session",
    icon = "plus",
    variant = "ghost",
    tone = "default",
    action = ui.action("botster.session.create.request"),
  }
end

-- empty_state_tree — manual Stack + Icon + Text(+Text) + Button composition
-- matching `WorkspaceList.jsx:59-111`. Intentionally NOT `ui.empty_state{}`:
-- v1 `EmptyStatePropsV1.primaryAction` carries no label, and the current
-- browser surface needs a labeled "New session" button. This is the same
-- tradeoff documented in the JSX comment on `EmptyState`.
local function empty_state_tree(density)
  local is_sidebar = density == "sidebar"
  local children = {
    ui.icon{ name = "sparkle", size = "md", tone = "muted" },
    ui.text{
      text = "No sessions running",
      size = is_sidebar and "sm" or "md",
      weight = "medium",
      tone = "muted",
    },
  }
  if not is_sidebar then
    children[#children + 1] = ui.text{
      text = "Start a new agent or accessory to begin working",
      size = "sm",
      tone = "muted",
    }
  end
  children[#children + 1] = new_session_button()

  return ui.stack{
    direction = "vertical",
    gap = "3",
    align = "center",
    children = children,
  }
end

-- hosted_preview_error_panel — returns nil unless the session has a preview
-- error. The inner content matches composites.ts:hostedPreviewErrorInner; the
-- outer container is a muted panel (v1 Panel tone is default|muted only, so
-- the danger accent lives on the inner text + icon children).
local function hosted_preview_error_panel(session, density)
  local preview = vm.preview_state(session)
  if preview.status ~= "error" or not preview.error then return nil end

  local icon_size = density == "sidebar" and "xs" or "sm"
  local rows = {
    ui.inline{
      gap = "2",
      align = "start",
      children = {
        ui.icon{ name = "exclamation-triangle", size = icon_size, tone = "danger" },
        ui.text{ text = preview.error, tone = "danger", size = icon_size },
      },
    },
  }

  if preview.install_url then
    rows[#rows + 1] = ui.button{
      label = "Install cloudflared",
      variant = "ghost",
      tone = "danger",
      action = ui.action("botster.session.preview.open", {
        sessionUuid = session.session_uuid,
        url = preview.install_url,
      }),
    }
  end

  return ui.panel{
    tone = "muted",
    border = true,
    children = {
      ui.stack{
        direction = "vertical",
        gap = density == "sidebar" and "1" or "2",
        align = "start",
        children = rows,
      },
    },
  }
end

-- session_tree_item — parity with composites.ts:sessionRowTreeItem, extended
-- with the actions-menu placeholder in the end slot.
local function session_tree_item(session, vm_state)
  local density = vm_state.density
  local is_accessory = session.session_type == "accessory"
  local title_size = density == "sidebar" and "xs" or "sm"
  local activity = vm.activity_state(session)
  local primary_name = vm.display_name(session)
  local title_line = vm.title_line(session)
  local subtext = vm.subtext(session)
  local selected = vm_state.selected_session_uuid == session.session_uuid

  local title_slot = {
    ui.text{
      text = primary_name,
      size = title_size,
      monospace = true,
      truncate = true,
      tone = is_accessory and "muted" or "default",
      weight = selected and "medium" or "regular",
    },
  }

  local subtitle_slot = nil
  if title_line ~= "" or subtext ~= "" then
    subtitle_slot = {}
    if title_line ~= "" then
      subtitle_slot[#subtitle_slot + 1] = ui.text{
        text = title_line,
        size = "xs",
        tone = "muted",
        italic = true,
        truncate = true,
      }
    end
    if subtext ~= "" then
      subtitle_slot[#subtitle_slot + 1] = ui.text{
        text = subtext,
        size = "xs",
        tone = "muted",
        truncate = true,
      }
    end
  end

  local start_slot = activity_dot(activity)

  local end_children = {}
  local preview = hosted_preview_node(session, density)
  if preview then end_children[#end_children + 1] = preview end
  end_children[#end_children + 1] = actions_menu_trigger(session)
  local end_slot = {
    ui.inline{ gap = "1", align = "center", children = end_children },
  }

  local item_args = {
    id = session.id,
    selected = selected,
    notification = session.notification == true,
    action = ui.action("botster.session.select", {
      sessionId = session.id,
      sessionUuid = session.session_uuid,
    }),
    title = title_slot,
  }
  if subtitle_slot then item_args.subtitle = subtitle_slot end
  if start_slot then item_args.start = start_slot end
  item_args.end_ = end_slot

  return ui.tree_item(item_args)
end

-- workspace_header_title — parity with composites.ts:workspaceHeaderContent.
local function workspace_header_title(group, density)
  local children = {
    ui.icon{
      name = "chevron-down",
      size = density == "sidebar" and "xs" or "sm",
      tone = "muted",
    },
    ui.text{
      text = group.title,
      size = "xs",
      tone = "muted",
      weight = "medium",
      truncate = true,
    },
  }
  if density ~= "sidebar" then
    children[#children + 1] = ui.text{
      text = tostring(group.count),
      size = "xs",
      tone = "muted",
    }
  end
  return { ui.inline{ gap = "2", align = "center", children = children } }
end

-- session_children — returns an array of UiNodeV1 nodes for the sessions in
-- one workspace group (or the ungrouped bucket): one tree_item per session,
-- plus an error panel when the session reports a preview error.
local function session_children(sessions, vm_state)
  local out = {}
  for _, session in ipairs(sessions) do
    out[#out + 1] = session_tree_item(session, vm_state)
    local err = hosted_preview_error_panel(session, vm_state.density)
    if err then out[#out + 1] = err end
  end
  return out
end

-- -------------------------------------------------------------------------
-- Surface entry points
-- -------------------------------------------------------------------------

function M.workspace_surface(state)
  local vm_state = vm.build(state)

  if vm_state.empty then
    return empty_state_tree(vm_state.density)
  end

  local tree_children = {}
  for _, group in ipairs(vm_state.groups) do
    -- `children` is a slot on tree_item (nested tree items); the DSL's
    -- top-level hoist for `children` also populates positional children,
    -- which would duplicate the subtree. Pass it explicitly via slots to
    -- keep the wire format clean.
    tree_children[#tree_children + 1] = ui.tree_item{
      id = group.workspace.id,
      expanded = true,
      action = ui.action("botster.workspace.toggle", {
        workspaceId = group.workspace.id,
      }),
      title = workspace_header_title(group, vm_state.density),
      slots = { children = session_children(group.sessions, vm_state) },
    }
  end

  for _, node in ipairs(session_children(vm_state.ungrouped, vm_state)) do
    tree_children[#tree_children + 1] = node
  end

  -- WorkspaceList.jsx wraps the groups/rows in a flat div and appends the
  -- NewSession button as a sibling. We mirror that with a vertical stack
  -- containing the tree and the button.
  return ui.stack{
    direction = "vertical",
    gap = vm_state.density == "sidebar" and "0" or "2",
    children = {
      ui.tree{ children = tree_children },
      new_session_button(),
    },
  }
end

return M
