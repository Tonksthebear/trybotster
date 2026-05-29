-- @template Cloudflare Stable URLs
-- @description Entity publishers for Cloudflare stable URL claims
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/cloudflare_stable_urls/entities.lua
-- @scope device
-- @version 1.0.0

local contract = require("cloudflare_stable_urls.entity_contract")
local repo = require("cloudflare_stable_urls.repo")

local OWNER = contract.owner
local ENTITY_TYPE = contract.types.stable_url

local M = {}

M.types = contract.types

local function connector()
    return repo.connector() or {}
end

local function project_claim(row)
    local state = connector()
    return {
        id = row.id,
        hostname = row.hostname,
        public_url = row.public_url,
        status = row.status,
        owner_plugin = row.owner_plugin,
        owner_key = row.owner_key,
        purpose = row.purpose,
        local_service_url = row.local_service_url,
        token_version = state.token_version,
        last_checked_at = state.updated_at,
        message = row.message or state.message,
    }
end

local function all()
    local out = {}
    for _, row in ipairs(repo.list_claims()) do
        out[#out + 1] = project_claim(row)
    end
    return out
end

function M.register()
    local EB = require("lib.entity_broadcast")
    EB.register(ENTITY_TYPE, {
        id_field = "id",
        owner_plugin = OWNER,
        all = all,
        query = function(request)
            local id = request and request.id
            local items = all()
            if type(id) ~= "string" or id == "" then return items end
            for _, item in ipairs(items) do
                if item.id == id then return { item } end
            end
            return {}
        end,
    })
end

function M.snapshot()
    require("lib.hub").get():entity_snapshot(ENTITY_TYPE, all(), { owner_plugin = OWNER })
end

function M.upsert(row)
    if type(row) ~= "table" or type(row.id) ~= "string" or row.id == "" then return end
    require("lib.hub").get():entity_upsert(ENTITY_TYPE, project_claim(row), { owner_plugin = OWNER })
end

return M
