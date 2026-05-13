-- @template Cloudflare Hosted Preview
-- @description Expose port-forwarded sessions with Cloudflare quick tunnels
-- @category plugins
-- @dest plugins/cloudflare-hosted-preview/init.lua
-- @scope device
-- @version 1.0.0

-- Cloudflare hosted-preview session action.
--
-- The Cloudflare quick-tunnel lifecycle is plugin-owned: this module registers
-- a generic session action, owns the hidden connector session, watches
-- cloudflared output, probes URL readiness, and mirrors only a reachable URL
-- onto the parent session.

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
local URL_DISCOVERY_TIMEOUT_SECS = 20.0
local URL_READY_RETRY_SECS = 1.0
local URL_READY_DNS_TIMEOUT_MS = 3000

local connector_output_buffers = state.get("cloudflare_hosted_preview.connector_output_buffers", {})
local connector_records = state.get("cloudflare_hosted_preview.connector_records", {})
local parents_by_uuid = state.get("cloudflare_hosted_preview.parents_by_uuid", {})
local url_wait_ids = state.get("cloudflare_hosted_preview.url_wait_ids", {})
local readiness_wait_ids = state.get("cloudflare_hosted_preview.readiness_wait_ids", {})
local prepare_seq = state.get("cloudflare_hosted_preview.prepare_seq", { value = 0 })
local url_wait_seq = state.get("cloudflare_hosted_preview.url_wait_seq", { value = 0 })
local readiness_seq = state.get("cloudflare_hosted_preview.readiness_seq", { value = 0 })
local event_subs = {}

local M = {}
local close_connector

local function metadata_flag(value)
    return value == true or value == "true"
end

local function session_port(session)
    if type(session) ~= "table" then return nil end
    local port = session._port or session.port
    if port == false or port == 0 or port == "" then return nil end
    return port
end

local function shallow_copy(tbl)
    local out = {}
    if type(tbl) == "table" then
        for key, value in pairs(tbl) do
            out[key] = value
        end
    end
    return out
end

local function cache_parent(parent)
    if parent and parent.session_uuid then
        parents_by_uuid[parent.session_uuid] = parent
    end
    return parent
end

local function cache_connector(session_uuid, metadata, parent)
    if type(session_uuid) ~= "string" or session_uuid == "" then return nil end
    local record = connector_records[session_uuid] or {}
    record.session_uuid = session_uuid
    record.metadata = shallow_copy(metadata or record.metadata)
    record.parent_uuid = record.metadata.target_session_uuid or (parent and parent.session_uuid)
    record.parent = cache_parent(parent) or record.parent or (record.parent_uuid and parents_by_uuid[record.parent_uuid])
    connector_records[session_uuid] = record
    return record
end

local function connector_meta(record, key)
    local metadata = type(record) == "table" and record.metadata or nil
    return type(metadata) == "table" and metadata[key] or nil
end

local function set_connector_meta(record, key, value)
    if type(record) ~= "table" or not record.session_uuid then return false end
    if type(record.metadata) ~= "table" then record.metadata = {} end
    record.metadata[key] = value
    local ok = pcall(function()
        Hub.get():update_session(record.session_uuid, { metadata = record.metadata })
    end)
    return ok
end

local function session_meta(subject, key)
    if type(subject) ~= "table" then return nil end
    if type(subject.get_meta) == "function" then
        local ok, value = pcall(function() return subject:get_meta(key) end)
        if ok then return value end
    end
    local metadata = subject.metadata
    return type(metadata) == "table" and metadata[key] or nil
end

local function normalize_terminal_text(text)
    if type(text) ~= "string" or text == "" then
        return ""
    end

    return text
        :gsub("\27%][^\7]*\7", "")
        :gsub("\27%][^\27]*\27\\", "")
        :gsub("\27%[[0-?]*[ -/]*[@-~]", "")
        :gsub("\r", "\n")
end

