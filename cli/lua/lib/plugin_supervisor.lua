-- Plugin supervisor ownership boundary.
--
-- This module owns plugin-level lifecycle cleanup for shared Lua registries.
-- Per-plugin execution workers sit behind this boundary: the hub/loader asks
-- the supervisor to tear down one plugin key, and registries remove only the
-- capabilities published by that plugin.

local M = {}

local cleanup_modules = {
    { module = "hub.hooks", fn = "unregister_by_plugin" },
    { module = "lib.commands", fn = "unregister_by_plugin" },
    { module = "lib.action", fn = "unregister_by_plugin" },
    { module = "lib.session_actions", fn = "unregister_by_plugin" },
    { module = "lib.surfaces", fn = "unregister_by_plugin" },
    { module = "lib.plugin_assets", fn = "unregister_by_plugin" },
    { module = "lib.mcp", fn = "unregister_by_plugin" },
    { module = "lib.entity_broadcast", fn = "unregister_by_plugin" },
}

local DEFAULT_TIMEOUT_MS = 5000

function M.current_plugin_key()
    local key = rawget(_G, "_loading_plugin_key") or rawget(_G, "_loading_plugin_name")
    if type(key) == "string" and key ~= "" then return key end
    return nil
end

function M.current_plugin_name()
    local name = rawget(_G, "_loading_plugin_display_name") or rawget(_G, "_loading_plugin_name")
    if type(name) == "string" and name ~= "" then return name end
    return nil
end

function M.cleanup_plugin(plugin_key, metadata)
    if type(plugin_key) ~= "string" or plugin_key == "" then return 0 end
    metadata = metadata or {}
    metadata.key = metadata.key or plugin_key

    local count = 0
    for _, entry in ipairs(cleanup_modules) do
        local ok, module = pcall(require, entry.module)
        if ok and type(module) == "table" and type(module[entry.fn]) == "function" then
            local clean_ok, cleaned = pcall(module[entry.fn], plugin_key, metadata)
            if clean_ok then
                count = count + (tonumber(cleaned) or 0)
            elseif log and log.warn then
                log.warn(string.format(
                    "plugin_supervisor cleanup failed for %s via %s.%s: %s",
                    plugin_key, entry.module, entry.fn, tostring(cleaned)))
            end
        end
    end

    local events_table = rawget(_G, "events")
    if type(events_table) == "table" and type(events_table._unregister_by_plugin) == "function" then
        local clean_ok, cleaned = pcall(events_table._unregister_by_plugin, plugin_key)
        if clean_ok then
            count = count + (tonumber(cleaned) or 0)
        elseif log and log.warn then
            log.warn(string.format(
                "plugin_supervisor cleanup failed for %s via events._unregister_by_plugin: %s",
                plugin_key, tostring(cleaned)))
        end
    end

    local watch_table = rawget(_G, "watch")
    if type(watch_table) == "table" and type(watch_table._unregister_by_plugin) == "function" then
        local clean_ok, cleaned = pcall(watch_table._unregister_by_plugin, plugin_key)
        if clean_ok then
            count = count + (tonumber(cleaned) or 0)
        elseif log and log.warn then
            log.warn(string.format(
                "plugin_supervisor cleanup failed for %s via watch._unregister_by_plugin: %s",
                plugin_key, tostring(cleaned)))
        end
    end

    return count
end

function M.load_plugin(plugin_key, metadata)
    if rawget(_G, "_loading_plugin_worker") == true then return true end
    if type(plugin_key) ~= "string" or plugin_key == "" then return true end
    if type(__plugin_worker_load) ~= "function" then return true end
    metadata = metadata or {}
    local ok, err = pcall(__plugin_worker_load, {
        plugin_key = plugin_key,
        display_name = metadata.name or metadata.plugin_name or plugin_key,
        init_path = metadata.path,
        source = metadata.source,
        repo_root = metadata.repo_root,
        lua_base_path = rawget(_G, "_lua_base_path"),
        parent_hub_id = hub and hub.hub_id and hub.hub_id() or nil,
    })
    if not ok then
        return false, tostring(err)
    end
    return true
end

function M.shutdown_plugin(plugin_key, reason)
    if type(plugin_key) ~= "string" or plugin_key == "" then return true end
    if type(__plugin_worker_shutdown) == "function" then
        pcall(__plugin_worker_shutdown, plugin_key, reason or "shutdown")
    end
    return true
end

function M.invoke(plugin_key, label, fn, opts, ...)
    assert(type(fn) == "function", "plugin_supervisor.invoke: fn must be a function")
    opts = opts or {}
    local timeout_ms = tonumber(opts.timeout_ms) or DEFAULT_TIMEOUT_MS
    local has_plugin_owner = type(plugin_key) == "string" and plugin_key ~= ""

    local returns
    if has_plugin_owner and type(__plugin_worker_invoke) == "function" and opts.handler_kind then
        returns = table.pack(pcall(
            __plugin_worker_invoke,
            plugin_key,
            opts.handler_kind,
            opts.handler_id or label,
            opts.handler_name,
            opts.payload or {},
            timeout_ms))
    elseif has_plugin_owner and type(__hook_timed_pcall) == "function" then
        returns = table.pack(__hook_timed_pcall(fn, timeout_ms, ...))
    else
        returns = table.pack(pcall(fn, ...))
    end

    local ok = returns[1]
    if not ok then
        local message = tostring(returns[2])
        if log and log.warn then
            log.warn(string.format(
                "plugin %s %s failed: %s",
                tostring(plugin_key or "builtin"), tostring(label or "handler"), message))
        end
        return false, message
    end

    return true, table.unpack(returns, 2, returns.n)
end

return M
