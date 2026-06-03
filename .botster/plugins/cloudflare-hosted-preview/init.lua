-- @template Cloudflare Hosted Preview
-- @description Expose port-forwarded sessions with Cloudflare quick tunnels
-- @category plugins
-- @dest plugins/cloudflare-hosted-preview/init.lua
-- @scope device
-- @version 1.0.0

-- Cloudflare hosted-preview session action.
--
-- The Cloudflare quick-tunnel lifecycle is plugin-owned: this module registers
-- a generic session action, owns the connector session, watches
-- cloudflared output, probes URL readiness, and mirrors action state onto the
-- parent session.

local Hub = require("lib.hub")
local Session = require("lib.session")
local SessionActions = require("lib.session_actions")
local TargetContext = require("lib.target_context")
local state = require("hub.state")

local ACTION_ID = "cloudflare.preview.toggle"
local PLUGIN_NAME = "cloudflare-hosted-preview"
local PLUGIN_STATE_KEY = "cloudflare_hosted_preview"
local CONNECTOR_SYSTEM_KIND = "cloudflare_hosted_preview_connector"
local CLOUDFLARED_INSTALL_URL =
    "https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/"
local MISSING_BINARY_ERROR =
    "Hosted preview requires cloudflared to be installed on this machine."
local QUICK_TUNNEL_CONFIG_BASENAME = "botster-cloudflared-quick.yml"
local PROBE_TIMEOUT_SECS = 15.0

local connector_output_buffers = state.get("cloudflare_hosted_preview.connector_output_buffers", {})
local prepare_seq = state.get("cloudflare_hosted_preview.prepare_seq", { value = 0 })
local event_subs = {}

local M = {}

local function session_port(session)
    if type(session) ~= "table" then return nil end
    local port = session._port or session.port
    if port == false or port == 0 or port == "" then return nil end
    return port
end

local function trycloudflare_url_from_text(text)
    if type(text) ~= "string" or text == "" then
        return nil
    end
    local host = text:match("https://([%w%-]+%.trycloudflare%.com)")
    if not host then
        return nil
    end
    return "https://" .. host, host
end

local function cloudflared_command()
    local override = os.getenv("BOTSTER_CLOUDFLARED_BIN")
    if type(override) == "string" and override:match("%S") then
        return override
    end

    return "cloudflared"
end

local function quick_tunnel_config_path()
    local override = os.getenv("BOTSTER_CLOUDFLARED_QUICK_CONFIG")
    if type(override) == "string" and override:match("%S") then
        return override
    end

    local tmpdir = os.getenv("TMPDIR")
    if type(tmpdir) ~= "string" or not tmpdir:match("%S") then
        tmpdir = "/tmp"
    end
    return tmpdir:gsub("/+$", "") .. "/" .. QUICK_TUNNEL_CONFIG_BASENAME
end

local function next_prepare_request_id(parent)
    prepare_seq.value = (tonumber(prepare_seq.value) or 0) + 1
    return tostring(parent.session_uuid) .. ":" .. tostring(prepare_seq.value)
end

local function preview_state_for(parent, extras)
    local plugin_state = parent and parent.plugin_state or nil
    local hosted = type(plugin_state) == "table" and plugin_state[PLUGIN_STATE_KEY] or nil
    local merged = {
        provider = "cloudflare",
        provider_label = "Cloudflare",
        port = session_port(parent),
    }
    if type(hosted) == "table" then
        for k, v in pairs(hosted) do
            merged[k] = v
        end
    end
    if type(extras) == "table" then
        for k, v in pairs(extras) do
            merged[k] = v
        end
    end
    return merged
end

local function update_parent_preview(parent, extras)
    if not parent then return end
    local plugin_state = {}
    if type(parent.plugin_state) == "table" then
        for key, value in pairs(parent.plugin_state) do
            plugin_state[key] = value
        end
    end
    plugin_state[PLUGIN_STATE_KEY] = preview_state_for(parent, extras)
    parent:update({ plugin_state = plugin_state })
