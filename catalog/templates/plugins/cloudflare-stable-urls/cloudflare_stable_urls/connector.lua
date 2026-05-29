-- @template Cloudflare Stable URLs
-- @description Hub-level Cloudflare named-tunnel connector lifecycle
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/cloudflare_stable_urls/connector.lua
-- @scope device
-- @version 1.0.0

local Hub = require("lib.hub")
local Session = require("lib.session")
local repo = require("cloudflare_stable_urls.repo")
local entities = require("cloudflare_stable_urls.entities")
local contract = require("cloudflare_stable_urls.entity_contract")

local PLUGIN_NAME = contract.owner
local CONNECTOR_SYSTEM_KIND = "cloudflare_stable_urls_connector"
local CLOUDFLARED_INSTALL_URL =
    "https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/"
local MISSING_BINARY_ERROR = "Cloudflare stable URLs require cloudflared to be installed on this machine."
local MAX_RETRIES = 3
local RETRY_DELAY_SECS = 5

local M = {}
local pending_prepares = {}
local reconcile_in_flight = false

local function now()
    return os.time()
end

local function trim_slash(value)
    return tostring(value or ""):gsub("/+$", "")
end

local function cloudflared_command()
    local override = os.getenv("BOTSTER_CLOUDFLARED_BIN")
    if type(override) == "string" and override:match("%S") then return override end
    return "cloudflared"
end

local function data_dir()
    local root = config and config.data_dir and config.data_dir() or "/tmp"
    return trim_slash(root) .. "/plugin-data/" .. PLUGIN_NAME
end

local function sanitize(value)
    return tostring(value or ""):gsub("[^%w%-_]", "_")
end

local function token_secret_key(token_version)
    return "connector_token_v" .. sanitize(token_version or "0")
end

local function token_path(token_version)
    return data_dir() .. "/runtime/token-" .. sanitize(token_version or "0")
end

local function command_args(tunnel)
    local name = tunnel.cloudflare_tunnel_name or tunnel.name or tunnel.cloudflare_tunnel_id or tunnel.id
    return { "tunnel", "run", "--token-file", token_path(tunnel.token_version), tostring(name) }
end

local function publish_connector(status, message, extras)
    local attrs = extras or {}
    attrs.status = status
    attrs.message = message
    attrs.updated_at = now()
    local record = repo.save_connector(attrs)
    if status == "unhealthy" or status == "reconciling" or status == "error" then
        repo.mark_claims_status(status == "error" and "unhealthy" or status, message)
    end
    pcall(entities.snapshot)
    return record
end

local function decode_json(raw)
    local ok, decoded = pcall(json.decode, raw or "")
    if ok and type(decoded) == "table" then return decoded end
    return nil
end

local function tunnel_payload(resp)
    if not resp or tonumber(resp.status) < 200 or tonumber(resp.status) >= 300 then
        return nil, "Rails Cloudflare tunnel broker returned status " .. tostring(resp and resp.status)
    end
    local decoded = decode_json(resp.body)
    local tunnel = decoded and decoded.cloudflare_tunnel
    if type(tunnel) ~= "table" then
        return nil, "Rails Cloudflare tunnel broker response did not include cloudflare_tunnel"
    end
    if type(tunnel.connector_token) ~= "string" or tunnel.connector_token == "" then
        return nil, "Rails Cloudflare tunnel broker response did not include connector_token"
    end
    return tunnel
end

local function store_token(tunnel)
    local key = token_secret_key(tunnel.token_version)
    local ok, err = secrets.set(PLUGIN_NAME, key, tunnel.connector_token)
    if not ok then return nil, err end
    return key
end

local function materialize_runtime_files(tunnel, secret_key)
    local token, token_err = secrets.get(PLUGIN_NAME, secret_key)
    if not token then return nil, token_err or "connector token secret missing" end

    local ok, err = fs.write_private(token_path(tunnel.token_version), token)
    if not ok then return nil, err end
    return true
end

