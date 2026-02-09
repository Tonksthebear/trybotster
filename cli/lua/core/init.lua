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
safe_require("lib.config_resolver")
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

-- Load ActionCable handlers (hub commands + GitHub events)
-- Must load after agents (emits command_message events)
safe_require("handlers.hub_commands")
safe_require("handlers.github")

-- Load transport handlers (register peer/message callbacks)
safe_require("handlers.webrtc")
safe_require("handlers.tui")

-- Load command registrations (registers built-in hub commands)
-- Must load after transports; uses require() for lazy handler access.
safe_require("handlers.commands")

-- Load filesystem command handlers (fs:read, fs:write, fs:list, etc.)
-- Must load after commands registry.
safe_require("handlers.filesystem")

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
-- Plugin Loading
-- ============================================================================
-- Load plugins from ~/.botster/lua/plugins/*/init.lua
-- Each plugin is a directory with an init.lua that registers hooks,
-- commands, or other extensions. Analogous to Neovim's plugin system.

local plugin_base = config.lua_path()
local plugin_dir = plugin_base .. "/plugins"

local plugins = loader.discover_plugins(plugin_dir)
if #plugins > 0 then
    log.info(string.format("Discovered %d plugin(s): %s", #plugins, table.concat(plugins, ", ")))
    for _, plugin_name in ipairs(plugins) do
        safe_require("plugins." .. plugin_name .. ".init")
    end
else
    log.debug("No plugins found")
end

-- ============================================================================
-- Agent Improvements (Sandboxed)
-- ============================================================================
-- Load agent-written improvements with restricted access.
-- These run in a sandbox: no process spawn, no keyring, fs restricted
-- to the improvements directory only.

local improvements_dir = plugin_base .. "/improvements"
if fs.exists(improvements_dir) then
    local count = loader.load_improvements(improvements_dir)
    if count > 0 then
        log.info(string.format("Loaded %d improvement(s) from %s", count, improvements_dir))
    end
end

-- ============================================================================
-- Initialization Complete
-- ============================================================================

log.info("=== Lua Runtime Ready ===")