end

function M.is_connector(subject)
    local metadata = type(subject) == "table" and subject.metadata or nil
    return type(metadata) == "table"
        and metadata.owner_plugin == PLUGIN_NAME
        and metadata.system_kind == CONNECTOR_SYSTEM_KIND
end

function M.find_connector(parent_uuid)
    if not parent_uuid then return nil end
    for _, session in ipairs(Session.list()) do
        if M.is_connector(session)
            and session:get_meta("target_session_uuid") == parent_uuid
            and session.status ~= "closed" then
            return session
        end
    end
    return nil
end

local function close_connector(connector)
    if not connector then return end
    connector_output_buffers[connector.session_uuid] = nil
    pcall(function()
        connector:close(false)
    end)
end

function M.disable_by_parent_uuid(parent_uuid, opts)
    opts = opts or {}
    local parent = Session.get(parent_uuid)
    if parent and opts.clear_parent ~= false then
        update_parent_preview(parent, {
            status = "inactive",
            error = nil,
            install_url = nil,
            url = nil,
            connector_session_uuid = nil,
            prepare_request_id = false,
        })
    end

    close_connector(M.find_connector(parent_uuid))
end

function M.disable(parent)
    if not parent then return end
    M.disable_by_parent_uuid(parent.session_uuid, { clear_parent = true })
end

local function connector_spec(parent, command, quick_config_path)
    return {
        command = command,
        args = {
            "tunnel",
            "--config",
            quick_config_path,
            "--url",
            "http://127.0.0.1:" .. tostring(session_port(parent)),
            "--no-autoupdate",
        },
    }
end

local function start_connector(parent, prepared)
    local command = prepared and prepared.command or nil
    local quick_config_path = prepared and prepared.config_path or nil
    local request_id = prepared and prepared.request_id or nil
    if type(command) ~= "string" or command == "" then
        local error_message = "Cloudflare hosted preview returned no connector command"
        update_parent_preview(parent, {
            status = "error",
            error = error_message,
            install_url = nil,
            url = nil,
            connector_session_uuid = nil,
            prepare_request_id = false,
        })
        return nil, error_message
    end

    if type(quick_config_path) ~= "string" or quick_config_path == "" then
        local error_message = "Cloudflare hosted preview returned no quick tunnel config path"
        update_parent_preview(parent, {
            status = "error",
            error = error_message,
            install_url = nil,
            url = nil,
            connector_session_uuid = nil,
            prepare_request_id = false,
        })
        return nil, error_message
    end

    if M.find_connector(parent.session_uuid) then
        M.disable_by_parent_uuid(parent.session_uuid, { clear_parent = false })
    end

    local metadata = TargetContext.with_metadata({
        request_id = request_id,
        workspace = parent._workspace_name,
        workspace_id = parent._workspace_id,
        system_kind = CONNECTOR_SYSTEM_KIND,
        owner_plugin = PLUGIN_NAME,
        visibility = "plugin",
        surface = PLUGIN_NAME,
        hosted_preview_provider = "cloudflare",
        target_session_uuid = parent.session_uuid,
        target_forward_port = session_port(parent),
    }, TargetContext.from_session(parent))

    local ok, result = pcall(function()
        return Hub.get():create_accessory({
            request_id = request_id,
            repo = parent.repo,
            target_id = parent.target_id,
            target_path = parent.target_path,
            target_repo = parent.target_repo,
            workspace_id = parent._workspace_id,
            workspace_name = parent._workspace_name,
            agent_name = parent.agent_name,
            metadata = metadata,
            session = {
                name = "cloudflare-preview",
                command = command,
                args = connector_spec(parent, command, quick_config_path).args,
                notifications = false,
                forward_port = false,
            },
        })
    end)

    if not ok then
        local error_message = tostring(result)
        update_parent_preview(parent, {
            status = "error",
            error = error_message,
            install_url = nil,
            url = nil,
            connector_session_uuid = nil,
            prepare_request_id = false,
        })
        return nil, error_message
    end

    local session_uuid = result and result.session_uuid or nil
    if session_uuid then
        connector_output_buffers[session_uuid] = ""
    end
    update_parent_preview(parent, {
        status = "starting",
        error = nil,
        install_url = nil,
        url = nil,
        connector_session_uuid = session_uuid,
        prepare_request_id = session_uuid and false or request_id,
    })
    return result or true
