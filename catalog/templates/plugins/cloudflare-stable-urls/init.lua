-- @template Cloudflare Stable URLs
-- @description Hub-level Cloudflare named tunnel connector for stable plugin URLs
-- @category plugins
-- @dest plugins/cloudflare-stable-urls/init.lua
-- @scope device
-- @version 1.0.0

for _, module_name in ipairs({
    "cloudflare_stable_urls.db",
    "cloudflare_stable_urls.entity_contract",
    "cloudflare_stable_urls.repo",
    "cloudflare_stable_urls.entities",
    "cloudflare_stable_urls.connector",
}) do
    package.loaded[module_name] = nil
end

local entities = require("cloudflare_stable_urls.entities")
local connector = require("cloudflare_stable_urls.connector")

local M = {
    claim = connector.claim,
    list = connector.list,
    reconcile = connector.reconcile,
}

entities.register()
entities.snapshot()

if events and events.on then
    if _G.__cloudflare_stable_urls_prepared_sub and events.off then
        pcall(events.off, _G.__cloudflare_stable_urls_prepared_sub)
    end
    _G.__cloudflare_stable_urls_prepared_sub = events.on("plugin_command_prepared", function(data)
        local ok, err = pcall(connector.handle_plugin_command_prepared, data)
        if not ok then log.warn("[cloudflare-stable-urls] plugin_command_prepared failed: " .. tostring(err)) end
    end)

    if _G.__cloudflare_stable_urls_process_exit_sub and events.off then
        pcall(events.off, _G.__cloudflare_stable_urls_process_exit_sub)
    end
    _G.__cloudflare_stable_urls_process_exit_sub = events.on("process_exited", function(data)
        local ok, err = pcall(connector.handle_process_exited, data)
        if not ok then log.warn("[cloudflare-stable-urls] process_exited failed: " .. tostring(err)) end
    end)
end

if hooks and hooks.on then
    hooks.on("agent_created", "cloudflare_stable_urls.connector_created", function(info)
        local ok, err = pcall(connector.handle_agent_created, info)
        if not ok then log.warn("[cloudflare-stable-urls] agent_created failed: " .. tostring(err)) end
    end)
end

pcall(connector.reconcile, "plugin_load")

log.info("[cloudflare-stable-urls] loaded")

return M
