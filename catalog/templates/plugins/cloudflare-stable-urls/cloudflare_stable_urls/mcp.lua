-- @template Cloudflare Stable URLs
-- @description Stable webhook URL pool and claim API backed by plugin.db
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/cloudflare_stable_urls/mcp.lua
-- @scope device
-- @version 0.1.0

local repo = require("cloudflare_stable_urls.repo")

local M = {}

local status_enum = { "available", "claimed", "reconciling", "unhealthy", "revoked" }

local function tool(name, description, properties, required, handler)
    mcp.tool(name, {
        description = description,
        input_schema = {
            type = "object",
            properties = properties or {},
            required = required or {},
        },
    }, function(params, context)
        return {
            ok = true,
            result = handler(params or {}, context or {}),
        }
    end)
end

function M.register()
    tool("stable_urls_claim", "Claim one available stable URL for a plugin owner.", {
        owner_plugin = { type = "string" },
        owner_key = { type = "string" },
        purpose = { type = "string" },
        id = { type = "string" },
        session_uuid = { type = "string" },
        local_service_url = { type = "string" },
        local_route = { type = "string" },
        local_port = { type = "integer" },
    }, { "owner_plugin", "owner_key", "purpose" }, function(params, context)
        if params.session_uuid == nil and context.session_uuid then
            params.session_uuid = context.session_uuid
        end
        return repo.claim(params)
    end)

    tool("stable_urls_release", "Release a stable URL claimed by the matching owner.", {
        id = { type = "string" },
        owner_plugin = { type = "string" },
        owner_key = { type = "string" },
        reason = { type = "string" },
        session_uuid = { type = "string" },
    }, { "id", "owner_plugin", "owner_key" }, function(params, context)
        if params.session_uuid == nil and context.session_uuid then
            params.session_uuid = context.session_uuid
        end
        return repo.release(params)
    end)

    tool("stable_urls_list", "List stable URL records.", {
        status = { type = "string", enum = status_enum },
        owner_plugin = { type = "string" },
        owner_key = { type = "string" },
    }, {}, function(params)
        return repo.list(params)
    end)

    tool("stable_urls_get", "Get one stable URL record by id.", {
        id = { type = "string" },
    }, { "id" }, function(params)
        return repo.get(params)
    end)
end

return M
