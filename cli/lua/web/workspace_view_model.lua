-- Pure view-model helpers for the workspace/session surface.
--
-- Ported from `app/frontend/store/workspace-store.js` (selectors) and
-- `app/frontend/ui_contract/composites.ts` (internal derivations) so the hub
-- produces the same display strings, activity state, and preview state that
-- the Phase-1 React composites produce today.
--
-- Inputs follow the `AgentWorkspaceSurfaceInput` shape defined in
-- `docs/specs/web-ui-primitives-runtime.md`. Outputs are plain Lua tables
-- consumed by `web/layout.lua`.
--
-- No side effects. No hub calls. Pure in, pure out — the reason this file is
-- testable in isolation.

local M = {}

-- -------------------------------------------------------------------------
-- Primitive helpers
-- -------------------------------------------------------------------------

local MIDDLE_DOT = " \u{00b7} "

local function trim(s)
  if type(s) ~= "string" then return nil end
  local trimmed = s:match("^%s*(.-)%s*$")
  if trimmed == "" then return nil end
  return trimmed
end

-- -------------------------------------------------------------------------
-- Session selectors (port of workspace-store.js:71-117)
-- -------------------------------------------------------------------------

--- Derive the primary display name for a session.
--- Mirrors `displayName(session)`: label > display_name > id.
function M.display_name(session)
  if not session then return "" end
  local label = trim(session.label)
  if label then return label end
  if session.display_name and session.display_name ~= "" then
    return session.display_name
  end
  return session.id or ""
end

--- Build the subtext line: target · branch · agent/profile. For accessories
--- with no configured metadata, falls back to the literal "accessory" so the
--- row reads as something rather than being empty.
function M.subtext(session)
  if not session then return "" end
  local parts = {}
  if session.target_name and session.target_name ~= "" then
    parts[#parts + 1] = session.target_name
  end
  if session.branch_name and session.branch_name ~= "" then
    parts[#parts + 1] = session.branch_name
  end
  local config_name = session.agent_name or session.profile_name
  if config_name and config_name ~= "" then
    parts[#parts + 1] = config_name
  end
  if session.session_type == "accessory" and #parts == 0 then
    parts[#parts + 1] = "accessory"
  end
  return table.concat(parts, MIDDLE_DOT)
end

--- Build the title line: title · task, dropping title if it equals the
--- primary display name (de-duplication mirrors workspace-store.js).
function M.title_line(session)
  if not session then return "" end
  local parts = {}
  local title = trim(session.title)
  local primary = M.display_name(session)
  if title and title ~= primary then
    parts[#parts + 1] = title
  end
  if session.task and session.task ~= "" then
    parts[#parts + 1] = session.task
  end
  return table.concat(parts, MIDDLE_DOT)
end

--- Derive activity state: "accessory" for accessory sessions, otherwise
--- "idle" unless the session explicitly reports `is_idle = false`.
function M.activity_state(session)
  if not session then return "idle" end
  if session.session_type == "accessory" then return "accessory" end
  if session.is_idle == false then return "active" end
  return "idle"
end

--- Preview state bundle: `{ can_preview, status, url, error, install_url }`.
--- `can_preview` is true only when the session has a forwarded port — without
--- one, there's no URL to open and no "preview" affordance to show. Matches
--- the JS `!!session.port` semantics: 0 counts as "no port", not "port 0".
function M.preview_state(session)
  if not session then return { can_preview = false, status = "inactive" } end
  local hp = session.hosted_preview
  local status = (hp and hp.status) or "inactive"
  local url = (hp and type(hp.url) == "string") and hp.url or nil
  local error_msg = (hp and hp.error and hp.error ~= "") and hp.error or nil
  local install_url = (hp and type(hp.install_url) == "string") and hp.install_url or nil
  local port = session.port
  local can_preview = port ~= nil and port ~= false and port ~= 0
  return {
    can_preview = can_preview,
    status = status,
    url = url,
    error = error_msg,
    install_url = install_url,
  }
end

-- -------------------------------------------------------------------------
-- Composite props (port of composites.ts logic without emitting nodes)
-- -------------------------------------------------------------------------

--- Map preview state onto the phase-1 indicator's extended status vocabulary:
--- "inactive" | "starting" | "running" | "error" | "unavailable". When the
--- session has no forwarded port we return "unavailable" so callers can choose
--- to suppress rendering.
function M.hosted_preview_status(session)
  local preview = M.preview_state(session)
  if not preview.can_preview then return "unavailable" end
  return preview.status
end

--- Availability flags for the session actions menu. Phase 2a emits a
--- placeholder trigger (see `web/layout.lua`); Phase 2c decides how the
--- browser renders the actual menu.
function M.session_action_availability(session)
  if not session then
    return { can_move_workspace = false, can_delete = false, in_worktree = true }
  end
  local in_worktree = true
  if session.in_worktree == false then in_worktree = false end
  return {
    can_move_workspace = true,
    can_delete = true,
    in_worktree = in_worktree,
  }
end

--- Determine the density variant for the given surface. Sidebar surfaces use
--- tighter typography and suppress the workspace count; panel surfaces get
--- the richer layout.
function M.density(surface)
  if surface == "sidebar" then return "sidebar" end
  return "panel"
end

-- -------------------------------------------------------------------------
-- Top-level bundler — groups sessions into workspaces for the layout
-- -------------------------------------------------------------------------

--- Build the per-workspace grouping given the flat agents array and the
--- workspace list. Returns `{ groups = {...}, ungrouped = {...}, empty = bool }`
--- where each group carries its workspace + the ordered sessions belonging to
--- it. Sessions not referenced by any workspace's `agents` field end up in
--- `ungrouped`.
---
--- Emptiness mirrors `WorkspaceList.jsx:17` (`sessionCount === 0`) — a hub
--- with zero sessions is "empty" regardless of how many open workspaces
--- exist. Workspace groups whose resolved session bucket is empty are
--- suppressed entirely to match `WorkspaceGroup.jsx:21`.
function M.build(state)
  local sessions = state.agents or {}
  local workspaces = state.open_workspaces or {}
  local density = M.density(state.surface)

  local sessions_by_id = {}
  for _, s in ipairs(sessions) do
    if s and s.id then sessions_by_id[s.id] = s end
  end

  local grouped_ids = {}
  local groups = {}
  for _, ws in ipairs(workspaces) do
    local bucket = {}
    if ws and type(ws.agents) == "table" then
      for _, agent_id in ipairs(ws.agents) do
        local s = sessions_by_id[agent_id]
        if s then
          bucket[#bucket + 1] = s
          grouped_ids[agent_id] = true
        end
      end
    end
    if #bucket > 0 then
      groups[#groups + 1] = {
        workspace = ws,
        sessions = bucket,
        title = (ws.name and ws.name ~= "") and ws.name or ws.id,
        count = #bucket,
      }
    end
  end

  local ungrouped = {}
  for _, s in ipairs(sessions) do
    if s and s.id and not grouped_ids[s.id] then
      ungrouped[#ungrouped + 1] = s
    end
  end

  -- Parity with WorkspaceList.jsx: empty is driven by session count, not by
  -- whether any workspaces happen to be open.
  local empty = #sessions == 0

  return {
    density = density,
    surface = state.surface or "panel",
    selected_session_uuid = state.selected_session_uuid,
    groups = groups,
    ungrouped = ungrouped,
    empty = empty,
  }
end

return M
