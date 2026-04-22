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
    return TargetContext.resolve({
        command = command,
        metadata = command and command.metadata or nil,
        require_target_id = true,
        require_target_path = true,
    })
end

-- ============================================================================
-- Query Commands
-- ============================================================================

commands.register("list_agents", function(client, sub_id, _command)
    client:send_agent_list(sub_id)
end, { description = "Send agent list to client" })

commands.register("list_worktrees", function(client, sub_id, _command)
    local target, target_err = resolve_command_target(_command)
    if not target then
        send_command_error(client, sub_id, "worktree_list_error", target_err)
        log.warn(string.format("list_worktrees failed: %s", tostring(target_err)))
        return
    end
    client:send_worktree_list(sub_id, target)
end, { description = "Send worktree list to client" })

commands.register("list_spawn_targets", function(client, sub_id, _command)
    client:send_spawn_target_list(sub_id)
end, { description = "Send admitted spawn target list to client" })

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

    local connections = require("handlers.connections")
    send_spawn_target_feedback(
        client,
        sub_id,
        "success",
        string.format("Admitted spawn target %s", target.path or target.name or target.id or path)
    )
    connections.broadcast_spawn_target_list()
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

    local connections = require("handlers.connections")
    send_spawn_target_feedback(client, sub_id, "success", "Removed spawn target.")
    connections.broadcast_spawn_target_list()
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

    local connections = require("handlers.connections")
    send_spawn_target_feedback(client, sub_id, "success", string.format("Renamed spawn target to %s.", new_name))
    connections.broadcast_spawn_target_list()
end, { description = "Rename an admitted spawn target" })

commands.register("list_workspaces", function(client, sub_id, _command)
    local Hub = require("lib.hub")
    local ok, workspaces = pcall(function()
        return Hub.get():list_workspaces()
    end)
    if not ok then
        log.warn(string.format("list_workspaces failed: %s", tostring(workspaces)))
        workspaces = {}
    end
    client:send({
        subscriptionId = sub_id,
        type = "workspace_list",
        workspaces = workspaces,
    })
end, { description = "Send workspace list to client" })

commands.register("list_open_workspaces", function(client, sub_id, _command)
    client:send_open_workspace_list(sub_id)
end, { description = "Send currently open workspaces to client" })

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

-- Backward compat: list_profiles → list_configs
commands.register("list_profiles", function(client, sub_id, command)
    local ConfigResolver = require("lib.config_resolver")
    local target, target_err = resolve_command_target(command)
    if not target then
        send_command_error(client, sub_id, "profiles_error", target_err)
        log.warn(string.format("list_profiles failed: %s", tostring(target_err)))
        return
    end
    local device_root = config.data_dir and config.data_dir() or nil
    local repo_root = target.target_path
    local agents = ConfigResolver.list_agents(device_root, repo_root)
    client:send({
        subscriptionId = sub_id,
        type = "profiles",
        profiles = agents,
        shared_agent = #agents > 0,
        target_id = target.target_id,
        target_path = target.target_path,
        target_repo = target.target_repo,
    })
end, { description = "List available config profiles (deprecated, use list_configs)" })

-- ============================================================================
-- Agent Lifecycle Commands
-- ============================================================================

commands.register("create_agent", function(client, sub_id, command)
    local issue_or_branch = command.issue_or_branch or command.branch
    local prompt = command.prompt
    local from_worktree = command.from_worktree
    -- Accept both "agent_name" (new) and "profile" (legacy)
    local agent_name = command.agent_name or command.profile
    local workspace_id = command.workspace_id
    local workspace_name = command.workspace_name

    local target, target_err = resolve_command_target(command)
    if not target then
        send_command_error(client, sub_id, "error", target_err)
        log.warn(string.format("create_agent failed: %s", tostring(target_err)))
        return
    end

    local metadata = TargetContext.with_metadata(nil, target)
    if workspace_id or workspace_name then
        metadata.workspace_id = workspace_id
        metadata.workspace = workspace_name
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

    require("handlers.agents").handle_create_agent(
        issue_or_branch, prompt, from_worktree, client, agent_name, metadata, target
    )
    log.info(string.format("Create agent request: %s (agent: %s, workspace: %s, target: %s)",
        tostring(issue_or_branch or "main"), tostring(agent_name or "auto"),
        tostring(workspace_id or workspace_name or "none"),
        tostring(target.target_id)))
end, { description = "Create a new agent (with optional worktree, agent name, and workspace)" })

