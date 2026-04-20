-- Builder for `AgentWorkspaceSurfaceInputV1`-shaped state consumed by
-- `web_layout.render(...)`.
--
-- Selection is per-browser (a user selecting session S on client A must
-- not flip the selected row on client B), so the canonical builder is
-- `build_for_subscription(client, sub_id)` — it reads the selection the
-- hub has recorded for that client. `build_global()` is provided for
-- tests or scenarios that don't care about selection, but production
-- broadcasts always use the per-subscription path.
--
-- The `surface` field is intentionally omitted here and filled in by
-- `layout_broadcast` when it renders per-density; callers should never set
-- it themselves.

local Agent = require("lib.agent")
local AgentListPayload = require("lib.agent_list_payload")

local M = {}

--- Base state: agents + open workspaces + hub id, no selection. Used as
--- the starting point for both the per-subscription and (test-only)
--- selection-agnostic builders so the two paths never drift.
local function build_base()
    local payload = AgentListPayload.build(Agent.all_info())
    local hub_id = hub.server_id and hub.server_id() or nil
    return {
        hub_id = hub_id,
        agents = payload.agents,
        open_workspaces = payload.workspaces,
        selected_session_uuid = nil,
    }
end

--- Build the input state for all subscribers on this hub WITHOUT selection.
--- Selection is per-client so this function is only useful for tests or
--- diagnostic flows; production code should prefer `build_for_subscription`.
-- @return table AgentWorkspaceSurfaceInputV1 without `surface`
function M.build_global()
    return build_base()
end

--- Build the input state for a single subscription. Threads the client's
--- recorded selection (set by the `select_agent` command handler and the
--- `botster.session.select` action) into `selected_session_uuid` so the
--- rendered tree applies `tree_item.selected` to the right row for THAT
--- browser. Different subscribers produce different trees — by design.
-- @param client Client instance (reads `client.selected_session_uuid`)
-- @param _sub_id string (reserved for future per-subscription state)
-- @return table AgentWorkspaceSurfaceInputV1 without `surface`
function M.build_for_subscription(client, _sub_id)
    local base = build_base()
    if client and client.selected_session_uuid then
        base.selected_session_uuid = client.selected_session_uuid
    end
    return base
end

return M