end

function M.enable(parent)
    if not parent then
        return nil, "Parent session is required"
    end
    local port = session_port(parent)
    if not port then
        return nil, "Parent session has no forwarded port"
    end

    if M.find_connector(parent.session_uuid) then
        M.disable_by_parent_uuid(parent.session_uuid, { clear_parent = false })
    end

    local request_id = next_prepare_request_id(parent)
    update_parent_preview(parent, {
        status = "starting",
        error = nil,
        install_url = nil,
        url = nil,
        connector_session_uuid = nil,
        prepare_request_id = request_id,
    })
    hub.prepare_plugin_command({
        request_id = request_id,
        command = cloudflared_command(),
        config_path = quick_tunnel_config_path(),
        config_contents = "{}\n",
        context = {
            parent_session_uuid = parent.session_uuid,
            port = port,
        },
    })
    return true
end

function M.handle_plugin_command_prepared(data)
    local request_id = data and data.request_id or nil
    local context = data and data.context or nil
    local parent_uuid = type(context) == "table" and context.parent_session_uuid or nil
    local parent = parent_uuid and Session.get(parent_uuid) or nil
    if not parent or type(request_id) ~= "string" then
        return false
    end

    local hosted = preview_state_for(parent)
    if hosted.prepare_request_id ~= request_id then
        return false
    end

    if data.error then
        local error_message = tostring(data.error)
        local install_url = nil
        if data.error_kind == "command_missing" then
            error_message = MISSING_BINARY_ERROR
            install_url = CLOUDFLARED_INSTALL_URL
        end
        update_parent_preview(parent, {
            status = "error",
            error = error_message,
            install_url = install_url,
            url = nil,
            connector_session_uuid = nil,
            prepare_request_id = false,
        })
        return true
    end

    return start_connector(parent, {
        command = data.command,
        config_path = data.config_path,
        request_id = request_id,
    }) ~= nil
end

function M.handle_agent_created(info)
    local metadata = info and info.metadata or {}
    if metadata.owner_plugin ~= PLUGIN_NAME
        or metadata.system_kind ~= CONNECTOR_SYSTEM_KIND
        or type(metadata.request_id) ~= "string" then
        return false
    end

    local parent = metadata.target_session_uuid and Session.get(metadata.target_session_uuid) or nil
    local session_uuid = info.session_uuid or info.id
    if not parent or not session_uuid then
        return false
    end

    local hosted = preview_state_for(parent)
    if hosted.prepare_request_id ~= metadata.request_id then
        return false
    end

    connector_output_buffers[session_uuid] = ""
    update_parent_preview(parent, {
        status = "starting",
        error = nil,
        install_url = nil,
        url = nil,
        connector_session_uuid = session_uuid,
        prepare_request_id = false,
    })
    return true
end

