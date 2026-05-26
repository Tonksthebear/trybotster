-- ActionCable Lua-side support for plugin worker boundary routing.
--
-- The Rust side (action_cable.rs) stores callbacks in the hub-side registry
-- for platform subscriptions, and for plugin-owned subscriptions it also
-- mints a stable handler_id and routes through the per-plugin worker.
--
-- This file provides the small handler table + _invoke_ac_message that the
-- worker dispatch arm (`plugin_worker_invoke_ac_message`) calls.

local M = {}

local handlers = {}

--- Called from Rust at subscribe time for plugin-owned AC subscriptions.
function M._register_handler(handler_id, fn)
    if type(handler_id) ~= "string" or handler_id == "" then
        error("handler_id must be non-empty string")
    end
    if type(fn) ~= "function" then
        error("_register_handler requires a function")
    end
    handlers[handler_id] = fn
end

--- Called from Rust (or Lua cleanup) to drop a handler.
function M._unregister_handler(handler_id)
    handlers[handler_id] = nil
end

--- Called from the plugin worker dispatch for "ac_message" kind.
function M._invoke_ac_message(handler_id, channel_id, message)
    local fn = handlers[handler_id]
    if not fn then
        error("ac handler not registered: " .. tostring(handler_id))
    end
    return fn(message, channel_id)
end

return M