local function trycloudflare_url_from_text(text)
    text = normalize_terminal_text(text)
    if text == "" then
        return nil
    end

    local host = text:match("https?://([%w%-]+%.trycloudflare%.com)")
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
    parent.plugin_state = plugin_state
    parent:update({ plugin_state = plugin_state })
end

local function next_url_wait_id(connector)
    url_wait_seq.value = (tonumber(url_wait_seq.value) or 0) + 1
    return tostring(connector.session_uuid) .. ":" .. tostring(url_wait_seq.value)
end

local function next_readiness_wait_id(connector)
    readiness_seq.value = (tonumber(readiness_seq.value) or 0) + 1
    return tostring(connector.session_uuid) .. ":" .. tostring(readiness_seq.value)
end

local function mark_preview_running(connector, parent, url, hostname)
    if not connector or not parent then
        return false
    end

    set_connector_meta(connector, "preview_url", url)
    set_connector_meta(connector, "preview_hostname", hostname)
    set_connector_meta(connector, "preview_pending_url", false)
    url_wait_ids[connector.session_uuid] = nil
    readiness_wait_ids[connector.session_uuid] = nil
    update_parent_preview(parent, {
        status = "running",
        error = false,
        install_url = false,
        url = url,
        connector_session_uuid = connector.session_uuid,
        prepare_request_id = false,
    })
    return true
end

local function parent_allows_readiness_result(parent, connector)
    local hosted = preview_state_for(parent)
    local current_uuid = hosted.connector_session_uuid
    if current_uuid == connector.session_uuid then
        return true
    end
    if current_uuid == nil and hosted.status == nil then
        return true
    end
    return false
end

local reconcile_connector_session

local function preview_readiness_still_current(connector_uuid, parent_uuid, url, wait_id)
    local connector = connector_uuid and connector_records[connector_uuid] or nil
    local parent = connector and connector.parent or (parent_uuid and parents_by_uuid[parent_uuid]) or nil
    if not connector or not parent or not M.is_connector(connector) then
        return nil, nil, false
    end

    local current = parent_allows_readiness_result(parent, connector)
        and connector_meta(connector, "preview_pending_url") == url
        and readiness_wait_ids[connector.session_uuid] == wait_id
    return connector, parent, current
end

local function dns_ready_from_response(resp)
    if not resp or tonumber(resp.status) ~= 200 or type(resp.body) ~= "string" then
        return false
    end

    local ok, body = pcall(json.decode, resp.body)
    if not ok or type(body) ~= "table" or tonumber(body.Status) ~= 0 then
        return false
    end

    local answers = body.Answer
    if type(answers) ~= "table" then
        return false
    end

    for _, answer in ipairs(answers) do
        local answer_type = type(answer) == "table" and tonumber(answer.type) or nil
        if answer_type == 1 or answer_type == 28 then
            return true
        end
    end
    return false
end

local function begin_preview_readiness(connector, parent, url, hostname)
    if not connector or not parent or type(url) ~= "string" or url == "" then
        return false
    end

    local wait_id = next_readiness_wait_id(connector)
    url_wait_ids[connector.session_uuid] = nil
    readiness_wait_ids[connector.session_uuid] = wait_id
    set_connector_meta(connector, "preview_pending_url", url)
    set_connector_meta(connector, "preview_hostname", hostname)
    update_parent_preview(parent, {
        status = "starting",
        error = false,
        install_url = false,
        url = false,
        connector_session_uuid = connector.session_uuid,
        prepare_request_id = false,
    })

    local function attempt()
        local current_connector, current_parent, current =
            preview_readiness_still_current(connector.session_uuid, parent.session_uuid, url, wait_id)
        if not current then
            return
        end

        local _, request_err = http.request({
            method = "GET",
            url = "https://cloudflare-dns.com/dns-query?name=" .. hostname .. "&type=A",
            headers = {
                ["Accept"] = "application/dns-json",
            },
            timeout_ms = URL_READY_DNS_TIMEOUT_MS,
        }, function(resp, err)
            local still_connector, still_parent, still_current =
                preview_readiness_still_current(connector.session_uuid, parent.session_uuid, url, wait_id)
            if not still_current then
                return
            end

            if dns_ready_from_response(resp) then
                mark_preview_running(still_connector, still_parent, url, hostname)
                return
            end

            timer.after(URL_READY_RETRY_SECS, attempt)
        end)

        if request_err then
            timer.after(URL_READY_RETRY_SECS, attempt)
        end
    end

    attempt()
    return true
