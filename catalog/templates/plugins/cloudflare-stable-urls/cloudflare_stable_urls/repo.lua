-- @template Cloudflare Stable URLs
-- @description Repository helpers for the Cloudflare stable URL connector plugin
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/cloudflare_stable_urls/repo.lua
-- @scope device
-- @version 1.0.0

local db = require("cloudflare_stable_urls.db")

local M = {}

local CONNECTOR_ID = "hub"

local function now()
    return os.time()
end

local function rows(sql, ...)
    local params = { ... }
    local result
    if #params == 0 then
        result = db:eval(sql)
    elseif #params == 1 then
        result = db:eval(sql, params[1])
    else
        result = db:eval(sql, params)
    end
    if type(result) == "table" then return result end
    return {}
end

local function exec(sql, ...)
    local params = { ... }
    if #params == 0 then
        return db:eval(sql)
    elseif #params == 1 then
        return db:eval(sql, params[1])
    end
    return db:eval(sql, params)
end

local function value(row, key, fallback)
    if row and row[key] ~= nil then return row[key] end
    return fallback
end

function M.connector()
    return rows("SELECT * FROM connector_state WHERE id = ? LIMIT 1", CONNECTOR_ID)[1]
end

function M.save_connector(attrs)
    attrs = attrs or {}
    local existing = M.connector() or {}
    local record = {
        id = CONNECTOR_ID,
        cloudflare_tunnel_id = value(attrs, "cloudflare_tunnel_id", existing.cloudflare_tunnel_id),
        cloudflare_tunnel_name = value(attrs, "cloudflare_tunnel_name", existing.cloudflare_tunnel_name),
        token_version = value(attrs, "token_version", existing.token_version),
        token_secret_key = value(attrs, "token_secret_key", existing.token_secret_key),
        token_path = value(attrs, "token_path", existing.token_path),
        config_path = value(attrs, "config_path", existing.config_path),
        connector_session_uuid = value(attrs, "connector_session_uuid", existing.connector_session_uuid),
        connector_generation = value(attrs, "connector_generation", existing.connector_generation) or 0,
        status = value(attrs, "status", existing.status) or "reconciling",
        message = value(attrs, "message", existing.message),
        retry_count = value(attrs, "retry_count", existing.retry_count) or 0,
        updated_at = now(),
    }

    exec([[INSERT INTO connector_state
        (id, cloudflare_tunnel_id, cloudflare_tunnel_name, token_version, token_secret_key,
         token_path, config_path, connector_session_uuid, connector_generation, status,
         message, retry_count, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
          cloudflare_tunnel_id = excluded.cloudflare_tunnel_id,
          cloudflare_tunnel_name = excluded.cloudflare_tunnel_name,
          token_version = excluded.token_version,
          token_secret_key = excluded.token_secret_key,
          token_path = excluded.token_path,
          config_path = excluded.config_path,
          connector_session_uuid = excluded.connector_session_uuid,
          connector_generation = excluded.connector_generation,
          status = excluded.status,
          message = excluded.message,
          retry_count = excluded.retry_count,
          updated_at = excluded.updated_at]],
        {
            record.id, record.cloudflare_tunnel_id, record.cloudflare_tunnel_name,
            record.token_version, record.token_secret_key, record.token_path,
            record.config_path, record.connector_session_uuid, record.connector_generation,
            record.status, record.message, record.retry_count, record.updated_at,
        })

    return record
end

function M.list_claims()
    return rows("SELECT * FROM stable_url_claims ORDER BY created_at ASC")
end

function M.active_claims()
    return rows([[SELECT * FROM stable_url_claims
                  WHERE COALESCE(status, 'claimed') IN ('claimed', 'reconciling')
                  ORDER BY created_at ASC]])
end

function M.upsert_claim(attrs)
    attrs = attrs or {}
    local id = attrs.id or attrs.hostname
    if type(id) ~= "string" or id == "" then
        return nil, "claim id is required"
    end
    if type(attrs.hostname) ~= "string" or attrs.hostname == "" then
        return nil, "hostname is required"
    end
    if type(attrs.local_service_url) ~= "string" or attrs.local_service_url == "" then
        return nil, "local_service_url is required"
    end
    local ts = now()
    local public_url = attrs.public_url or ("https://" .. attrs.hostname)
    exec([[INSERT INTO stable_url_claims
        (id, hostname, public_url, owner_plugin, owner_key, purpose, local_service_url,
         status, message, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
          hostname = excluded.hostname,
          public_url = excluded.public_url,
          owner_plugin = excluded.owner_plugin,
          owner_key = excluded.owner_key,
          purpose = excluded.purpose,
          local_service_url = excluded.local_service_url,
          status = excluded.status,
          message = excluded.message,
          updated_at = excluded.updated_at]],
        {
            id, attrs.hostname, public_url, attrs.owner_plugin, attrs.owner_key,
            attrs.purpose, attrs.local_service_url, attrs.status or "claimed",
            attrs.message, ts, ts,
        })
    return rows("SELECT * FROM stable_url_claims WHERE id = ? LIMIT 1", id)[1]
end

function M.mark_claims_status(status, message)
    exec("UPDATE stable_url_claims SET status = ?, message = ?, updated_at = ?", { status, message, now() })
end

return M
