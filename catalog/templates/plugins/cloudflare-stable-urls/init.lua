-- @template Cloudflare Stable URLs
-- @description Stable webhook URL pool and claim API backed by plugin.db
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/init.lua
-- @scope device
-- @version 0.1.0

for _, module_name in ipairs({
    "cloudflare_stable_urls.db",
    "cloudflare_stable_urls.entity_contract",
    "cloudflare_stable_urls.entities",
    "cloudflare_stable_urls.repo",
    "cloudflare_stable_urls.api",
    "cloudflare_stable_urls.mcp",
}) do
    package.loaded[module_name] = nil
end

local repo = require("cloudflare_stable_urls.repo")
local entities = require("cloudflare_stable_urls.entities")
local api = require("cloudflare_stable_urls.api")
local mcp_tools = require("cloudflare_stable_urls.mcp")

local M = {}

repo.seed_default_pool()
entities.register()
api.register()
mcp_tools.register()

log.info("[cloudflare-stable-urls] loaded")

return M