end

local function url_wait_still_current(connector_uuid, parent_uuid, wait_id)
    local connector = connector_uuid and connector_records[connector_uuid] or nil
    local parent = connector and connector.parent or (parent_uuid and parents_by_uuid[parent_uuid]) or nil
    if not connector or not parent or not M.is_connector(connector) then
        return nil, nil, false
    end

    local hosted = preview_state_for(parent)
    local current = hosted.connector_session_uuid == connector.session_uuid
        and not connector_meta(connector, "preview_url")
        and url_wait_ids[connector.session_uuid] == wait_id
    return connector, parent, current
end

local function finish_url_discovery_timeout(connector_uuid, parent_uuid, wait_id)
    local connector, parent, current = url_wait_still_current(connector_uuid, parent_uuid, wait_id)
    if not current then
        return false
    end

    url_wait_ids[connector.session_uuid] = nil
    update_parent_preview(parent, {
        status = "error",
        error = "Cloudflare quick tunnel did not emit a preview URL",
        install_url = false,
        url = false,
        connector_session_uuid = false,
        prepare_request_id = false,
    })
    close_connector(connector)
    return true
end

local function schedule_url_discovery_timeout(connector, parent)
    if not connector or not parent then
        return false
    end
    if connector_meta(connector, "preview_url") then
        return true
    end
    if url_wait_ids[connector.session_uuid] then
        return true
    end

    local wait_id = next_url_wait_id(connector)
    url_wait_ids[connector.session_uuid] = wait_id
    timer.after(URL_DISCOVERY_TIMEOUT_SECS, function()
        finish_url_discovery_timeout(connector.session_uuid, parent.session_uuid, wait_id)
    end)
    return true
end

function M.is_connector(subject)
    local metadata = type(subject) == "table" and subject.metadata or nil
    return type(metadata) == "table"
        and metadata_flag(metadata.system_session)
        and metadata.system_kind == CONNECTOR_SYSTEM_KIND
end

function M.find_connector(parent_uuid)
    local connectors = M.find_connectors(parent_uuid)
    return connectors and connectors[1] or nil
end

