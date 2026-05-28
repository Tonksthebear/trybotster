-- @template Cloudflare Stable URLs
-- @description Stable webhook URL pool and claim API backed by plugin.db
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/cloudflare_stable_urls/entity_contract.lua
-- @scope device
-- @version 0.1.0

local M = {}

M.owner = "cloudflare-stable-urls"

M.types = {
    stable_url = M.owner .. ".stable_url",
}

M.fields = {
    stable_url = {
        "id",
        "hostname",
        "public_url",
        "status",
        "owner_plugin",
        "owner_key",
        "purpose",
        "local_service_url",
        "local_route",
        "local_port",
        "session_uuid",
        "token_version",
        "last_checked_at",
        "message",
        "created_at",
        "claimed_at",
        "released_at",
    },
}

return M
