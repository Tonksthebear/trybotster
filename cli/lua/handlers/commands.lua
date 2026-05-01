-- Built-in hub command registrations (hot-reloadable)
--
-- Registers all built-in hub channel commands with the command registry.
--
-- Users can override built-in commands or add new ones:
--   local commands = require("lib.commands")
--   commands.register("my_command", function(client, sub_id, command)
--       client:send({ subscriptionId = sub_id, type = "my_response", data = "hello" })
--   end)

local commands = require("lib.commands")
local TargetContext = require("lib.target_context")

local function generate_request_id()
    local hub_id = hub and hub.hub_id and hub.hub_id() or nil
    local hub_prefix = hub_id and hub_id:sub(1, 8) or "00000000"
    return string.format("msg_%s_%d_%06x", hub_prefix, os.time(), math.random(0, 0xffffff))
end

local function send_command_response(client, sub_id, command, data)
    if not client or not command.request_id then return end
    data = data or {}
    data.subscriptionId = sub_id
    data.type = "command_response"
    data.request_id = command.request_id
    client:send(data)
end

local function send_command_error(client, sub_id, error_type, message)
    if not client then return end
    client:send({
        subscriptionId = sub_id,
        type = error_type or "error",
        error = message,
    })
end

local function send_spawn_target_feedback(client, sub_id, tone, message)
    if not client then return end
    client:send({
        subscriptionId = sub_id,
        type = "spawn_target_feedback",
        tone = tone or "neutral",
        message = message,
    })
end

local function resolve_command_target(command)
    local target, err = TargetContext.resolve({
        command = command,
        metadata = command and command.metadata or nil,
        require_target_id = true,
        require_target_path = true,
    })
    if target then return target end
    if command and command.repo then
        return TargetContext.find_by_repo(command.repo)
    end
    return nil, err
end

commands.register("add_spawn_target", function(client, sub_id, command)
    local registry = rawget(_G, "spawn_targets")
    if not registry or type(registry.add) ~= "function" then
        send_spawn_target_feedback(client, sub_id, "error", "Spawn target registry is unavailable.")
        return
    end

    local path = command.path or command.target_path
    local name = command.name or command.target_name
    if not path or path == "" then
        send_spawn_target_feedback(client, sub_id, "error", "Path is required to admit a spawn target.")
        return
    end

    local ok, target = pcall(registry.add, path, name)
    if not ok or type(target) ~= "table" then
        send_spawn_target_feedback(client, sub_id, "error", tostring(target))
        log.warn(string.format("add_spawn_target failed: %s", tostring(target)))
        return
    end

    send_spawn_target_feedback(
        client,
        sub_id,
        "success",
        string.format("Admitted spawn target %s", target.path or target.name or target.id or path)
    )
    -- Wire protocol: publish spawn targets through the shared entity model
    -- layer so command handlers do not know envelope details.
    local EntityModel = require("lib.entity_model")
    if require("lib.entity_broadcast").is_registered("spawn_target") then
        local registry = rawget(_G, "spawn_targets")
        local list_ok, listed = pcall(registry.list)
        if list_ok and type(listed) == "table" then
            for _, t in ipairs(listed) do
                EntityModel.upsert_spawn_target(t)
            end
        end
    end
end, { description = "Admit a directory as a spawn target" })

commands.register("remove_spawn_target", function(client, sub_id, command)
    local registry = rawget(_G, "spawn_targets")
    if not registry or type(registry.remove) ~= "function" then
        send_spawn_target_feedback(client, sub_id, "error", "Spawn target registry is unavailable.")
        return
    end

    local target_id = command.target_id
    if not target_id or target_id == "" then
        send_spawn_target_feedback(client, sub_id, "error", "Target ID is required to remove a spawn target.")
        return
    end

    local ok, removed = pcall(registry.remove, target_id)
    if not ok or not removed then
        send_spawn_target_feedback(client, sub_id, "error", tostring(removed or "Failed to remove spawn target."))
        log.warn(string.format("remove_spawn_target failed: %s", tostring(removed)))
        return
    end

    send_spawn_target_feedback(client, sub_id, "success", "Removed spawn target.")
    require("lib.entity_model").remove_spawn_target(target_id)
end, { description = "Remove an admitted spawn target" })

