-- @template Cloudflare Stable URLs
-- @description Stable webhook URL pool and claim API backed by plugin.db
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/cloudflare_stable_urls/repo.lua
-- @scope device
-- @version 0.1.0

local db = require("cloudflare_stable_urls.db")

local M = {}

math.randomseed(os.time())

local ALLOWED_STATUSES = {
    available = true,
    claimed = true,
    reconciling = true,
    unhealthy = true,
    revoked = true,
}

local PUBLIC_STATUS = {
    available = true,
    claimed = true,
}

local function now()
    return os.time()
end

local function id(prefix)
    return string.format("%s_%d_%06d", prefix, os.time(), math.random(0, 999999))
end

local function blank(value)
    return value == nil or tostring(value):match("^%s*$") ~= nil
end

local function assert_present(value, name)
    if blank(value) then
        error(name .. " is required")
    end
    return tostring(value)
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

local function first(sql, ...)
    local result = rows(sql, ...)
    return result[1]
end

local function encode(value)
    if value == nil then return "{}" end
    local ok, encoded = pcall(json.encode, value)
    if ok and encoded then return encoded end
    return "{}"
end

local function publish(row)
    local ok, entities = pcall(require, "cloudflare_stable_urls.entities")
    if ok and entities and type(entities.upsert) == "function" then
        pcall(entities.upsert, row)
    end
end

local function audit(attrs)
    db.audit_events:insert{
        id = id("audit"),
        stable_url_id = attrs.stable_url_id,
        claim_id = attrs.claim_id,
        action = attrs.action,
        owner_plugin = attrs.owner_plugin,
        owner_key = attrs.owner_key,
        session_uuid = attrs.session_uuid,
        message = attrs.message,
        metadata_json = encode(attrs.metadata),
        created_at = now(),
    }
end

local function active_claim(stable_url_id)
    return first("SELECT * FROM claims WHERE stable_url_id = ? AND released_at IS NULL LIMIT 1", stable_url_id)
end

local function project(url, claim)
    if not url then return nil end
    claim = claim or active_claim(url.id)
    local row = {
        id = url.id,
        hostname = url.hostname,
        public_url = url.public_url,
        status = url.status,
        token_version = url.token_version,
        last_checked_at = url.last_checked_at,
        message = url.message,
        created_at = url.created_at,
        updated_at = url.updated_at,
    }
    if claim then
        row.owner_plugin = claim.owner_plugin
        row.owner_key = claim.owner_key
        row.purpose = claim.purpose
        row.session_uuid = claim.session_uuid
        row.local_service_url = claim.local_service_url
        row.local_route = claim.local_route
        row.local_port = claim.local_port
        row.claimed_at = claim.created_at
        row.released_at = claim.released_at
    end
    return row
end

local function get_url(stable_url_id)
    return db.stable_urls:where{ id = stable_url_id }
end

function M.seed_default_pool()
    local existing = first("SELECT COUNT(*) AS count FROM stable_urls")
    if existing and tonumber(existing.count or 0) > 0 then
        return
    end
    local ts = now()
    for index = 1, 2 do
        local hostname = string.format("hook-%d.example.invalid", index)
        db.stable_urls:insert{
            id = "surl_seed_" .. tostring(index),
            hostname = hostname,
            public_url = "https://" .. hostname,
            status = "available",
            message = "Seeded placeholder URL",
            created_at = ts,
            updated_at = ts,
        }
    end
end

function M.get(params)
    params = params or {}
    local stable_url_id = assert_present(params.id, "id")
    local url = get_url(stable_url_id)
    return project(url)
end

