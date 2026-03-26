-- Mouse event handler registry.
--
-- Rust dispatches pre-routed mouse events here after hit-testing widget areas.
-- Handlers are registered by plugins via botster.mouse.on(). Each handler
-- receives an event table with widget-local coordinates and can return an
-- ops array (like on_action/on_hub_event) or nil to pass through.
--
-- Event table shape:
--   {
--     type = "press" | "release" | "drag",
--     button = "left" | "right" | "middle",
--     x = 10, y = 5,          -- screen coords (0-indexed)
--     widget = {               -- nil if no widget hit
--       type = "terminal",
--       id = "sess-...",
--       x = 3, y = 2,         -- widget-local coords (0-indexed)
--     }
--   }

local M = {}

-- Handler registry: array of { event_type, callback, widget_type, namespace }
local handlers = {}

--- Register a mouse event handler.
---
--- @param event_type string "press"|"release"|"drag"|"*"
--- @param callback function(event) -> table[]|nil  -- ops array: { {op="...", ...}, ... }
--- @param opts table? {widget_type=string?, namespace=string?}
function M.on(event_type, callback, opts)
  opts = opts or {}
  handlers[#handlers + 1] = {
    event_type = event_type,
    callback = callback,
    widget_type = opts.widget_type,
    namespace = opts.namespace or "default",
  }
end

--- Remove all handlers in a namespace.
---
--- @param namespace string
function M.off(namespace)
  local kept = {}
  for _, h in ipairs(handlers) do
    if h.namespace ~= namespace then
      kept[#kept + 1] = h
    end
  end
  handlers = kept
end

--- Dispatch a mouse event to registered handlers.
---
--- Called by Rust. Returns the first non-nil handler result, or nil if no
--- handler claims the event.
---
--- @param event table Mouse event from Rust
--- @return table|nil Action table or nil
function M.handle_mouse(event)
  local evt_type = event.type
  local widget_type = event.widget and event.widget.type

  for _, h in ipairs(handlers) do
    -- Match event type (* = wildcard)
    if h.event_type == evt_type or h.event_type == "*" then
      -- Match widget type filter (nil = any widget)
      if h.widget_type == nil or h.widget_type == widget_type then
        local result = h.callback(event)
        if result ~= nil then
          return result
        end
      end
    end
  end

  return nil
end

return M
