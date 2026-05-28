-- @template Cloudflare Stable URLs
-- @description Stable webhook URL pool and claim API backed by plugin.db
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/cloudflare_stable_urls/api.lua
-- @scope device
-- @version 0.1.0

local repo = require("cloudflare_stable_urls.repo")

local M = {}

local function register_one(name, fn)
    require("lib.stable_urls").register(name, fn, {
        owner_plugin = "cloudflare-stable-urls",
        handler_id = "stable_urls:" .. name,
        timeout_ms = 5000,
    })
end

function M.register()
    register_one("claim", function(params, context)
        params = params or {}
        if params.session_uuid == nil and context and context.session_uuid then
            params.session_uuid = context.session_uuid
        end
        return repo.claim(params)
    end)
    register_one("release", function(params, context)
        params = params or {}
        if params.session_uuid == nil and context and context.session_uuid then
            params.session_uuid = context.session_uuid
        end
        return repo.release(params)
    end)
    register_one("list", function(params)
        return repo.list(params or {})
    end)
    register_one("get", function(params)
        return repo.get(params or {})
    end)
end

return M