commands.register("rename_spawn_target", function(client, sub_id, command)
    local registry = rawget(_G, "spawn_targets")
    if not registry or type(registry.update) ~= "function" then
        send_spawn_target_feedback(client, sub_id, "error", "Spawn target registry is unavailable.")
        return
    end

    local target_id = command.target_id
    if not target_id or target_id == "" then
        send_spawn_target_feedback(client, sub_id, "error", "Target ID is required to rename a spawn target.")
        return
    end

    local new_name = command.new_name
    if not new_name or new_name == "" then
        send_spawn_target_feedback(client, sub_id, "error", "New name is required to rename a spawn target.")
        return
    end

    local ok, updated = pcall(registry.update, target_id, new_name)
    if not ok or type(updated) ~= "table" then
        send_spawn_target_feedback(client, sub_id, "error", tostring(updated or "Failed to rename spawn target."))
        log.warn(string.format("rename_spawn_target failed: %s", tostring(updated)))
        return
    end

    send_spawn_target_feedback(client, sub_id, "success", string.format("Renamed spawn target to %s.", new_name))
    require("lib.entity_model").patch_spawn_target(target_id, { target_name = new_name })
end, { description = "Rename an admitted spawn target" })

local function route_push_control(client, _sub_id, command)
    local push = rawget(_G, "push")
    if not push or type(push.control) ~= "function" then
        log.warn("push control primitive is unavailable")
        return
    end
    push.control(client.peer_id, command)
end

commands.register("push_status_req", route_push_control, { description = "Query browser push status" })
commands.register("vapid_generate", route_push_control, { description = "Generate VAPID keys for browser push" })
commands.register("vapid_pub_req", route_push_control, { description = "Request VAPID public key" })
commands.register("vapid_key_req", route_push_control, { description = "Request VAPID keypair for copy flow" })
commands.register("vapid_key_set", route_push_control, { description = "Install copied VAPID keypair" })
commands.register("push_sub", route_push_control, { description = "Store browser push subscription" })
commands.register("push_test", route_push_control, { description = "Send a browser push test notification" })
commands.register("push_disable", route_push_control, { description = "Disable browser push notifications" })

local function send_agent_config(client, sub_id, command)
    local ConfigResolver = require("lib.config_resolver")
    local target, target_err = resolve_command_target(command)
    if not target then
        send_command_error(client, sub_id, "agent_config_error", target_err)
        log.warn(string.format("list_configs failed: %s", tostring(target_err)))
        return
    end
    local device_root = config.data_dir and config.data_dir() or nil
    local repo_root = target.target_path
    local agents = ConfigResolver.list_agents(device_root, repo_root)
    local accessories = ConfigResolver.list_accessories(device_root, repo_root)
    local workspaces = ConfigResolver.list_workspaces(device_root, repo_root)
    client:send({
        subscriptionId = sub_id,
        type = "agent_config",
        target_id = target.target_id,
        target_path = target.target_path,
        target_repo = target.target_repo,
        agents = agents,
        accessories = accessories,
        workspaces = workspaces,
    })
end

commands.register("list_configs", function(client, sub_id, command)
    send_agent_config(client, sub_id, command)
end, { description = "List available agents, accessories, and workspaces" })

commands.register("list_agent_config", function(client, sub_id, command)
    send_agent_config(client, sub_id, command)
end, { description = "List available agent config (alias for list_configs)" })

-- ============================================================================
-- Agent Lifecycle Commands
-- ============================================================================