local function live_owned_connectors()
    local seen = {}
    local out = {}
    local function add(session)
        local metadata = session and session.metadata or {}
        if metadata.owner_plugin ~= PLUGIN_NAME or metadata.system_kind ~= CONNECTOR_SYSTEM_KIND then return end
        local uuid = session.session_uuid or session.id
        if not uuid or seen[uuid] then return end
        if session.status == "closed" then return end
        seen[uuid] = true
        out[#out + 1] = session
    end
    for _, session in ipairs(Hub.get():list_owned_sessions(PLUGIN_NAME) or {}) do add(session) end
    for _, session in ipairs(Session.list and Session.list() or {}) do add(session) end
    return out
end

local function close_session(session)
    if not session then return end
    pcall(function()
        if type(session.close) == "function" then
            session:close(false)
            return
        end
        local uuid = session.session_uuid or session.id
        local real = uuid and Session.get and Session.get(uuid)
        if real and type(real.close) == "function" then real:close(false) end
    end)
end

local function reconcile_live_connectors(state)
    local generation = tonumber(state.connector_generation) or 0
    local current = nil
    for _, session in ipairs(live_owned_connectors()) do
        local metadata = session.metadata or {}
        local session_generation = tonumber(metadata.connector_generation) or 0
        if session_generation == generation and not current then
            current = session
        else
            close_session(session)
        end
    end
    return current
end

local function spawn_connector(state, tunnel, reason)
    local request_id = PLUGIN_NAME .. ":" .. tostring(now()) .. ":" .. tostring(state.connector_generation or 0)
    pending_prepares[request_id] = {
        tunnel = tunnel,
        generation = state.connector_generation,
        reason = reason,
    }
    Hub.get():prepare_plugin_command({
        request_id = request_id,
        command = cloudflared_command(),
        context = {
            owner_plugin = PLUGIN_NAME,
            connector_generation = state.connector_generation,
            reason = reason,
        },
    })
    return true
end

local function schedule_retry(reason)
    local state = repo.connector() or {}
    local retries = tonumber(state.retry_count) or 0
    if retries >= MAX_RETRIES then return false end
    repo.save_connector({ retry_count = retries + 1, status = "reconciling", message = reason })
    if timer and timer.after then
        timer.after(RETRY_DELAY_SECS, function()
            M.reconcile("retry")
        end)
        return true
    end
    return false
end

function M.reconcile(reason)
    reason = reason or "reconcile"
    if reconcile_in_flight then
        return false
    end

    reconcile_in_flight = true
    local function finish_reconcile()
        reconcile_in_flight = false
    end

    publish_connector("reconciling", "Reconciling Cloudflare stable connector (" .. reason .. ")")

    if hub and hub.is_offline and hub.is_offline() then
        publish_connector("unhealthy", "Hub is offline; Cloudflare stable connector is paused")
        finish_reconcile()
        return false
    end

    local server_url = config and config.server_url and config.server_url()
    local api_token = hub and hub.api_token and hub.api_token()
    local hub_id = hub and hub.hub_id and hub.hub_id()
    if not server_url or not api_token or not hub_id then
        publish_connector("unhealthy", "Missing hub server URL, API token, or hub id")
        finish_reconcile()
        return false
    end

    local completed = false
    local request_id = http.request({
        method = "POST",
        url = trim_slash(server_url) .. "/hubs/" .. tostring(hub_id) .. "/cloudflare_tunnel",
        headers = {
            ["Authorization"] = "Bearer " .. tostring(api_token),
            ["Accept"] = "application/json",
            ["Content-Type"] = "application/json",
        },
        body = "{}",
        timeout_ms = 30000,
    }, function(resp, err)
        completed = true
        finish_reconcile()
        if err then
            publish_connector("unhealthy", tostring(err))
            schedule_retry(tostring(err))
            return
        end
        local tunnel, tunnel_err = tunnel_payload(resp)
        if not tunnel then
            publish_connector("unhealthy", tunnel_err)
            schedule_retry(tunnel_err)
            return
        end

        local secret_key, secret_err = store_token(tunnel)
        if not secret_key then
            publish_connector("unhealthy", secret_err)
            return
        end

        local generation = tonumber(tunnel.token_version) or ((tonumber((repo.connector() or {}).connector_generation) or 0) + 1)
        local state = repo.save_connector({
            cloudflare_tunnel_id = tunnel.cloudflare_tunnel_id,
            cloudflare_tunnel_name = tunnel.cloudflare_tunnel_name,
            token_version = tunnel.token_version,
            token_secret_key = secret_key,
            token_path = token_path(tunnel.token_version),
            config_path = false,
            connector_generation = generation,
            status = "reconciling",
            message = "Broker material received",
            retry_count = 0,
        })

        local ok, file_err = materialize_runtime_files(tunnel, secret_key)
        if not ok then
            publish_connector("unhealthy", file_err)
            return
        end

        local live = reconcile_live_connectors(state)
        if live then
            repo.mark_claims_status("claimed", "Cloudflare stable connector is running")
            repo.save_connector({
                connector_session_uuid = live.session_uuid or live.id,
                status = "running",
                message = "Cloudflare stable connector is running",
                retry_count = 0,
            })
            entities.snapshot()
            return
        end

        spawn_connector(state, tunnel, reason)
    end)
    if completed then finish_reconcile() end
    return request_id
end

function M.handle_plugin_command_prepared(data)
    local request_id = data and data.request_id
    local pending = request_id and pending_prepares[request_id]
    if not pending then return false end
    pending_prepares[request_id] = nil

    if data.error then
        local error_message = tostring(data.error)
        local install_url = false
        if data.error_kind == "command_missing" then
            error_message = MISSING_BINARY_ERROR
            install_url = CLOUDFLARED_INSTALL_URL
        end
        publish_connector("unhealthy", error_message, { message = error_message })
        schedule_retry(error_message)
        return true, install_url
    end

    local state = repo.connector() or {}
    if tonumber(state.connector_generation) ~= tonumber(pending.generation) then
        return false
    end

    local ok, result = pcall(function()
        return Hub.get():create_accessory({
            request_id = request_id,
            metadata = {
                request_id = request_id,
                system_session = true,
                system_kind = CONNECTOR_SYSTEM_KIND,
                owner_plugin = PLUGIN_NAME,
                connector_generation = pending.generation,
                observe_output = false,
            },
            session = {
                name = "cloudflare-stable-urls",
                command = data.command,
                args = command_args(pending.tunnel),
                notifications = false,
                forward_port = false,
            },
        })
    end)
    if not ok then
        publish_connector("unhealthy", tostring(result))
        schedule_retry(tostring(result))
        return true
    end
    repo.save_connector({
        connector_session_uuid = result and result.session_uuid,
        status = "starting",
        message = "Cloudflare stable connector starting",
        retry_count = 0,
    })
    entities.snapshot()
    return true
end

function M.handle_agent_created(info)
    local metadata = info and info.metadata or {}
    if metadata.owner_plugin ~= PLUGIN_NAME or metadata.system_kind ~= CONNECTOR_SYSTEM_KIND then
        return false
    end
    local state = repo.connector() or {}
    if tonumber(metadata.connector_generation) ~= tonumber(state.connector_generation) then
        return false
    end
    repo.save_connector({
        connector_session_uuid = info.session_uuid or info.id,
        status = "running",
        message = "Cloudflare stable connector is running",
        retry_count = 0,
    })
    repo.mark_claims_status("claimed", "Cloudflare stable connector is running")
    entities.snapshot()
    return true
end

function M.handle_process_exited(data)
    local session_uuid = data and data.session_uuid
    local state = repo.connector() or {}
    if type(session_uuid) ~= "string" or session_uuid ~= state.connector_session_uuid then
        return false
    end
    local exit_code = data.exit_code
    local message = "cloudflared exited" .. (exit_code ~= nil and (" (code " .. tostring(exit_code) .. ")") or "")
    repo.save_connector({
        connector_session_uuid = false,
        status = "unhealthy",
        message = message,
    })
    repo.mark_claims_status("unhealthy", message)
    entities.snapshot()
    schedule_retry(message)
    return true
end

function M.claim(attrs)
    local row, err = repo.upsert_claim(attrs)
    if row then entities.upsert(row) end
    return row, err
end

function M.list()
    return repo.list_claims()
end

return M
