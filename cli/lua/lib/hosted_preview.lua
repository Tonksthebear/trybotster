-- Hosted preview orchestration via hidden accessory sessions.
--
-- Runs cloudflared through the normal session/accessory PTY path instead of a
-- hub-owned background child process. This reuses the execution model already
-- proven to work when cloudflared is launched manually inside Botster.
--
-- Readiness model: cloudflared prints the URL before the tunnel is fully
-- reachable. We keep the parent session in "starting" until DNS resolves and
-- HTTPS responds, then surface the hosted URL. Process exit catches tunnel
-- failures after startup.

local state = require("hub.state")
local Accessory = require("lib.accessory")
local Session = require("lib.session")
local TargetContext = require("lib.target_context")

local M = {}

local connector_output_buffers = state.get("hosted_preview.connector_output_buffers", {})
local CLOUDFLARED_INSTALL_URL =
    "https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/"
local MISSING_BINARY_ERROR =
    "Hosted preview requires cloudflared to be installed on this machine."

local function metadata_flag(value)
    return value == true or value == "true"
end

local function preview_state_for(parent, extras)
    local hosted = parent and parent.hosted_preview or nil
    local merged = {
        provider = "cloudflare",
        port = parent and parent._port or nil,
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

local function resolve_cloudflared_binary()
    local override = os.getenv("BOTSTER_CLOUDFLARED_BIN")
    if type(override) == "string" and override:match("%S") then
        local resolved = hub.resolve_command_path(override)
        if resolved then
            return resolved
        end
        return nil
    end

    return hub.resolve_command_path("cloudflared")
end

function M.is_system_session(subject)
    local metadata = type(subject) == "table" and subject.metadata or nil
    return type(metadata) == "table" and metadata_flag(metadata.system_session)
end

function M.is_connector(subject)
    local metadata = type(subject) == "table" and subject.metadata or nil
    return M.is_system_session(subject)
        and type(metadata) == "table"
        and metadata.system_kind == "hosted_preview_connector"
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
        parent:update({
            hosted_preview = preview_state_for(parent, {
                status = "inactive",
                error = nil,
                install_url = nil,
                url = nil,
                connector_session_uuid = nil,
            }),
        })
    end

    local connector = M.find_connector(parent_uuid)
    if connector then
        close_connector(connector)
    end
end

function M.disable(parent)
    if not parent then return end
    M.disable_by_parent_uuid(parent.session_uuid, { clear_parent = true })
end

function M.enable(parent)
    if not parent then
        return nil, "Parent session is required"
    end
    if not parent._port then
        return nil, "Parent session has no forwarded port"
    end

    local cloudflared_bin = resolve_cloudflared_binary()
    if not cloudflared_bin then
        parent:update({
            hosted_preview = preview_state_for(parent, {
                status = "error",
                error = MISSING_BINARY_ERROR,
                install_url = CLOUDFLARED_INSTALL_URL,
                url = nil,
                connector_session_uuid = nil,
            }),
        })
        return nil, MISSING_BINARY_ERROR
    end

    local existing = M.find_connector(parent.session_uuid)
    if existing then
        M.disable_by_parent_uuid(parent.session_uuid, { clear_parent = false })
    end

    local metadata = TargetContext.with_metadata({
        workspace = parent._workspace_name,
        workspace_id = parent._workspace_id,
        system_session = true,
        system_kind = "hosted_preview_connector",
        target_session_uuid = parent.session_uuid,
        target_forward_port = parent._port,
    }, TargetContext.from_session(parent))

    local ok, connector = pcall(Accessory.new, {
        repo = parent.repo,
        branch_name = parent.branch_name,
        worktree_path = parent.worktree_path,
        session = {
            name = "hosted-preview",
            command = cloudflared_bin,
            args = {
                "tunnel",
                "--url",
                "http://127.0.0.1:" .. tostring(parent._port),
                "--no-autoupdate",
            },
            notifications = false,
            forward_port = false,
        },
        metadata = metadata,
        target_id = parent.target_id,
        target_path = parent.target_path,
        target_repo = parent.target_repo,
        workspace = parent._workspace_name,
        workspace_id = parent._workspace_id,
        dims = { rows = 24, cols = 80 },
        agent_name = parent.agent_name,
    })

    if not ok then
        local error_message = tostring(connector)
        parent:update({
            hosted_preview = preview_state_for(parent, {
                status = "error",
                error = error_message,
                install_url = nil,
                url = nil,
                connector_session_uuid = nil,
            }),
        })
        return nil, error_message
    end

    connector_output_buffers[connector.session_uuid] = ""
    parent:update({
        hosted_preview = preview_state_for(parent, {
            status = "starting",
            error = nil,
            install_url = nil,
            url = nil,
            connector_session_uuid = connector.session_uuid,
        }),
    })
    return connector
end

function M.handle_output(ctx, data)
    local session_uuid = ctx and ctx.session_uuid or nil
    if not session_uuid then
        return false
    end

    local connector = Session.get(session_uuid)
    if not connector or not M.is_connector(connector) then
        return false
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

    local hosted = parent.hosted_preview or {}
    if hosted.connector_session_uuid ~= connector.session_uuid then
        return true
    end

    -- Already probing or running for this URL — skip
    if connector:get_meta("preview_url") == url then
        return true
    end
    connector:set_meta("preview_url", url)

    -- Stay in "starting" until the probe confirms the hosted URL is live.
    parent:update({
        hosted_preview = preview_state_for(parent, {
            status = "starting",
            error = nil,
            install_url = nil,
            url = nil,
            connector_session_uuid = connector.session_uuid,
        }),
    })
    hub.probe_preview_dns(connector.session_uuid, parent.session_uuid, url, hostname, 15.0)
    return true
end

function M.handle_dns_ready(data)
    local connector_uuid = data and data.connector_session_uuid or nil
    local parent_uuid = data and data.parent_session_uuid or nil
    local url = data and data.url or nil
    local connector = connector_uuid and Session.get(connector_uuid) or nil
    local parent = parent_uuid and Session.get(parent_uuid) or nil
    if not connector or not parent or not M.is_connector(connector) then
        return false
    end

    local hosted = parent.hosted_preview or {}
    if hosted.connector_session_uuid ~= connector.session_uuid then
        return false
    end
    if connector:get_meta("preview_url") ~= url then
        return false
    end

    if data.ready then
        parent:update({
            hosted_preview = preview_state_for(parent, {
                status = "running",
                url = url,
                error = nil,
                install_url = nil,
                connector_session_uuid = connector.session_uuid,
            }),
        })
    else
        parent:update({
            hosted_preview = preview_state_for(parent, {
                status = "error",
                error = data.error or "Preview never became reachable",
                install_url = nil,
                url = nil,
                connector_session_uuid = connector.session_uuid,
            }),
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
    local hosted = parent and parent.hosted_preview or nil
    local still_owned = parent and type(hosted) == "table"
        and hosted.connector_session_uuid == connector.session_uuid

    local exit_code = data.exit_code
    local error_message = string.format(
        "cloudflared exited%s",
        exit_code ~= nil and (" (code " .. tostring(exit_code) .. ")") or ""
    )

    close_connector(connector)

    if still_owned then
        parent:update({
            hosted_preview = preview_state_for(parent, {
                status = "error",
                url = nil,
                error = error_message,
                install_url = nil,
                connector_session_uuid = nil,
            }),
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

    if session.hosted_preview then
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
                if url then
                    -- Re-probe on recovery — DNS cache state is unknown
                    local hostname = url:match("https://([%w%-]+%.trycloudflare%.com)")
                    if hostname then
                        parent:update({
                            hosted_preview = preview_state_for(parent, {
                                status = "starting",
                                url = nil,
                                error = nil,
                                install_url = nil,
                                connector_session_uuid = session.session_uuid,
                            }),
                        })
                        hub.probe_preview_dns(session.session_uuid, parent.session_uuid, url, hostname, 15.0)
                    end
                else
                    parent:update({
                        hosted_preview = preview_state_for(parent, {
                            status = "starting",
                            url = nil,
                            error = nil,
                            install_url = nil,
                            connector_session_uuid = session.session_uuid,
                        }),
                    })
                end
            end
        end
    end
end

return M