commands.register("create_accessory", function(client, sub_id, command)
    -- Accept both "accessory_name" (new) and "session_name" (legacy)
    local accessory_name = command.accessory_name or command.session_name or command.name
    local workspace_id = command.workspace_id
    local workspace_name = command.workspace_name
    local agent_name = command.agent_name or command.profile
    local target, target_err = resolve_command_target(command)
    if not target then
        send_command_error(client, sub_id, "error", target_err)
        log.warn(string.format("create_accessory failed: %s", tostring(target_err)))
        return
    end
    local metadata = TargetContext.with_metadata(command.metadata, target)

    if not accessory_name then
        log.warn("create_accessory missing accessory_name")
        return
    end

    require("handlers.agents").handle_create_accessory(
        workspace_id, workspace_name, accessory_name, agent_name, metadata, target
    )
    log.info(string.format("Create accessory request: %s (workspace: %s, target: %s)",
        accessory_name, tostring(workspace_id or workspace_name or "none"), tostring(target.target_id)))
end, { description = "Create an accessory session (no AI autonomy)" })

commands.register("rename_workspace", function(client, sub_id, command)
    local workspace_id = command.workspace_id
    local new_name = command.new_name or command.name
    if not workspace_id or not new_name then
        log.warn("rename_workspace missing workspace_id or new_name")
        return
    end

    local data_dir = config.data_dir and config.data_dir() or nil
    if not data_dir then
        log.warn("rename_workspace: no data_dir configured")
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
            end
        end

        local connections = require("handlers.connections")
        connections.broadcast_hub_event("agent_list", {
            agents = Agent.all_info(),
        })
        connections.broadcast_workspace_list()
        log.info(string.format("Workspace %s renamed to '%s'", workspace_id, new_name))
    end
end, { description = "Rename a workspace" })

commands.register("move_agent_workspace", function(_client, _sub_id, command)
    local session_id = command.id or command.agent_id or command.session_uuid or command.session_key
    local workspace_id = command.workspace_id
    local workspace_name = command.workspace_name

    if not session_id then
        log.warn("move_agent_workspace missing session identifier")
        return
    end
    if not workspace_id and not workspace_name then
        log.warn("move_agent_workspace missing workspace_id/workspace_name")
        return
    end

    local Agent = require("lib.agent")
    local session = Agent.get(session_id)
    if not session then
        log.warn(string.format("move_agent_workspace: session '%s' not found", tostring(session_id)))
        return
    end

    local moved, err = session:move_to_workspace({
        workspace_id = workspace_id,
        workspace_name = workspace_name,
    })
    if not moved then
        log.warn(string.format("move_agent_workspace failed for %s: %s",
            tostring(session_id), tostring(err)))
        return
    end

    local connections = require("handlers.connections")
    connections.broadcast_hub_event("agent_list", {
        agents = Agent.all_info(),
    })
    connections.broadcast_workspace_list()

    log.info(string.format("Moved session %s to workspace %s (%s)",
        session.session_uuid, moved.workspace_id, moved.workspace_name or "unnamed"))
end, { description = "Move a live session to another workspace" })

