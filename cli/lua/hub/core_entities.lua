-- Built-in entity providers for the browser entity protocol.
--
-- lib.entity_broadcast keeps a transient registry so provider callbacks always
-- point at current module code after hot reload. Keep these registrations in a
-- callable module so hub boot and entity_broadcast reload share one path.

local M = {}

local function spawn_target_registry()
    local registry = rawget(_G, "spawn_targets")
    if not registry or type(registry.list) ~= "function" then return nil end
    return registry
end

local function list_spawn_targets(registry, context)
    if context and type(context["spawn_target.list"]) == "table" then
        return context["spawn_target.list"]
    end
    if not registry then return {} end
    local ok, listed = pcall(registry.list)
    if not ok or type(listed) ~= "table" then return {} end
    if context then context["spawn_target.list"] = listed end
    return listed
end

local function inspect_spawn_target(registry, target_path, context)
    if type(target_path) ~= "string" or target_path == "" then return nil end
    if not registry or type(registry.inspect) ~= "function" then return nil end

    local inspections = context and context["spawn_target.inspections"]
    if context and type(inspections) ~= "table" then
        inspections = {}
        context["spawn_target.inspections"] = inspections
    end
    if inspections and inspections[target_path] ~= nil then
        return inspections[target_path] or nil
    end

    local inspect_ok, inspection = pcall(registry.inspect, target_path)
    if not inspect_ok or type(inspection) ~= "table" then
        inspection = false
    end
    if inspections then inspections[target_path] = inspection end
    return inspection or nil
end

local function copy_table(source)
    local out = {}
    if type(source) ~= "table" then return out end
    for k, v in pairs(source) do out[k] = v end
    return out
end

local function target_for_worktree_path(targets, path)
    if type(path) ~= "string" then return nil end
    for _, target in ipairs(targets or {}) do
        local target_path = type(target) == "table" and target.path or nil
        if type(target_path) == "string"
            and (
                path == target_path
                or path:sub(1, #target_path + 1) == (target_path .. "/")
            )
        then
            return target
        end
    end
    return nil
end

function M.register()
    local EB = require("lib.entity_broadcast")

    EB.register("session", {
        id_field = "session_uuid",
        all = function(context)
            local Session = require("lib.session")
            local ClientSessionPayload = require("lib.client_session_payload")
            -- Session.all_info returns normalized tables; session_action reuses
            -- exactly this request-local table when the client asks for both.
            local sessions = Session.all_info()
            if context then context["session.info"] = sessions end
            return ClientSessionPayload.build_many(sessions)
        end,
        filter = function(info)
            return not require("lib.session").is_system_session(info)
        end,
    })

    EB.register("session_action", {
        id_field = "id",
        all = function(context)
            local SessionActions = require("lib.session_actions")
            return SessionActions.all(context and context["session.info"] or nil)
        end,
    })

    EB.register("workspace", {
        id_field = "workspace_id",
        all = function()
            local Hub = require("lib.hub")
            local ok, workspaces = pcall(function()
                return Hub.get():list_workspaces()
            end)
            return ok and workspaces or {}
        end,
    })

    EB.register("spawn_target", {
        id_field = "target_id",
        all = function(context)
            local registry = spawn_target_registry()
            local listed = list_spawn_targets(registry, context)
            local out = {}
            for _, target in ipairs(listed) do
                local merged = target
                local inspection = inspect_spawn_target(registry, target.path, context)
                if type(inspection) == "table" then
                    merged = copy_table(target)
                    for k, v in pairs(inspection) do merged[k] = v end
                end
                out[#out + 1] = merged
            end
            return out
        end,
    })

    EB.register("worktree", {
        id_field = "worktree_path",
        all = function(context)
            local worktrees = hub.get_worktrees()
            local registry = spawn_target_registry()
            local targets = list_spawn_targets(registry, context)
            local out = {}
            local by_path = {}
            local function append_worktree(payload)
                if type(payload) ~= "table" then return end
                local path = payload.worktree_path or payload.path
                if type(path) ~= "string" or path == "" then return end
                payload.worktree_path = path
                payload.path = payload.path or path
                if by_path[path] then
                    for k, v in pairs(payload) do by_path[path][k] = v end
                    return
                end
                by_path[path] = payload
                out[#out + 1] = payload
            end
            for _, worktree_entry in ipairs(worktrees or {}) do
                if type(worktree_entry) == "table" then
                    local payload = copy_table(worktree_entry)
                    payload.worktree_path = payload.worktree_path or payload.path
                    local target = target_for_worktree_path(targets, payload.worktree_path)
                    if target then
                        payload.target_id = target.target_id or target.id
                    end
                    append_worktree(payload)
                end
            end
            for _, target in ipairs(targets) do
                local target_path = type(target) == "table" and target.path or nil
                if type(target_path) == "string" then
                    local inspection = inspect_spawn_target(registry, target_path, context)
                    local target_worktrees = type(inspection) == "table" and inspection.worktrees or nil
                    if type(target_worktrees) == "table" then
                        for _, target_worktree in ipairs(target_worktrees) do
                            if type(target_worktree) == "table" then
                                local path = target_worktree.worktree_path or target_worktree.path
                                if type(path) == "string" and path ~= "" then
                                    append_worktree({
                                        worktree_path = path,
                                        path = path,
                                        branch = target_worktree.branch,
                                        target_id = target.target_id or target.id,
                                    })
                                end
                            end
                        end
                    end
                end
            end
            return out
        end,
    })

    EB.register("hub", {
        id_field = "hub_id",
        all = function()
            local hub_id = (hub.server_id and hub.server_id())
                or (hub.hub_id and hub.hub_id())
                or nil
            local recovery = require("hub.state").get("connections.hub_recovery_state", { state = "starting" })
            local payload = { hub_id = hub_id }
            for k, v in pairs(recovery) do payload[k] = v end
            if type(payload.hub_id) ~= "string" or payload.hub_id == "" then
                return {}
            end
            return { payload }
        end,
    })

    EB.register("connection_code", {
        id_field = "hub_id",
        all = function()
            local hub_id = hub.server_id and hub.server_id() or nil
            local code = require("hub.state").get("connections.last_connection_code", nil)
            if not hub_id or type(code) ~= "table" or next(code) == nil then
                return {}
            end
            local payload = { hub_id = hub_id }
            for k, v in pairs(code) do payload[k] = v end
            return { payload }
        end,
    })

    EB.register("template", {
        id_field = "id",
        all = function()
            local Catalog = require("lib.template_catalog")
            local ok, templates = pcall(Catalog.list)
            return ok and templates or {}
        end,
    })

    log.info("Registered built-in entity providers")
end

function M._after_reload()
    M.register()
end

return M
