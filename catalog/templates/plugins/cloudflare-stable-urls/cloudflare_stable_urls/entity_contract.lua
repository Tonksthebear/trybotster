-- @template Cloudflare Stable URLs
-- @description Entity contract for hub-level Cloudflare stable URL claims
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/cloudflare_stable_urls/entity_contract.lua
-- @scope device
-- @version 1.0.0

local M = {}

M.owner = "cloudflare-stable-urls"

M.types = {
    stable_url = M.owner .. ".stable_url",
}

M.stable_url_fields = {
    "id",
    "hostname",
    "public_url",
    "status",
    "owner_plugin",
    "owner_key",
    "purpose",
    "local_service_url",
    "token_version",
    "last_checked_at",
    "message",
}

return M
