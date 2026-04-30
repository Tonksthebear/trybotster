-- Model-to-wire publication helpers.
--
-- `lib.entity_broadcast` owns the envelope protocol. This module owns the
-- model contract: when a domain model mutates, publish the client-facing
-- entity shape from one place instead of sprinkling EB calls across handlers.

local M = {}

function M.publish_session(session)
    if not session then return end

    local Session = require("lib.session")
    local EB = require("lib.entity_broadcast")
    if not EB.is_registered("session") or Session.is_system_session(session) then
        return
    end

    local Agent = require("lib.agent")
    local ClientSessionPayload = require("lib.client_session_payload")
    EB.upsert("session", ClientSessionPayload.build(session, Agent.all_info()))
end

function M.patch_session(session, changed_fields)
    if not session or type(changed_fields) ~= "table" then return end

    local Session = require("lib.session")
    local EB = require("lib.entity_broadcast")
    if not EB.is_registered("session") or Session.is_system_session(session) then
        return
    end

    local ClientSessionPayload = require("lib.client_session_payload")
    local patch = ClientSessionPayload.project_fields(changed_fields, session)
    if next(patch) ~= nil then
        EB.patch("session", session.session_uuid, patch)
    end
end

function M.remove_session(session_id)
    local EB = require("lib.entity_broadcast")
    if session_id and EB.is_registered("session") then
        EB.remove("session", session_id)
    end
end

function M.upsert_session_action(action)
    local EB = require("lib.entity_broadcast")
    if type(action) == "table" and action.id and EB.is_registered("session_action") then
        EB.upsert("session_action", action)
    end
end

function M.remove_session_action(session_uuid, action_id)
    local EB = require("lib.entity_broadcast")
    if session_uuid and action_id and EB.is_registered("session_action") then
        local SessionActions = require("lib.session_actions")
        EB.remove("session_action", SessionActions.entity_id(session_uuid, action_id))
    end
end

function M.upsert_spawn_target(target)
    local EB = require("lib.entity_broadcast")
    if type(target) == "table" and EB.is_registered("spawn_target") then
        EB.upsert("spawn_target", target)
    end
end

function M.patch_spawn_target(target_id, fields)
    local EB = require("lib.entity_broadcast")
    if target_id and type(fields) == "table" and next(fields) ~= nil and EB.is_registered("spawn_target") then
        EB.patch("spawn_target", target_id, fields)
    end
end

function M.remove_spawn_target(target_id)
    local EB = require("lib.entity_broadcast")
    if target_id and EB.is_registered("spawn_target") then
        EB.remove("spawn_target", target_id)
    end
end

function M.upsert_workspace(workspace)
    local EB = require("lib.entity_broadcast")
    if type(workspace) == "table" and workspace.workspace_id and EB.is_registered("workspace") then
        EB.upsert("workspace", workspace)
    end
end

function M.upsert_session_workspace(session)
    if type(session) ~= "table" then return end

    local workspace_id = session.workspace_id or session._workspace_id
    if not workspace_id or workspace_id == "" then return end

    local session_type = session.session_type or "agent"
    local counts = { agent = 0, accessory = 0, other = 0 }
    if session_type == "agent" then
        counts.agent = 1
    elseif session_type == "accessory" then
        counts.accessory = 1
    else
        counts.other = 1
    end

    M.upsert_workspace({
        id = workspace_id,
        workspace_id = workspace_id,
        name = session.workspace_name or session._workspace_name or workspace_id,
        status = "active",
        agents = { session.session_uuid or session.id },
        session_counts = counts,
    })
end

function M.patch_workspace(workspace_id, fields)
    local EB = require("lib.entity_broadcast")
    if workspace_id and type(fields) == "table" and next(fields) ~= nil and EB.is_registered("workspace") then
        EB.patch("workspace", workspace_id, fields)
    end
end

function M.upsert_worktree(worktree)
    local EB = require("lib.entity_broadcast")
    if type(worktree) == "table" and EB.is_registered("worktree") then
        EB.upsert("worktree", worktree)
    end
end

function M.remove_worktree(worktree_path)
    local EB = require("lib.entity_broadcast")
    if worktree_path and EB.is_registered("worktree") then
        EB.remove("worktree", worktree_path)
    end
end

function M.upsert_hub(payload)
    local EB = require("lib.entity_broadcast")
    if type(payload) == "table" and payload.hub_id and EB.is_registered("hub") then
        EB.upsert("hub", payload)
    end
end

function M.upsert_connection_code(payload)
    local EB = require("lib.entity_broadcast")
    if type(payload) == "table" and payload.hub_id and EB.is_registered("connection_code") then
        EB.upsert("connection_code", payload)
    end
end

function M.upsert_template(template)
    local EB = require("lib.entity_broadcast")
    if type(template) == "table" and template.id and EB.is_registered("template") then
        EB.upsert("template", template)
    end
end

function M.remove_template(template_id)
    local EB = require("lib.entity_broadcast")
    if template_id and EB.is_registered("template") then
        EB.remove("template", template_id)
    end
end

return M
