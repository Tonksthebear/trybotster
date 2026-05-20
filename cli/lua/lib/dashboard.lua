-- Dashboard widget registry.
--
-- Plugins register inert UiNode descriptors here. The built-in workspace panel
-- renders those descriptors into the hub dashboard; entity bindings inside the
-- widget bodies hydrate through the normal client pull path.

local state = require("hub.state")

local M = {}

local registry = state.get("dashboard.registry", { by_id = {}, seq = 0 })
if registry.by_id == nil then registry.by_id = {} end
if registry.seq == nil then registry.seq = 0 end

local VALID_SIZES = {
    small = true,
    medium = true,
    wide = true,
    full = true,
}

local function is_nonempty_string(value)
    return type(value) == "string" and value ~= ""
end

local function current_plugin_key()
    local key = rawget(_G, "_loading_plugin_key")
        or rawget(_G, "_plugin_worker_key")
        or rawget(_G, "_loading_plugin_name")
    if is_nonempty_string(key) then return key end
    return nil
end

local function notify_changed()
    if type(hooks) == "table" and type(hooks.notify) == "function" then
        pcall(hooks.notify, "surfaces_changed", { registry = M, reason = "dashboard_widgets_changed" })
    end
end

local function normalize_size(value)
    if value == nil or value == "" then return "medium" end
    assert(type(value) == "string", "dashboard.register_widget: size must be a string")
    if not VALID_SIZES[value] then
        error("dashboard.register_widget: size must be one of small, medium, wide, full")
    end
    return value
end

local function normalize_body(opts)
    local body = opts.body or opts.node
    if body ~= nil then
        assert(type(body) == "table", "dashboard.register_widget: body must be a UiNode table")
        return body
    end
    if opts.children ~= nil then
        assert(type(opts.children) == "table", "dashboard.register_widget: children must be a table")
        return ui.stack{
            direction = "vertical",
            gap = opts.gap or "2",
            children = opts.children,
        }
    end
    error("dashboard.register_widget: pass body, node, or children")
end

--- Register or replace a dashboard widget.
--
-- Shape:
--   dashboard.register_widget("plugin.id", {
--     title = "Active Runs",
--     size = "wide",
--     order = 30,
--     body = ui.list{ ... },
--   })
function M.register_widget(id, opts)
    assert(is_nonempty_string(id), "dashboard.register_widget: id must be a non-empty string")
    assert(type(opts) == "table", "dashboard.register_widget: opts must be a table")

    local existing = registry.by_id[id]
    local seq = existing and existing.seq or (registry.seq + 1)
    if not existing then registry.seq = seq end

    local entry = {
        id = id,
        title = is_nonempty_string(opts.title) and opts.title or id,
        description = is_nonempty_string(opts.description) and opts.description or nil,
        icon = is_nonempty_string(opts.icon) and opts.icon or nil,
        size = normalize_size(opts.size),
        order = type(opts.order) == "number" and opts.order or nil,
        source = is_nonempty_string(opts.source) and opts.source or nil,
        owner_plugin = opts.owner_plugin or current_plugin_key(),
        body = normalize_body(opts),
        seq = seq,
    }
    registry.by_id[id] = entry
    notify_changed()
    return entry
end

function M.unregister_widget(id)
    if not is_nonempty_string(id) then return false end
    if registry.by_id[id] == nil then return false end
    registry.by_id[id] = nil
    notify_changed()
    return true
end

function M.unregister_by_plugin(plugin_key)
    if not is_nonempty_string(plugin_key) then return 0 end
    local ids = {}
    for id, entry in pairs(registry.by_id) do
        if entry.owner_plugin == plugin_key then
            ids[#ids + 1] = id
        end
    end
    table.sort(ids)
    for _, id in ipairs(ids) do
        registry.by_id[id] = nil
    end
    if #ids > 0 then notify_changed() end
    return #ids
end

function M.list()
    local out = {}
    for id, entry in pairs(registry.by_id) do
        out[#out + 1] = {
            id = id,
            title = entry.title,
            description = entry.description,
            icon = entry.icon,
            size = entry.size,
            order = entry.order,
            source = entry.source,
            owner_plugin = entry.owner_plugin,
            seq = entry.seq,
        }
    end
    table.sort(out, function(a, b)
        local ao = a.order or math.huge
        local bo = b.order or math.huge
        if ao ~= bo then return ao < bo end
        if a.seq ~= b.seq then return a.seq < b.seq end
        return a.id < b.id
    end)
    return out
end

function M.has_widgets()
    return next(registry.by_id) ~= nil
end

function M.render(_opts)
    local children = {}
    for _, summary in ipairs(M.list()) do
        local entry = registry.by_id[summary.id]
        if entry then
            children[#children + 1] = ui.panel{
                id = "dashboard-widget-" .. entry.id,
                title = entry.title,
                border = true,
                interaction_density = "comfortable",
                children = {
                    entry.body,
                },
            }
        end
    end
    return ui.stack{
        direction = "vertical",
        gap = "3",
        children = children,
    }
end

function M._reset_for_tests()
    for id in pairs(registry.by_id) do registry.by_id[id] = nil end
    registry.seq = 0
end

function M._before_reload()
    if log and log.info then log.info("dashboard.lua reloading") end
end

function M._after_reload()
    if log and log.info then log.info("dashboard.lua reloaded") end
end

return M
