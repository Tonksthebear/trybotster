-- Botster Lua Runtime Bootstrap
--
-- This file is loaded once on hub startup. It initializes core modules
-- and loads handler modules that register callbacks.
--
-- Module layout:
--   core/     - Protected modules (never reloaded): state, hooks, loader
--   lib/      - Library modules (hot-reloadable): client, utils
--   handlers/ - Handler modules (hot-reloadable): connections, agents, webrtc, tui

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
-- Library Loading
-- ============================================================================
-- Load library modules that provide shared abstractions. These are
-- hot-reloadable and must load BEFORE handlers that depend on them.

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

-- Load library modules
safe_require("lib.agent")
safe_require("lib.commands")

-- ============================================================================
-- Handler Loading
-- ============================================================================
-- Load handler modules that register callbacks. These are hot-reloadable.
-- Errors in handlers are caught to prevent breaking the entire runtime.

-- Load connection registry (shared client management for all transports)
safe_require("handlers.connections")

-- Load agent lifecycle handler (orchestrates creation/deletion)
-- Must load after connections (uses broadcast_hub_event)
safe_require("handlers.agents")

-- Load transport handlers (register peer/message callbacks)
safe_require("handlers.webrtc")
safe_require("handlers.tui")

-- Load command registrations (registers built-in hub commands)
-- Must load after transports; uses require() for lazy handler access.
safe_require("handlers.commands")

-- ============================================================================
-- Event Subscriptions (Logging)
-- ============================================================================
-- Register for Hub lifecycle events for logging purposes.
-- The actual event handling and broadcasting is done in handlers/connections.lua

events.on("shutdown", function()
    log.info("Hub shutting down - Lua cleanup")
    -- Could add cleanup logic here if needed
end)

-- ============================================================================
-- User Customization
-- ============================================================================
-- Load user init file if it exists. This is the entry point for all user
-- customization: hooks, custom commands, overrides, etc.
-- Analogous to Neovim's ~/.config/nvim/init.lua.

safe_require("user.init")

-- ============================================================================
-- Initialization Complete
-- ============================================================================

log.info("=== Lua Runtime Ready ===")