commands.register("create_agent", function(client, sub_id, command)
    command.request_id = command.request_id or generate_request_id()
    local issue_or_branch = command.issue_or_branch or command.branch
    local prompt = command.prompt
    local from_worktree = command.from_worktree
    local agent_name = command.agent_name
    local workspace_id = command.workspace_id
    local workspace_name = command.workspace_name

    local target, target_err = resolve_command_target(command)
    if not target then
        send_command_error(client, sub_id, "error", target_err)
        send_command_response(client, sub_id, command, { ok = false, error = target_err })
        log.warn(string.format("create_agent failed: %s", tostring(target_err)))
        return
    end

    local metadata = TargetContext.with_metadata(command.metadata, target)
    if command.request_id and metadata.request_id == nil then
        metadata.request_id = command.request_id
    end
    if command.label and metadata.label == nil then
        metadata.label = command.label
    end
    if command.assignment_id and metadata.assignment_id == nil then
        metadata.assignment_id = command.assignment_id
    end
    if workspace_id or workspace_name then
        metadata.workspace_id = workspace_id or metadata.workspace_id
        metadata.workspace = workspace_name or metadata.workspace
    end
    if command.invocation_url and not metadata.invocation_url then
        metadata.invocation_url = command.invocation_url
    end

    -- Optional workspace template for auto-spawning accessory bundles.
    local workspace_config_name = command.workspace_template
    if workspace_config_name then
        local ConfigResolver = require("lib.config_resolver")
        local device_root = config.data_dir and config.data_dir() or nil
        local repo_root = target.target_path
        local resolved = ConfigResolver.resolve_all({
            device_root = device_root,
            repo_root = repo_root,
            require_agent = false,
        })
        if resolved and resolved.workspaces[workspace_config_name] then
            metadata.workspace_config = resolved.workspaces[workspace_config_name]
            -- If no explicit runtime workspace was supplied, use template name.
            metadata.workspace = metadata.workspace or workspace_config_name
        end
    end

    local agent, err = require("handlers.agents").handle_create_agent(
        issue_or_branch, prompt, from_worktree, client, agent_name, metadata, target
    )
    if err then
        send_command_response(client, sub_id, command, { ok = false, error = err })
        return
    end
    if agent then
        send_command_response(client, sub_id, command, {
            ok = true,
            session_uuid = agent.session_uuid,
            id = agent.session_uuid,
            assignment_id = metadata.assignment_id,
        })
    else
        send_command_response(client, sub_id, command, {
            ok = true,
            status = "pending",
            assignment_id = metadata.assignment_id,
        })
    end
    log.info(string.format("Create agent request: %s (agent: %s, workspace: %s, target: %s)",
        tostring(issue_or_branch or "main"), tostring(agent_name or "auto"),
        tostring(workspace_id or workspace_name or "none"),
        tostring(target.target_id)))
end, { description = "Create a new agent (with optional worktree, agent name, and workspace)" })

commands.register("create_accessory", function(client, sub_id, command)
    local accessory_name = command.accessory_name
    local workspace_id = command.workspace_id
    local workspace_name = command.workspace_name
    local agent_name = command.agent_name
    local target, target_err = resolve_command_target(command)
    if not target then
        send_command_error(client, sub_id, "error", target_err)
        send_command_response(client, sub_id, command, { ok = false, error = target_err })
        log.warn(string.format("create_accessory failed: %s", tostring(target_err)))
        return
    end
    local metadata = TargetContext.with_metadata(command.metadata, target)

    if not accessory_name then
        log.warn("create_accessory missing accessory_name")
        send_command_response(client, sub_id, command, { ok = false, error = "accessory_name is required" })
        return
    end

    local accessory, err = require("handlers.agents").handle_create_accessory(
        workspace_id, workspace_name, accessory_name, agent_name, metadata, target
    )
    if err then
        send_command_response(client, sub_id, command, { ok = false, error = err })
        return
    end
    send_command_response(client, sub_id, command, {
        ok = accessory ~= nil,
        session_uuid = accessory and accessory.session_uuid or nil,
        id = accessory and accessory.session_uuid or nil,
    })
    log.info(string.format("Create accessory request: %s (workspace: %s, target: %s)",
        accessory_name, tostring(workspace_id or workspace_name or "none"), tostring(target.target_id)))
end, { description = "Create an accessory session (no AI autonomy)" })

commands.register("list_owned_sessions", function(client, sub_id, command)
    local owner_plugin = command.owner_plugin
    if not owner_plugin or owner_plugin == "" then
        send_command_response(client, sub_id, command, { ok = false, error = "owner_plugin is required" })
        return
    end

    local owned = {}
    local Agent = require("lib.agent")
    for _, session in ipairs(Agent.list()) do
        if session.owner_plugin == owner_plugin
                or (session.metadata and session.metadata.owner_plugin == owner_plugin) then
            owned[#owned + 1] = {
                session_uuid = session.session_uuid,
                label = session.label,
                metadata = session.metadata,
                status = session.status,
            }
        end
    end

    send_command_response(client, sub_id, command, { ok = true, sessions = owned })
end, { description = "List sessions owned by a plugin" })

