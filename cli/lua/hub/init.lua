-- Botster Lua Runtime Bootstrap
--
-- This file is loaded once on hub startup. It initializes core modules
-- and loads handler modules that register callbacks.
--
-- Module layout:
--   hub/      - Protected modules (never reloaded): state, hooks, loader
--   lib/      - Library modules (hot-reloadable): client, utils
--   handlers/ - Handler modules (hot-reloadable): connections, agents, webrtc, tui

log.info("=== Botster Lua Runtime ===")

-- Load hub modules (protected, never reloaded)
local state = require("hub.state")
local hooks = require("hub.hooks")
local loader = require("hub.loader")

-- Make hub modules globally available for convenient access
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
_G.mcp = safe_require("lib.mcp")

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

-- Load ActionCable handlers (hub commands)
-- Must load after agents (emits command_message events)
safe_require("handlers.hub_commands")

-- Load transport handlers (register peer/message callbacks)
safe_require("handlers.webrtc")
safe_require("handlers.tui")
safe_require("handlers.socket")

-- Load command registrations (registers built-in hub commands)
-- Must load after transports; uses require() for lazy handler access.
safe_require("handlers.commands")

-- Load filesystem command handlers (fs:read, fs:write, fs:list, etc.)
-- Must load after commands registry.
safe_require("handlers.filesystem")

-- Load template command handlers (template:install, template:uninstall, template:list)
-- Must load after commands registry.
safe_require("handlers.templates")

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
-- Plugin Loading (Unified: device + repo layers)
-- ============================================================================
-- Uses config_resolver.resolve_all() to discover plugins across:
--   1. ~/.botster/shared/plugins/
--   2. ~/.botster/profiles/{profile}/plugins/
--   3. {repo}/.botster/shared/plugins/
--   4. {repo}/.botster/profiles/{profile}/plugins/

local ConfigResolver = require("lib.config_resolver")
local state = require("hub.state")
local plugin_registry = state.get("plugin_registry", {})
local loaded_plugin_names = {}

local device_root = config.data_dir and config.data_dir() or nil
local repo_root = (worktree and worktree.repo_root) and worktree.repo_root() or nil
local active_profile = (config.get and config.get("active_profile")) or nil

-- Store resolver opts so plugin watcher/reload can re-discover plugins
state.set("plugin_resolver_opts", {
    device_root = device_root,
    repo_root = repo_root,
    profile = active_profile,
})

if device_root or repo_root then
    local unified = ConfigResolver.resolve_all({
        device_root = device_root,
        repo_root = repo_root,
        profile = active_profile,
        require_agent = false,  -- plugin discovery doesn't need agent session
    })

    if unified and unified.plugins then
        for _, plugin in ipairs(unified.plugins) do
            if loader.load_plugin(plugin.init_path, plugin.name) then
                loaded_plugin_names[plugin.name] = true
                plugin_registry[plugin.name] = { path = plugin.init_path }
            end
        end
    end
end

if not next(loaded_plugin_names) then
    log.debug("No plugins found")
end

-- Watch core modules and plugin directories for hot-reload on file changes
safe_require("handlers.module_watcher")

-- ============================================================================
-- Agent Improvements (Sandboxed)
-- ============================================================================
-- Load agent-written improvements with restricted access.
-- These run in a sandbox: no process spawn, no keyring, fs restricted
-- to the improvements directory only.

local improvements_dir = config.lua_path() .. "/improvements"
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
