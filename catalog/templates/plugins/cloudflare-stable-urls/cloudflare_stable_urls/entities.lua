-- @template Cloudflare Stable URLs
-- @description Stable webhook URL pool and claim API backed by plugin.db
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/cloudflare_stable_urls/entities.lua
-- @scope device
-- @version 0.1.0

local contract = require("cloudflare_stable_urls.entity_contract")
local repo = require("cloudflare_stable_urls.repo")

local M = {}

M.types = contract.types

local OWNER = contract.owner
local ENTITY_TYPE = contract.types.stable_url

local function opts()
    return { owner_plugin = OWNER }
end

function M.register()
    local EB = require("lib.entity_broadcast")
    EB.register(ENTITY_TYPE, {
        id_field = "id",
        owner_plugin = OWNER,
        all = function()
            return repo.list()
        end,
        query = function(request, _context)
            request = request or {}
            if request.id then
                local row = repo.get{ id = request.id }
                return row and { row } or {}
            end
            if request.where then
                return repo.list(request.where)
            end
            return repo.list()
        end,
    })
end

function M.snapshot()
    require("lib.hub").get():entity_snapshot(ENTITY_TYPE, repo.list(), opts())
end

function M.upsert(row)
    if not row or type(row.id) ~= "string" or row.id == "" then
        return
    end
    require("lib.hub").get():entity_upsert(ENTITY_TYPE, row, opts())
end

return M