function M.list(params)
    params = params or {}
    local clauses = {}
    local values = {}
    if not blank(params.status) then
        local status = tostring(params.status)
        if not ALLOWED_STATUSES[status] then
            error("status must be available, claimed, reconciling, unhealthy, or revoked")
        end
        clauses[#clauses + 1] = "u.status = ?"
        values[#values + 1] = status
    end
    if not blank(params.owner_plugin) then
        clauses[#clauses + 1] = "c.owner_plugin = ?"
        values[#values + 1] = tostring(params.owner_plugin)
    end
    if not blank(params.owner_key) then
        clauses[#clauses + 1] = "c.owner_key = ?"
        values[#values + 1] = tostring(params.owner_key)
    end

    local sql = [[SELECT u.*, c.id AS claim_id, c.owner_plugin, c.owner_key, c.purpose,
                         c.session_uuid, c.local_service_url, c.local_route,
                         c.local_port, c.created_at AS claim_created_at,
                         c.released_at AS claim_released_at
                    FROM stable_urls u
                    LEFT JOIN claims c
                      ON c.stable_url_id = u.id AND c.released_at IS NULL]]
    if #clauses > 0 then
        sql = sql .. " WHERE " .. table.concat(clauses, " AND ")
    end
    sql = sql .. " ORDER BY u.created_at ASC, u.id ASC"

    local records = rows(sql, table.unpack(values))
    local out = {}
    for _, row in ipairs(records) do
        local claim = nil
        if row.claim_id then
            claim = {
                id = row.claim_id,
                owner_plugin = row.owner_plugin,
                owner_key = row.owner_key,
                purpose = row.purpose,
                session_uuid = row.session_uuid,
                local_service_url = row.local_service_url,
                local_route = row.local_route,
                local_port = row.local_port,
                created_at = row.claim_created_at,
                released_at = row.claim_released_at,
            }
        end
        out[#out + 1] = project(row, claim)
    end
    return out
end

function M.claim(params)
    params = params or {}
    local owner_plugin = assert_present(params.owner_plugin, "owner_plugin")
    local owner_key = assert_present(params.owner_key, "owner_key")
    if params.owner_id ~= nil then
        error("owner_id is not accepted; use owner_key")
    end
    local purpose = assert_present(params.purpose, "purpose")
    if blank(params.local_service_url) and (blank(params.local_route) or params.local_port == nil) then
        error("local_service_url or local_route/local_port is required")
    end
    if not blank(params.id) then
        local existing_claim = active_claim(tostring(params.id))
        if existing_claim then
            audit{
                stable_url_id = tostring(params.id),
                claim_id = existing_claim.id,
                action = "claim_failed",
                owner_plugin = owner_plugin,
                owner_key = owner_key,
                session_uuid = params.session_uuid,
                message = "Stable URL already claimed",
            }
            error("stable URL already claimed")
        end
    end

    local selected
    local claim
    db:execute(function()
        if blank(params.id) then
            selected = first("SELECT * FROM stable_urls WHERE status = 'available' ORDER BY created_at ASC, id ASC LIMIT 1")
        else
            selected = get_url(tostring(params.id))
            if selected and selected.status ~= "available" then
                selected = nil
            end
        end
        if not selected then
            audit{
                action = "claim_failed",
                owner_plugin = owner_plugin,
                owner_key = owner_key,
                session_uuid = params.session_uuid,
                message = "No available stable URLs",
            }
            error("no available stable URLs")
        end
        if active_claim(selected.id) then
            audit{
                stable_url_id = selected.id,
                action = "claim_failed",
                owner_plugin = owner_plugin,
                owner_key = owner_key,
                session_uuid = params.session_uuid,
                message = "Stable URL already claimed",
            }
            error("stable URL already claimed")
        end

        local ts = now()
        claim = {
            id = id("claim"),
            stable_url_id = selected.id,
            owner_plugin = owner_plugin,
            owner_key = owner_key,
            purpose = purpose,
            session_uuid = params.session_uuid,
            local_service_url = params.local_service_url,
            local_route = params.local_route,
            local_port = params.local_port,
            created_at = ts,
        }
        db.claims:insert(claim)
        db.stable_urls:update{
            where = { id = selected.id },
            set = {
                status = "claimed",
                message = "Claimed by " .. owner_plugin,
                updated_at = ts,
            },
        }
        audit{
            stable_url_id = selected.id,
            claim_id = claim.id,
            action = "claim",
            owner_plugin = owner_plugin,
            owner_key = owner_key,
            session_uuid = params.session_uuid,
            message = "Stable URL claimed",
        }
    end)

    local row = project(get_url(selected.id), claim)
    publish(row)
    return row
end

function M.release(params)
    params = params or {}
    local stable_url_id = assert_present(params.id, "id")
    local owner_plugin = assert_present(params.owner_plugin, "owner_plugin")
    local owner_key = assert_present(params.owner_key, "owner_key")
    if params.owner_id ~= nil then
        error("owner_id is not accepted; use owner_key")
    end

    local claim = active_claim(stable_url_id)
    if not claim then
        audit{
            stable_url_id = stable_url_id,
            action = "release_failed",
            owner_plugin = owner_plugin,
            owner_key = owner_key,
            session_uuid = params.session_uuid,
            message = "No active claim",
        }
        error("no active claim")
    end
    if claim.owner_plugin ~= owner_plugin or claim.owner_key ~= owner_key then
        audit{
            stable_url_id = stable_url_id,
            claim_id = claim.id,
            action = "release_failed",
            owner_plugin = owner_plugin,
            owner_key = owner_key,
            session_uuid = params.session_uuid,
            message = "Owner validation failed",
        }
        error("owner validation failed")
    end

    db:execute(function()
        local ts = now()
        db.claims:update{
            where = { id = claim.id },
            set = {
                released_at = ts,
                release_reason = params.reason,
            },
        }
        db.stable_urls:update{
            where = { id = stable_url_id },
            set = {
                status = "available",
                message = "Available",
                updated_at = ts,
            },
        }
        audit{
            stable_url_id = stable_url_id,
            claim_id = claim.id,
            action = "release",
            owner_plugin = owner_plugin,
            owner_key = owner_key,
            session_uuid = params.session_uuid,
            message = params.reason or "Stable URL released",
        }
    end)

    local row = project(get_url(stable_url_id), nil)
    publish(row)
    return row
end

function M.audit_events()
    return rows("SELECT id, stable_url_id, claim_id, action, owner_plugin, owner_key, session_uuid, message, metadata_json, created_at FROM audit_events ORDER BY created_at ASC, id ASC")
end

function M.allowed_statuses()
    return ALLOWED_STATUSES
end

function M.public_statuses()
    return PUBLIC_STATUS
end

return M
