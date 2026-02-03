-- Botster Lua Runtime Bootstrap
--
-- This file is loaded once on hub startup. It initializes core modules
-- and loads handler modules that register callbacks.
--
-- Module layout:
--   core/     - Protected modules (never reloaded): state, hooks, loader
--   lib/      - Library modules (hot-reloadable): client, utils
--   handlers/ - Handler modules (hot-reloadable): webrtc

log.info("=== Botster Lua Runtime ===")

-- Load core modules (protected, never reloaded)
local state = require("core.state")
local hooks = require("core.hooks")
local loader = require("core.loader")

-- Make core modules globally available for convenient access
_G.hooks = hooks
_G.state = state
_G.loader = loader

log.debug("Core modules loaded: state, hooks, loader")

-- ============================================================================
-- Handler Loading
-- ============================================================================
-- Load handler modules that register callbacks. These are hot-reloadable.
-- Errors in handlers are caught to prevent breaking the entire runtime.

--- Safely require a module, logging errors without failing.
-- @param module_name The module name to require
-- @return The module if successful, nil otherwise
local function safe_require(module_name)
    local ok, result = pcall(require, module_name)
    if ok then
        log.info(string.format("Loaded: %s", module_name))
        return result
    else
        log.error(string.format("Failed to load %s: %s", module_name, tostring(result)))
        return nil
    end
end

-- Load WebRTC handler (registers peer/message callbacks)
-- This is the main handler that routes browser messages to Client instances
safe_require("handlers.webrtc")

-- ============================================================================
-- Event Subscriptions (Logging)
-- ============================================================================
-- Register for Hub lifecycle events for logging purposes.
-- The actual event handling and broadcasting is done in handlers/webrtc.lua

events.on("shutdown", function()
    log.info("Hub shutting down - Lua cleanup")
    -- Could add cleanup logic here if needed
end)

-- ============================================================================
-- Initialization Complete
-- ============================================================================

log.info("=== Lua Runtime Ready ===")