function M.handle_output(ctx, data)
    local session_uuid = ctx and ctx.session_uuid or nil
    local connector = session_uuid and Session.get(session_uuid) or nil
    if not connector or not M.is_connector(connector) then
        return false
    end
    if connector:get_meta("preview_url") then
        return true
    end

    local chunk = tostring(data or "")
    local buffer = (connector_output_buffers[session_uuid] or "") .. chunk
    if #buffer > 32768 then
        buffer = buffer:sub(-32768)
    end
    connector_output_buffers[session_uuid] = buffer

    local url, hostname = trycloudflare_url_from_text(buffer)
    if not url then
        return true
    end

    local parent_uuid = connector:get_meta("target_session_uuid")
    local parent = parent_uuid and Session.get(parent_uuid) or nil
    if not parent then
        return true
    end

    local hosted = preview_state_for(parent)
    if hosted.connector_session_uuid ~= connector.session_uuid then
        return true
    end

    if connector:get_meta("preview_url") == url then
        return true
    end
    connector:set_meta("preview_url", url)
    connector:set_meta("preview_hostname", hostname)

    update_parent_preview(parent, {
        status = "starting",
        error = nil,
        install_url = nil,
        url = nil,
        connector_session_uuid = connector.session_uuid,
        prepare_request_id = false,
    })
    hub.probe_url_ready(
        connector.session_uuid,
        parent.session_uuid,
        url,
        hostname,
        PROBE_TIMEOUT_SECS
    )
    return true
end

function M.handle_url_ready(data)
    local connector_uuid = data and data.connector_session_uuid or nil
    local parent_uuid = data and data.parent_session_uuid or nil
    local url = data and data.url or nil
    local connector = connector_uuid and Session.get(connector_uuid) or nil
    local parent = parent_uuid and Session.get(parent_uuid) or nil
    if not connector or not parent or not M.is_connector(connector) then
        return false
    end

    local hosted = preview_state_for(parent)
    if hosted.connector_session_uuid ~= connector.session_uuid then
        return false
    end
    if connector:get_meta("preview_url") ~= url then
        return false
    end

    if data.ready then
        update_parent_preview(parent, {
            status = "running",
            url = url,
            error = nil,
            install_url = nil,
            connector_session_uuid = connector.session_uuid,
            prepare_request_id = false,
        })
    else
        update_parent_preview(parent, {
            status = "error",
            error = data.error or "Preview never became reachable",
            install_url = nil,
            url = nil,
            connector_session_uuid = connector.session_uuid,
            prepare_request_id = false,
        })
    end
    return true
end

function M.handle_process_exited(data)
    local session_uuid = data and data.session_uuid or nil
    local connector = session_uuid and Session.get(session_uuid) or nil
    if not connector or not M.is_connector(connector) then
        return false
    end

    local parent_uuid = connector:get_meta("target_session_uuid")
    local parent = parent_uuid and Session.get(parent_uuid) or nil
    local hosted = parent and preview_state_for(parent) or nil
    local still_owned = parent and type(hosted) == "table"
        and hosted.connector_session_uuid == connector.session_uuid

    local exit_code = data.exit_code
    local error_message = string.format(
        "cloudflared exited%s",
        exit_code ~= nil and (" (code " .. tostring(exit_code) .. ")") or ""
    )

    close_connector(connector)

    if still_owned then
        update_parent_preview(parent, {
            status = "error",
            url = nil,
            error = error_message,
            install_url = nil,
            connector_session_uuid = nil,
            prepare_request_id = false,
        })
    end

    return true
end

function M.handle_session_closing(session)
    if not session then return end

    if M.is_connector(session) then
        connector_output_buffers[session.session_uuid] = nil
        return
    end

    if preview_state_for(session).status then
        M.disable_by_parent_uuid(session.session_uuid, { clear_parent = false })
    end
end

