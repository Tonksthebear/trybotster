-- Plugin asset registry.
--
-- Plugins use this to expose explicit local files to browser surfaces without
-- handing the browser arbitrary filesystem paths. The browser resolves
-- `botster-plugin-asset://<asset_id>?v=<version>` URLs over the existing hub
-- transport and mounts the response in a sandboxed iframe.

local state = require("hub.state")

local M = {}

local registry = state.get("plugin_assets.registry", { by_id = {} })
if registry.by_id == nil then registry.by_id = {} end

local message_handlers = state.get("plugin_assets.message_handlers", {})

local function caller_plugin_key()
    local name = rawget(_G, "_loading_plugin_key") or rawget(_G, "_loading_plugin_name")
    if type(name) == "string" and name ~= "" then return name end
    return "builtin"
end

local function running_in_plugin_worker(plugin_key)
    return type(plugin_key) == "string"
        and plugin_key ~= ""
        and rawget(_G, "_plugin_worker_key") == plugin_key
end

local function normalize_message_result(result)
    local ok, action = pcall(require, "lib.action")
    if ok and result == action.HANDLED then
        return { __plugin_asset_status = "handled" }
    end
    if type(result) == "table" and result.__ui_action_result == true then
        result.__plugin_asset_status = "result"
        return result
    end
    return { __plugin_asset_status = "handled" }
end

local function sanitize_part(value)
    return tostring(value or ""):gsub("[^%w%._-]", "_")
end

local function version_for(path)
    local stat = type(fs) == "table" and fs.stat and fs.stat(path) or nil
    if type(stat) ~= "table" then return "0" end
    local modified = stat.modified or stat.mtime or stat.updated_at or "0"
    local size = stat.size or "0"
    return tostring(modified) .. "-" .. tostring(size)
end

--- Expose one explicit file to browser plugin surfaces.
---
--- The returned URL is intentionally not a real http/file URL. The browser
--- iframe primitive resolves it through `plugin_asset:read` over the paired hub
--- connection, creates a blob URL locally, and uses that as iframe src.
function M.expose_file(name, path, opts)
    assert(type(name) == "string" and name ~= "",
        "plugin_assets.expose_file: name must be a non-empty string")
    assert(type(path) == "string" and path ~= "",
        "plugin_assets.expose_file: path must be a non-empty string")
    opts = opts or {}
    assert(type(opts) == "table", "plugin_assets.expose_file: opts must be a table")

    local plugin_name = caller_plugin_key()
    local asset_id = sanitize_part(plugin_name) .. ":" .. sanitize_part(name)
    local content_type = opts.content_type or opts.mime_type or "application/octet-stream"
    assert(type(content_type) == "string" and content_type ~= "",
        "plugin_assets.expose_file: content_type must be a non-empty string")

    registry.by_id[asset_id] = {
        id = asset_id,
        name = name,
        plugin_name = plugin_name,
        path = path,
        content_type = content_type,
    }

    return "botster-plugin-asset://" .. asset_id .. "?v=" .. version_for(path)
end

function M.get(asset_id)
    if type(asset_id) ~= "string" or asset_id == "" then return nil end
    return registry.by_id[asset_id]
end

function M.read(asset_id)
    local entry = M.get(asset_id)
    if not entry then
        return nil, "Unknown plugin asset"
    end
    local content, err = fs.read(entry.path)
    if not content then
        return nil, err or "Unable to read plugin asset"
    end
    return {
        asset_id = asset_id,
        content = content,
        content_type = entry.content_type,
        version = version_for(entry.path),
    }, nil
end

--- Register an iframe bridge message handler.
---
--- Browser iframes post messages that the `ui.iframe` primitive wraps as:
---   { id = "botster.plugin_asset.message",
---     payload = { assetId, action, payload } }
function M.on_message(action_name, handler, opts)
    assert(type(action_name) == "string" and action_name ~= "",
        "plugin_assets.on_message: action_name must be a non-empty string")
    assert(type(handler) == "function",
        "plugin_assets.on_message: handler must be a function")
    opts = opts or {}

    local plugin_name = caller_plugin_key()
    local key = plugin_name .. ":" .. action_name
    message_handlers[key] = {
        plugin_name = plugin_name,
        action = action_name,
        handler = handler,
        timeout_ms = opts.timeout_ms or 2000,
    }
end

local function dispatch_message(envelope, ctx)
    local payload = envelope.payload or {}
    local asset_id = payload.assetId or payload.asset_id
    local action_name = payload.action
    if type(action_name) ~= "string" or action_name == "" then
        return false
    end

    local entry = asset_id and M.get(asset_id) or nil
    local plugin_name = entry and entry.plugin_name or caller_plugin_key()
    local handler_entry =
        message_handlers[plugin_name .. ":" .. action_name] or
        message_handlers["builtin:" .. action_name]
    if not handler_entry then
        log.debug(string.format(
            "plugin_assets: unhandled iframe action `%s` for asset `%s`",
            tostring(action_name), tostring(asset_id)))
        return false
    end

    local local_ctx = {
        asset_id = asset_id,
        action = action_name,
        plugin_name = plugin_name,
        client = ctx and ctx.client,
        sub_id = ctx and ctx.sub_id,
        target_surface = ctx and ctx.target_surface,
    }
    local worker_ctx = {
        asset_id = asset_id,
        action = action_name,
        plugin_name = plugin_name,
        sub_id = ctx and ctx.sub_id,
        target_surface = ctx and ctx.target_surface,
    }

    if handler_entry.plugin_name ~= "builtin" and not running_in_plugin_worker(handler_entry.plugin_name) then
        local ok, result = require("lib.plugin_supervisor").invoke(
            handler_entry.plugin_name,
            "asset_message:" .. tostring(action_name),
            handler_entry.handler,
            {
                timeout_ms = handler_entry.timeout_ms or 2000,
                handler_kind = "asset_message",
                handler_id = action_name,
                handler_name = handler_entry.plugin_name,
                payload = {
                    message = payload.payload,
                    ctx = worker_ctx,
                },
            },
            payload.payload,
            worker_ctx)
        if not ok then error(result) end
        return true, result
    end

    return true, normalize_message_result(handler_entry.handler(payload.payload, local_ctx))
end

function M._install_action_handler()
    local action = require("lib.action")
    action.on("botster.plugin_asset.message", "builtin.plugin_assets.message", function(envelope, ctx)
        local handled, result = dispatch_message(envelope, ctx)
        if handled then
            if type(result) == "table" and result.__plugin_asset_status == "result" then
                return result
            end
            return action.HANDLED
        end
        return nil
    end)
end

function M._invoke_message(plugin_name, action_name, message, ctx)
    local handler_entry = message_handlers[tostring(plugin_name) .. ":" .. tostring(action_name)]
    if not handler_entry then
        error("plugin asset message handler not registered: " .. tostring(plugin_name) .. ":" .. tostring(action_name))
    end
    return normalize_message_result(handler_entry.handler(message, ctx or {}))
end

function M.unregister_by_plugin(plugin_key)
    if type(plugin_key) ~= "string" or plugin_key == "" then return 0 end
    local removed = 0
    for asset_id, entry in pairs(registry.by_id) do
        if entry.plugin_name == plugin_key then
            registry.by_id[asset_id] = nil
            removed = removed + 1
        end
    end
    for key, entry in pairs(message_handlers) do
        if entry.plugin_name == plugin_key then
            message_handlers[key] = nil
            removed = removed + 1
        end
    end
    return removed
end

function M._reset_for_tests()
    registry.by_id = {}
    for k in pairs(message_handlers) do message_handlers[k] = nil end
end

return M