function M.find_connectors(parent_uuid)
    local connectors = {}
    if not parent_uuid then return connectors end
    for _, session in ipairs(Session.list()) do
        if M.is_connector(session)
            and session:get_meta("target_session_uuid") == parent_uuid
            and session.status ~= "closed" then
            connectors[#connectors + 1] = session
        end
    end
    return connectors
end

close_connector = function(connector)
    if not connector then return end
    local session_uuid = connector.session_uuid
    connector_output_buffers[session_uuid] = nil
    connector_records[session_uuid] = nil
    url_wait_ids[session_uuid] = nil
    readiness_wait_ids[session_uuid] = nil
    pcall(function()
        if type(connector.close) == "function" then
            connector:close(false)
        elseif session_uuid then
            local session = Session.get(session_uuid)
            if session and type(session.close) == "function" then
                session:close(false)
            end
        end
    end)
end

local function close_connectors_for_parent(parent_uuid)
    local closed = {}
    for _, connector in ipairs(M.find_connectors(parent_uuid) or {}) do
        closed[connector.session_uuid] = true
        close_connector(connector)
    end
    for session_uuid, record in pairs(connector_records) do
        if not closed[session_uuid] and connector_meta(record, "target_session_uuid") == parent_uuid then
            close_connector(record)
        end
    end
end

function M.disable_by_parent_uuid(parent_uuid, opts)
    opts = opts or {}
    local parent = Session.get(parent_uuid)
    if parent and opts.clear_parent ~= false then
        update_parent_preview(parent, {
            status = "inactive",
            error = false,
            install_url = false,
            url = false,
            connector_session_uuid = false,
            prepare_request_id = false,
        })
    end

    close_connectors_for_parent(parent_uuid)
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
            install_url = false,
            url = false,
            connector_session_uuid = false,
            prepare_request_id = false,
        })
        return nil, error_message
    end

    if type(quick_config_path) ~= "string" or quick_config_path == "" then
        local error_message = "Cloudflare hosted preview returned no quick tunnel config path"
        update_parent_preview(parent, {
            status = "error",
            error = error_message,
            install_url = false,
            url = false,
            connector_session_uuid = false,
            prepare_request_id = false,
        })
        return nil, error_message
    end

    if #(M.find_connectors(parent.session_uuid) or {}) > 0 then
        M.disable_by_parent_uuid(parent.session_uuid, { clear_parent = false })
    end

    local metadata = TargetContext.with_metadata({
        request_id = request_id,
        workspace = parent._workspace_name,
        workspace_id = parent._workspace_id,
        system_session = true,
        system_kind = CONNECTOR_SYSTEM_KIND,
        owner_plugin = PLUGIN_NAME,
        hosted_preview_provider = "cloudflare",
        target_session_uuid = parent.session_uuid,
        target_forward_port = session_port(parent),
        observe_output = true,
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
            install_url = false,
            url = false,
            connector_session_uuid = false,
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
        error = false,
        install_url = false,
        url = false,
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

    if #(M.find_connectors(parent.session_uuid) or {}) > 0 then
        M.disable_by_parent_uuid(parent.session_uuid, { clear_parent = false })
    end

    local request_id = next_prepare_request_id(parent)
    cache_parent(parent)
    update_parent_preview(parent, {
        status = "starting",
        error = false,
        install_url = false,
        url = false,
        connector_session_uuid = false,
        prepare_request_id = request_id,
    })
    Hub.get():prepare_plugin_command({
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
    local parent = parent_uuid and (parents_by_uuid[parent_uuid] or Session.get(parent_uuid)) or nil
    cache_parent(parent)
    if not parent or type(request_id) ~= "string" then
        return false
    end

    local hosted = preview_state_for(parent)
    if hosted.prepare_request_id ~= request_id then
        return false
    end

    if data.error then
        local error_message = tostring(data.error)
        local install_url = false
        if data.error_kind == "command_missing" then
            error_message = MISSING_BINARY_ERROR
            install_url = CLOUDFLARED_INSTALL_URL
        end
        update_parent_preview(parent, {
            status = "error",
            error = error_message,
            install_url = install_url,
            url = false,
            connector_session_uuid = false,
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

    local parent = metadata.target_session_uuid
        and (parents_by_uuid[metadata.target_session_uuid] or Session.get(metadata.target_session_uuid))
        or nil
    local session_uuid = info.session_uuid or info.id
    if not parent or not session_uuid then
        return false
    end

    local hosted = preview_state_for(parent)
    if hosted.prepare_request_id ~= metadata.request_id then
        if hosted.status == nil or hosted.connector_session_uuid == session_uuid then
            return reconcile_connector_session({
                session_uuid = session_uuid,
                metadata = metadata,
                get_meta = function(self, key) return self.metadata and self.metadata[key] end,
            }, parent)
        end
        return false
    end

    connector_output_buffers[session_uuid] = ""
    local connector = cache_connector(session_uuid, metadata, parent)
    update_parent_preview(parent, {
        status = "starting",
        error = false,
        install_url = false,
        url = false,
        connector_session_uuid = session_uuid,
        prepare_request_id = false,
    })
    if connector then
        schedule_url_discovery_timeout(connector, parent)
    end
    return true
end

function M.handle_output(ctx, data)
    local session_uuid = ctx and ctx.session_uuid or nil
    local connector = session_uuid and connector_records[session_uuid] or nil
    if not connector and ctx and type(ctx.metadata) == "table" then
        local parent_uuid = ctx.metadata.target_session_uuid
        local parent = parent_uuid and parents_by_uuid[parent_uuid] or nil
        connector = cache_connector(session_uuid, ctx.metadata, parent)
    end
    if not connector or not M.is_connector(connector) then
        return false
    end
    if connector_meta(connector, "preview_url") then
        return true
    end
    if connector_meta(connector, "preview_pending_url") then
        return true
    end

    local parent_uuid = connector_meta(connector, "target_session_uuid")
    local parent = connector.parent or (parent_uuid and parents_by_uuid[parent_uuid]) or nil
    if parent then
        schedule_url_discovery_timeout(connector, parent)
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

    if not parent then
        return true
    end

    local hosted = preview_state_for(parent)
    if hosted.connector_session_uuid ~= connector.session_uuid then
        return true
    end

    if connector_meta(connector, "preview_url") == url then
        return true
    end
    begin_preview_readiness(connector, parent, url, hostname)
    return true
end

function M.handle_process_exited(data)
    local session_uuid = data and data.session_uuid or nil
    local connector = session_uuid and (connector_records[session_uuid] or Session.get(session_uuid)) or nil
    if not connector or not M.is_connector(connector) then
        return false
    end

    local parent_uuid = connector_meta(connector, "target_session_uuid")
        or (type(connector.get_meta) == "function" and connector:get_meta("target_session_uuid"))
    local parent = connector.parent or (parent_uuid and (parents_by_uuid[parent_uuid] or Session.get(parent_uuid))) or nil
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
            url = false,
            error = error_message,
            install_url = false,
            connector_session_uuid = false,
            prepare_request_id = false,
        })
    end

    return true
end

function M.handle_session_closing(session)
    if not session then return end

    if M.is_connector(session) then
        connector_output_buffers[session.session_uuid] = nil
        connector_records[session.session_uuid] = nil
        url_wait_ids[session.session_uuid] = nil
        readiness_wait_ids[session.session_uuid] = nil
        return
    end

    if preview_state_for(session).status then
        M.disable_by_parent_uuid(session.session_uuid, { clear_parent = false })
    end
end

reconcile_connector_session = function(session, parent)
    if not session or not parent then
        return false
    end

    cache_parent(parent)
    local hosted = preview_state_for(parent)
    if hosted.status == "error" or hosted.status == "inactive" then
        close_connector(session)
        return true
    end
    if type(hosted.connector_session_uuid) == "string"
        and hosted.connector_session_uuid ~= session.session_uuid then
        close_connector(session)
        return true
    end

    local connector = cache_connector(session.session_uuid, session.metadata, parent)
    local url = session_meta(session, "preview_url")
    local hostname = session_meta(session, "preview_hostname")
    if url and hostname then
        return mark_preview_running(connector, parent, url, hostname)
    end

    local pending_url = session_meta(session, "preview_pending_url")
    if pending_url and hostname then
        return begin_preview_readiness(connector, parent, pending_url, hostname)
    end

    connector_output_buffers[session.session_uuid] =
        connector_output_buffers[session.session_uuid] or ""
    update_parent_preview(parent, {
        status = "starting",
        url = false,
        error = false,
        install_url = false,
        connector_session_uuid = session.session_uuid,
        prepare_request_id = false,
    })
    schedule_url_discovery_timeout(connector, parent)
    return true
end

function M.reconcile()
    for _, session in ipairs(Session.list()) do
        if M.is_connector(session) then
            local parent_uuid = session:get_meta("target_session_uuid")
            local parent = parent_uuid and Session.get(parent_uuid) or nil
            if not parent then
                close_connector(session)
            else
                reconcile_connector_session(session, parent)
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
                    error = false,
                    install_url = false,
                    url = false,
                    connector_session_uuid = false,
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

hooks.on("agent_created", PLUGIN_NAME .. ".connector_created", function(info)
    M.handle_agent_created(info)
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
    hooks.off("agent_created", PLUGIN_NAME .. ".connector_created")
    for _, sub in ipairs(event_subs) do
        events.off(sub)
    end
    event_subs = {}
end

return M