commands.register("rename_workspace", function(client, sub_id, command)
    local workspace_id = command.workspace_id
    local new_name = command.new_name or command.name
    if not workspace_id or not new_name then
        log.warn("rename_workspace missing workspace_id or new_name")
        send_command_response(client, sub_id, command, { ok = false, error = "workspace_id and new_name are required" })
        return
    end

    local data_dir = config.data_dir and config.data_dir() or nil
    if not data_dir then
        log.warn("rename_workspace: no data_dir configured")
        send_command_response(client, sub_id, command, { ok = false, error = "no data_dir configured" })
        return
    end

    local ws = require("lib.workspace_store")
    local ok = ws.rename_workspace(data_dir, workspace_id, new_name)
    if ok then
        local Agent = require("lib.agent")
        for _, session in ipairs(Agent.list()) do
            if session._workspace_id == workspace_id then
                session._workspace_name = new_name
                session:set_meta("workspace", new_name)
                session:_sync_workspace_manifest()
                session:publish_entity()
            end
        end

        require("lib.entity_model").patch_workspace(workspace_id, { name = new_name })
        log.info(string.format("Workspace %s renamed to '%s'", workspace_id, new_name))
        send_command_response(client, sub_id, command, { ok = true, workspace_id = workspace_id, name = new_name })
    else
        send_command_response(client, sub_id, command, { ok = false, error = "failed to rename workspace" })
    end
end, { description = "Rename a workspace" })

commands.register("move_agent_workspace", function(client, sub_id, command)
    local session_id = command.session_uuid or command.agent_id
    local workspace_id = command.workspace_id
    local workspace_name = command.workspace_name

    if not session_id then
        log.warn("move_agent_workspace missing session identifier")
        send_command_response(client, sub_id, command, { ok = false, error = "session identifier is required" })
        return
    end
    if not workspace_id and not workspace_name then
        log.warn("move_agent_workspace missing workspace_id/workspace_name")
        send_command_response(client, sub_id, command, { ok = false, error = "workspace_id or workspace_name is required" })
        return
    end

    local Agent = require("lib.agent")
    local session = Agent.get(session_id)
    if not session then
        log.warn(string.format("move_agent_workspace: session '%s' not found", tostring(session_id)))
        send_command_response(client, sub_id, command, { ok = false, error = "session not found" })
        return
    end

    local moved, err = session:move_to_workspace({
        workspace_id = workspace_id,
        workspace_name = workspace_name,
    })
    if not moved then
        log.warn(string.format("move_agent_workspace failed for %s: %s",
            tostring(session_id), tostring(err)))
        send_command_response(client, sub_id, command, { ok = false, error = err or "move failed" })
        return
    end

    -- Wire protocol: move_to_workspace publishes the moved session. Upsert
    -- workspaces so target/old workspace status and membership summaries
    -- reach clients filtering for active workspaces.
    local EntityModel = require("lib.entity_model")
    if require("lib.entity_broadcast").is_registered("workspace") then
        local Hub = require("lib.hub")
        local ok, workspaces = pcall(function() return Hub.get():list_workspaces() end)
        if ok and type(workspaces) == "table" then
            for _, workspace in ipairs(workspaces) do
                EntityModel.upsert_workspace(workspace)
            end
        end
    end

    log.info(string.format("Moved session %s to workspace %s (%s)",
        session.session_uuid, moved.workspace_id, moved.workspace_name or "unnamed"))
    send_command_response(client, sub_id, command, {
        ok = true,
        agent_id = session.session_uuid,
        session_uuid = session.session_uuid,
        workspace_id = moved.workspace_id,
        workspace_name = moved.workspace_name,
        previous_workspace_id = moved.previous_workspace_id,
        previous_workspace_name = moved.previous_workspace_name,
    })
end, { description = "Move a live session to another workspace" })