function M.reconcile()
    for _, session in ipairs(Session.list()) do
        if M.is_connector(session) then
            local parent_uuid = session:get_meta("target_session_uuid")
            local parent = parent_uuid and Session.get(parent_uuid) or nil
            if not parent then
                close_connector(session)
            else
                local url = session:get_meta("preview_url")
                local hostname = session:get_meta("preview_hostname")
                if url and hostname then
                    update_parent_preview(parent, {
                        status = "starting",
                        url = nil,
                        error = nil,
                        install_url = nil,
                        connector_session_uuid = session.session_uuid,
                        prepare_request_id = false,
                    })
                    hub.probe_url_ready(
                        session.session_uuid,
                        parent.session_uuid,
                        url,
                        hostname,
                        PROBE_TIMEOUT_SECS
                    )
                else
                    update_parent_preview(parent, {
                        status = "starting",
                        url = nil,
                        error = nil,
                        install_url = nil,
                        connector_session_uuid = session.session_uuid,
                        prepare_request_id = false,
                    })
                end
            end
        end
    end
    for _, session in ipairs(Session.list()) do
        if not M.is_connector(session) then
            local hosted = preview_state_for(session)
            if type(hosted.prepare_request_id) == "string"
                and not hosted.connector_session_uuid then
                update_parent_preview(session, {
                    status = "inactive",
                    error = nil,
                    install_url = nil,
                    url = nil,
                    connector_session_uuid = nil,
                    prepare_request_id = false,
                })
            end
        end
    end
end

local function run_action(session_uuid, _action_id, context)
    local parent = Session.get(session_uuid)
    if not parent then
        return nil, "session not found: " .. tostring(session_uuid)
    end

    local params = context and context.params or {}
    local enabled = params and params.enabled
    if enabled == nil then
        local hosted = preview_state_for(parent)
        enabled = not (type(hosted) == "table"
            and (hosted.status == "starting" or hosted.status == "running"))
    end

    if enabled then
        return M.enable(parent)
    end

    M.disable(parent)
    return true
end

SessionActions.register(ACTION_ID, {
    plugin = PLUGIN_NAME,
    label = function(session)
        local hosted = preview_state_for(session)
        if type(hosted) == "table" and hosted.status == "running" then
            return "Disable Cloudflare preview"
        end
        if type(hosted) == "table" and hosted.status == "error" then
            return "Retry Cloudflare preview"
        end
        if type(hosted) == "table" and hosted.status == "starting" then
            return "Starting Cloudflare preview"
        end
        return "Enable Cloudflare preview"
    end,
    status = function(session)
        local hosted = preview_state_for(session)
        return (type(hosted) == "table" and hosted.status) or "inactive"
    end,
    url = function(session)
        local hosted = preview_state_for(session)
        if type(hosted) == "table" then return hosted.url end
        return nil
    end,
    error = function(session)
        local hosted = preview_state_for(session)
        if type(hosted) == "table" then return hosted.error end
        return nil
    end,
    icon = "globe-alt",
    visibility = function(session)
        return session_port(session) ~= nil
    end,
    enabled = function(session)
        return session_port(session) ~= nil
    end,
    run = run_action,
})

hooks.on("pty_output", PLUGIN_NAME .. ".cloudflared_output", function(ctx, data)
    M.handle_output(ctx, data)
end)

hooks.on("before_agent_close", PLUGIN_NAME .. ".close_connector", function(session)
    M.handle_session_closing(session)
end)

hooks.on("agent_created", PLUGIN_NAME .. ".connector_created", function(info)
    M.handle_agent_created(info)
end)

event_subs[#event_subs + 1] = events.on("url_probe_ready", function(data)
    M.handle_url_ready(data)
end)

event_subs[#event_subs + 1] = events.on("plugin_command_prepared", function(data)
    M.handle_plugin_command_prepared(data)
end)

event_subs[#event_subs + 1] = events.on("process_exited", function(data)
    M.handle_process_exited(data)
end)

M.reconcile()

function M._before_reload()
    SessionActions.unregister(ACTION_ID)
    hooks.off("pty_output", PLUGIN_NAME .. ".cloudflared_output")
    hooks.off("before_agent_close", PLUGIN_NAME .. ".close_connector")
    hooks.off("agent_created", PLUGIN_NAME .. ".connector_created")
    for _, sub in ipairs(event_subs) do
        events.off(sub)
    end
    event_subs = {}
end

return M