commands.register("update_session", function(_client, _sub_id, command)
    local session_id = command.id or command.agent_id or command.session_uuid or command.session_key
    if not session_id then
        log.warn("update_session missing session identifier")
        return
    end

    local Agent = require("lib.agent")
    local session = Agent.get(session_id)
    if not session then
        log.warn(string.format("update_session: session '%s' not found", tostring(session_id)))
        return
    end

    -- Only allow updating label and task (not arbitrary fields)
    local fields = {}
    if command.label ~= nil then fields.label = command.label end
    if command.task ~= nil then fields.task = command.task end

    if next(fields) then
        session:update(fields)
        log.info(string.format("Session %s updated: %s", session.session_uuid,
            table.concat((function()
                local parts = {}
                for k, v in pairs(fields) do parts[#parts + 1] = k .. "=" .. tostring(v) end
                return parts
            end)(), ", ")))
    end
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
        local agent_name = command.agent_name or command.profile
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

commands.register("delete_agent", function(_client, _sub_id, command)
    local session_id = command.id or command.agent_id or command.session_uuid or command.session_key
    local delete_worktree = command.delete_worktree or false

    if session_id then
        require("handlers.agents").handle_delete_session(session_id, delete_worktree)
        log.info(string.format("Delete session request: %s", session_id))
    else
        log.warn("delete_agent missing session identifier")
    end
end, { description = "Delete a session (agent or accessory, optionally with worktree)" })

commands.register("toggle_hosted_preview", function(_client, _sub_id, command)
    local Session = require("lib.session")
    local HostedPreview = require("lib.hosted_preview")
    local session_id = command.session_uuid or command.agent_id or command.id
    if not session_id then
        log.warn("toggle_hosted_preview missing session identifier")
        return
    end

    local session = Session.get(session_id)
    if not session then
        log.warn(string.format("toggle_hosted_preview: session not found: %s", session_id))
        return
    end

    if not session._port then
        log.warn(string.format("toggle_hosted_preview: session has no forwarded port: %s", session_id))
        return
    end

    local hosted = session.hosted_preview
    local enabled = command.enabled
    if enabled == nil then
        enabled = not (hosted and (hosted.status == "starting" or hosted.status == "running"))
    end

    if enabled then
        local _, err = HostedPreview.enable(session)
        if err then
            log.warn(string.format("toggle_hosted_preview failed: %s", tostring(err)))
        end
    else
        HostedPreview.disable(session)
    end
end, { description = "Enable or disable a Cloudflare-hosted preview for a forwarded session" })

-- Alias: delete_session → delete_agent
commands.register("delete_session", function(_client, _sub_id, command)
    local session_id = command.id or command.session_uuid or command.agent_id or command.session_key
    local delete_worktree = command.delete_worktree or false

    if session_id then
        require("handlers.agents").handle_delete_session(session_id, delete_worktree)
        log.info(string.format("Delete session request: %s", session_id))
    else
        log.warn("delete_session missing session identifier")
    end
end, { description = "Delete a session (alias for delete_agent)" })

commands.register("select_agent", function(client, _sub_id, command)
    local new_selection = command.session_uuid or command.id
    log.debug(string.format("Select agent: %s", tostring(new_selection)))

    if not client or new_selection == nil then
        return
    end

    -- Phase 2b: selection is baked into hub-rendered ui_layout_tree_v1
    -- frames. Record the new selection on THIS client so its next render
    -- (triggered below and on the next session_updated broadcast) applies
    -- the correct `tree_item.selected` to the matching row. Other peers'
    -- selections are untouched.
    if client.selected_session_uuid == new_selection then
        return
    end
    client.selected_session_uuid = new_selection

    -- Re-broadcast this client's hub subscriptions with the new selection.
    -- The per-subscription dedup in `layout_broadcast` compares versions
    -- against THIS sub's baseline, so the frames ship exactly when the
    -- selection change actually alters the rendered tree.
    for sub_id, sub in pairs(client.subscriptions or {}) do
        if sub.channel == "hub" then
            pcall(client.send_ui_layout_trees, client, sub_id)
        end
    end
end, { description = "Record per-client selection and re-broadcast its UI trees" })

-- Phase 2b: structured browser → hub action envelopes. Wraps the Phase-1
-- command channel with semantic action ids so plugin-registered handlers
-- (`action.on("botster.session.select", name, handler)`) can intercept
-- intents uniformly. Falls back to the legacy command for known action ids
-- so browsers emitting `ui_action_v1` do not regress vs `select_agent` etc.
commands.register("ui_action_v1", function(client, sub_id, command)
    local envelope = command.envelope
    if type(envelope) ~= "table" then
        log.warn("ui_action_v1 missing envelope table")
        return
    end
    local action = require("lib.action")
    action.dispatch(envelope, {
        client = client,
        sub_id = sub_id,
        target_surface = command.target_surface,
    })
end, { description = "Dispatch a semantic UI action envelope to hub handlers" })

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
    local name = command.name or command.plugin_name
    if not name then
        if client then client:send({ subscriptionId = sub_id, type = "error", message = "Missing plugin name" }) end
        return
    end
    local ok, err = loader.reload_plugin(name)
    if client then
        client:send({ subscriptionId = sub_id, type = "plugin_reloaded", name = name, success = ok, error = not ok and tostring(err) or nil })
    end
end, { description = "Reload a plugin by name" })

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
    local broadcast_ok, broadcast_err = pcall(connections.broadcast_ui_layout_trees)
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
    local name = command.name or command.plugin_name
    if not name then
        if client then client:send({ subscriptionId = sub_id, type = "error", message = "Missing plugin name" }) end
        return
    end
    local ok, err = loader.enable_plugin(name)
    if client then
        client:send({ subscriptionId = sub_id, type = "plugin_enabled", name = name, success = ok, error = not ok and tostring(err) or nil })
    end
end, { description = "Enable a disabled plugin" })

commands.register("disable_plugin", function(client, sub_id, command)
    local name = command.name or command.plugin_name
    if not name then
        if client then client:send({ subscriptionId = sub_id, type = "error", message = "Missing plugin name" }) end
        return
    end
    local ok, err = loader.disable_plugin(name)
    if client then
        client:send({ subscriptionId = sub_id, type = "plugin_disabled", name = name, success = ok, error = not ok and tostring(err) or nil })
    end
end, { description = "Disable a plugin" })

-- Lifecycle hooks for hot-reload
function M._before_reload()
    log.info("handlers/commands.lua reloading")
end

function M._after_reload()
    log.info(string.format("handlers/commands.lua reloaded (%d commands)", commands.count()))
end

log.info(string.format("Built-in commands registered: %d", commands.count()))

return M