commands.register("update_session", function(client, sub_id, command)
    local session_id = command.session_uuid or command.agent_id
    if not session_id then
        log.warn("update_session missing session identifier")
        send_command_response(client, sub_id, command, { ok = false, error = "session identifier is required" })
        return
    end

    local Agent = require("lib.agent")
    local session = Agent.get(session_id)
    if not session then
        log.warn(string.format("update_session: session '%s' not found", tostring(session_id)))
        send_command_response(client, sub_id, command, { ok = false, error = "session not found" })
        return
    end

    -- Only allow updating label and task (not arbitrary fields)
    local fields = {}
    if command.label ~= nil then fields.label = command.label end
    if command.task ~= nil then fields.task = command.task end

    if not next(fields) then
        log.warn("update_session missing updatable fields")
        send_command_response(client, sub_id, command, { ok = false, error = "label or task is required" })
        return
    end

    session:update(fields)
    log.info(string.format("Session %s updated: %s", session.session_uuid,
        table.concat((function()
            local parts = {}
            for k, v in pairs(fields) do parts[#parts + 1] = k .. "=" .. tostring(v) end
            return parts
        end)(), ", ")))
    send_command_response(client, sub_id, command, { ok = true, session_uuid = session.session_uuid })
end, { description = "Update session label or task" })

commands.register("reopen_worktree", function(client, _sub_id, command)
    local path = command.path
    local branch = command.branch or ""
    local prompt = command.prompt

    if path then
        local target, target_err = resolve_command_target(command)
        if not target then
            send_command_error(client, _sub_id, "error", target_err)
            log.warn(string.format("reopen_worktree failed: %s", tostring(target_err)))
            return
        end
        local agent_name = command.agent_name
        local metadata = TargetContext.with_metadata(nil, target)
        if command.workspace_id or command.workspace_name then
            metadata.workspace_id = command.workspace_id
            metadata.workspace = command.workspace_name
        end
        require("handlers.agents").handle_create_agent(
            branch, prompt, path, client, agent_name, metadata, target
        )
        log.info(string.format("Reopen worktree request: %s", path))
    else
        log.warn("reopen_worktree missing path")
    end
end, { description = "Reopen an existing worktree as an agent" })

commands.register("delete_agent", function(client, sub_id, command)
    local session_id = command.session_uuid or command.agent_id
    local delete_worktree = command.delete_worktree or false

    if session_id then
        local ok = require("handlers.agents").handle_delete_session(session_id, delete_worktree)
        log.info(string.format("Delete session request: %s", session_id))
        send_command_response(client, sub_id, command, {
            ok = ok == true,
            session_uuid = session_id,
            error = ok == true and nil or "session not found",
        })
    else
        log.warn("delete_agent missing session identifier")
        send_command_response(client, sub_id, command, { ok = false, error = "session identifier is required" })
    end
end, { description = "Delete a session (agent or accessory, optionally with worktree)" })

commands.register("execute_session_action", function(client, sub_id, command)
    local session_uuid = command.session_uuid
    local action_id = command.action_id
    local ok, result = require("lib.session_actions").run(session_uuid, action_id, {
        client = client,
        sub_id = sub_id,
        params = command.params,
    })
    if not ok then
        log.warn(string.format("execute_session_action failed: %s", tostring(result)))
        send_command_error(client, sub_id, "session_action_error", result)
    end
end, { description = "Execute a plugin-registered session action" })

commands.register("select_agent", function(_client, _sub_id, command)
    -- Wire protocol: selection is purely client-side (web
    -- ui-presentation-store, TUI widget_state). Hub no longer tracks
    -- per-client selection or re-renders trees on selection changes. This
    -- handler is kept as a no-op acknowledgment for cross-client handoff
    -- flows that may evolve later (e.g. focus a session in the TUI from a
    -- browser click).
    local new_selection = command.session_uuid
    log.debug(string.format("select_agent: %s", tostring(new_selection)))
end, { description = "Acknowledge selection (client-side only)" })

-- Phase 2b: structured browser → hub action envelopes. Wraps the Phase-1
-- command channel with semantic action ids so plugin-registered handlers
-- (`action.on("botster.session.select", name, handler)`) can intercept
-- intents uniformly. Falls back to the legacy command for known action ids
-- so browsers emitting `ui_action` do not regress vs `select_agent` etc.
commands.register("ui_action", function(client, sub_id, command)
    local envelope = command.envelope
    if type(envelope) ~= "table" then
        log.warn("ui_action missing envelope table")
        return
    end
    local action = require("lib.action")
    action.dispatch(envelope, {
        client = client,
        sub_id = sub_id,
        target_surface = command.target_surface,
    })
end, { description = "Dispatch a semantic UI action envelope to hub handlers" })

do
    local plugin_assets = require("lib.plugin_assets")
    plugin_assets._install_action_handler()

    commands.register("plugin_asset:read", function(client, sub_id, command)
        local request_id = command.request_id
        local asset_id = command.asset_id or command.assetId
        local result, err = plugin_assets.read(asset_id)
        local response
        if result then
            response = {
                type = "plugin_asset:response",
                request_id = request_id,
                subscriptionId = sub_id,
                ok = true,
                asset_id = result.asset_id,
                content = result.content,
                content_type = result.content_type,
                version = result.version,
            }
        else
            response = {
                type = "plugin_asset:response",
                request_id = request_id,
                subscriptionId = sub_id,
                ok = false,
                asset_id = asset_id,
                error = err or "Unable to read plugin asset",
            }
        end
        client:send(response)
    end, { description = "Read a plugin-exposed static asset" })
end

-- Phase 4b: surface subpath notifier. The browser fires this whenever its
-- URL changes within a registered surface so the hub updates per-client
-- `surface_subpaths[surface_name]` and re-renders just that surface for
-- this subscription. Returns `action.HANDLED` so we don't silently drop
-- into a legacy command fallback if one is ever added.
--
-- Payload shape: `{ target_surface = "kanban", subpath = "/board/42" }`.
-- Browser also accepts `surface` / `path` aliases in case a plugin emits a
-- slightly different shape from a Lua action builder — action observers
-- plan on normalising.
do
    local action = require("lib.action")
    action.on("botster.surface.subpath", "builtin.surface.subpath", function(envelope, ctx)
        local client = ctx and ctx.client
        if not client then return action.HANDLED end
        local payload = envelope.payload or {}
        local surface_name = payload.target_surface or payload.surface
        local subpath = payload.subpath or payload.path or "/"
        if type(surface_name) ~= "string" or surface_name == "" then
            log.debug("botster.surface.subpath: missing target_surface; ignoring")
            return action.HANDLED
        end
        if type(subpath) ~= "string" or subpath == "" then subpath = "/" end
        if type(client.set_surface_subpath) == "function" then
            client:set_surface_subpath(surface_name, subpath)
        else
            -- Hot-reload seam: Client methods upgrade in place but defend
            -- against a stale VM where the method hasn't landed yet.
            client.surface_subpaths = client.surface_subpaths or {}
            client.surface_subpaths[surface_name] = subpath
        end
        return action.HANDLED
    end)
end

commands.register("clear_notification", function(_client, _sub_id, command)
    local session_uuid = command.session_uuid
    if session_uuid then
        _clear_session_notification(session_uuid)
    else
        log.warn("clear_notification missing session_uuid")
    end
end, { description = "Clear notification flag on a session" })

-- ============================================================================
-- Connection Commands
-- ============================================================================

commands.register("get_connection_code", function(_client, _sub_id, _command)
    connection.generate()
end, { description = "Get or generate connection code with QR" })

commands.register("regenerate_connection_code", function(_client, _sub_id, _command)
    connection.regenerate()
    log.info("Connection code regeneration requested")
end, { description = "Force-regenerate connection code" })

commands.register("copy_connection_url", function(_client, _sub_id, _command)
    connection.copy_to_clipboard()
end, { description = "Copy connection URL to clipboard" })

-- ============================================================================
-- Hub Control Commands
-- ============================================================================

commands.register("quit", function(_client, _sub_id, _command)
    hub.quit()
end, { description = "Shut down the hub" })

commands.register("restart_hub", function(_client, _sub_id, _command)
    hub.exec_restart()
end, { description = "Graceful restart — agents survive the Hub restarting" })

commands.register("dev_rebuild", function(_client, _sub_id, _command)
    hub.dev_rebuild()
end, { description = "Dev: cargo build then exec-restart — agents survive (requires cargo on PATH)" })

-- ============================================================================
-- Update Commands
-- ============================================================================

commands.register("check_update", function(client, sub_id, _command)
    local ok, status = pcall(update.check)
    if not ok then
        client:send({
            subscriptionId = sub_id,
            type = "update_error",
            error = tostring(status),
        })
        return
    end
    local agents = require("lib.agent").all_info()
    local active_count = 0
    for _, agent in ipairs(agents) do
        if agent.status ~= "closed" then active_count = active_count + 1 end
    end
    client:send({
        subscriptionId = sub_id,
        type = "update_status",
        status = status.status,
        current = status.current,
        latest = status.latest,
        active_agents = active_count,
    })
end, { description = "Check for CLI updates" })

commands.register("install_update", function(client, sub_id, _command)
    local result = update.install()
    if result.error then
        client:send({
            subscriptionId = sub_id,
            type = "update_error",
            error = result.error,
        })
    end
end, { description = "Install update and restart (kills active agents)" })

-- ============================================================================
-- Module Interface
-- ============================================================================

local M = {}

-- ============================================================================
-- Plugin Management Commands
-- ============================================================================

local loader = require("hub.loader")

commands.register("list_plugins", function(client, sub_id, _command)
    local plugins = loader.list_plugins()
    if client then
        client:send({ subscriptionId = sub_id, type = "plugin_list", plugins = plugins })
    end
end, { description = "List all plugins with status" })

commands.register("reload_plugin", function(client, sub_id, command)
    local key = command.key or command.plugin_key or command.name or command.plugin_name
    if not key then
        if client then client:send({ subscriptionId = sub_id, type = "error", message = "Missing plugin key" }) end
        return
    end
    local ok, err = loader.reload_plugin(key)
    if client then
        client:send({ subscriptionId = sub_id, type = "plugin_reloaded", key = key, name = key, success = ok, error = not ok and tostring(err) or nil })
    end
end, { description = "Reload a plugin by key" })

-- Explicit invalidation of the web layout cache + proactive rebroadcast to
-- every subscribed browser. Matches the `reload_plugin` pattern: the hub
-- does NOT watch layout files, so users call this after editing
-- `.botster/layout_web.lua` (or a shared override) to push their changes.
commands.register("reload_layout", function(client, sub_id, _command)
    local ok_reload, err = pcall(function()
        web_layout.reload()
    end)
    if not ok_reload then
        if client then
            client:send({
                subscriptionId = sub_id,
                type = "layout_reloaded",
                success = false,
                error = tostring(err),
            })
        end
        return
    end

    -- Trigger proactive rebroadcast so subscribers render the new layout
    -- without waiting for the next state-change tick.
    local connections = require("handlers.connections")
    -- Wire protocol: tree dedup is global, so invalidate first to force
    -- the broadcast through.
    local TreeSnapshot = require("lib.tree_snapshot")
    pcall(TreeSnapshot.invalidate)
    local broadcast_ok, broadcast_err = pcall(connections.broadcast_ui_tree_snapshots)
    if not broadcast_ok then
        log.warn(string.format("reload_layout: broadcast failed: %s", tostring(broadcast_err)))
    end

    if client then
        client:send({
            subscriptionId = sub_id,
            type = "layout_reloaded",
            success = true,
        })
    end
end, { description = "Reload the web UI layout overrides and rebroadcast to subscribers" })

commands.register("enable_plugin", function(client, sub_id, command)
    local key = command.key or command.plugin_key or command.name or command.plugin_name
    if not key then
        if client then client:send({ subscriptionId = sub_id, type = "error", message = "Missing plugin key" }) end
        return
    end
    local ok, err = loader.enable_plugin(key)
    if client then
        client:send({ subscriptionId = sub_id, type = "plugin_enabled", key = key, name = key, success = ok, error = not ok and tostring(err) or nil })
    end
end, { description = "Enable a disabled plugin by key" })

commands.register("disable_plugin", function(client, sub_id, command)
    local key = command.key or command.plugin_key or command.name or command.plugin_name
    if not key then
        if client then client:send({ subscriptionId = sub_id, type = "error", message = "Missing plugin key" }) end
        return
    end
    local ok, err = loader.disable_plugin(key)
    if client then
        client:send({ subscriptionId = sub_id, type = "plugin_disabled", key = key, name = key, success = ok, error = not ok and tostring(err) or nil })
    end
end, { description = "Disable a plugin by key" })

-- Lifecycle hooks for hot-reload
function M._before_reload()
    log.info("handlers/commands.lua reloading")
end

function M._after_reload()
    log.info(string.format("handlers/commands.lua reloaded (%d commands)", commands.count()))
end

log.info(string.format("Built-in commands registered: %d", commands.count()))

return M
