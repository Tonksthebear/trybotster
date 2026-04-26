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
safe_require("lib.session")
safe_require("lib.agent")
safe_require("lib.accessory")
safe_require("lib.commands")
_G.mcp = safe_require("lib.mcp")

-- Install plugin.db{} BEFORE any plugin loads. This wires `_G.plugin.db`
-- and subscribes to `plugin_unloading` + `shutdown` so cached sqlite
-- connections close cleanly on reload and on hub exit.
local plugin_db_mod = safe_require("lib.plugin_db")
if plugin_db_mod and type(plugin_db_mod.install) == "function" then
    plugin_db_mod.install()
end

-- UI DSL transport libs. `lib.action` is the registry for browser-emitted
-- UI action envelopes; `lib.tree_snapshot` (formerly layout_broadcast) wraps
-- `web_layout.render(...)` with global per-(surface,subpath) hash dedup.
-- Wire protocol dropped per-subscription dedup — selection is client-side
-- now, so the same tree ships to every subscriber and dedup is global.
_G.action = safe_require("lib.action")

-- Phase 4a: surface registry. Must load BEFORE `lib.tree_snapshot` so the
-- broadcast module can see the registry, and BEFORE `handlers.connections`
-- so the `surfaces_changed` hook subscription lands on the real table. The
-- surfaces global lets plugin authors call `surfaces.register(name, opts)`
-- without boilerplate. `hub.builtin_surfaces` registers workspace_sidebar /
-- workspace_panel so the workspace isn't special-cased anywhere else.
_G.surfaces = safe_require("lib.surfaces")
safe_require("hub.builtin_surfaces")

safe_require("lib.tree_snapshot")

-- ============================================================================
-- Wire protocol — entity broadcast registry
-- ============================================================================
-- Load EB and register every built-in entity type BEFORE handlers/connections
-- so `Session:update` (which calls EB.patch) and the agent_created/deleted
-- hooks (which call EB.upsert/remove) always land on a populated registry.
-- Plugins can also register their own entity types after this point via
-- `EB.register("<plugin>.<type>", { id_field, all, filter? })`.

local EB = safe_require("lib.entity_broadcast")
if EB then
    local Session = require("lib.session")
    local Agent = require("lib.agent")
    local ClientSessionPayload = require("lib.client_session_payload")

    EB.register("session", {
        id_field = "session_uuid",
        all = function()
            return ClientSessionPayload.build_many(Session.all_info())
        end,
        filter = function(info)
            return not Session.is_system_session(info)
        end,
    })
    EB.register("workspace", {
        id_field = "workspace_id",
        all = function()
            local Hub = require("lib.hub")
            local ok, workspaces = pcall(function()
                return Hub.get():list_workspaces()
            end)
            return ok and workspaces or {}
        end,
    })
    EB.register("spawn_target", {
        id_field = "target_id",
        all = function()
            local registry = rawget(_G, "spawn_targets")
            if not registry or type(registry.list) ~= "function" then
                return {}
            end
            local ok, listed = pcall(registry.list)
            if not ok or type(listed) ~= "table" then return {} end
            local out = {}
            for _, target in ipairs(listed) do
                local merged = target
                if type(registry.inspect) == "function" and target.path then
                    local inspect_ok, inspection = pcall(registry.inspect, target.path)
                    if inspect_ok and type(inspection) == "table" then
                        merged = {}
                        for k, v in pairs(target) do merged[k] = v end
                        for k, v in pairs(inspection) do merged[k] = v end
                    end
                end
                out[#out + 1] = merged
            end
            return out
        end,
    })
    EB.register("worktree", {
        id_field = "worktree_path",
        all = function()
            local worktrees = hub.get_worktrees()
            return worktrees or {}
        end,
    })
    EB.register("hub", {
        id_field = "hub_id",
        all = function()
            -- Prefer the server-assigned botster_id; fall back to the local
            -- hub_identifier so fresh / unregistered hubs still ship a stable
            -- snapshot. Rust's hub_recovery_state event already mirrors this
            -- choice via Hub::server_hub_id() so the in-flight `recovery.hub_id`
            -- typically wins the merge below.
            local hub_id = (hub.server_id and hub.server_id())
                or (hub.hub_id and hub.hub_id())
                or nil
            local recovery = state.get("connections.hub_recovery_state", { state = "starting" })
            local payload = { hub_id = hub_id }
            for k, v in pairs(recovery) do payload[k] = v end
            if type(payload.hub_id) ~= "string" or payload.hub_id == "" then
                return {}
            end
            return { payload }
        end,
    })
    EB.register("connection_code", {
        id_field = "hub_id",
        all = function()
            local hub_id = hub.server_id and hub.server_id() or nil
            local code = state.get("connections.last_connection_code", nil)
            -- state.get auto-inits nil default to `{}`, so `type == "table"`
            -- is not sufficient — also check `next(code) ~= nil` so a hub
            -- that never fired connection_code_ready / _error reports an
            -- empty snapshot instead of a bare `{hub_id}` entity.
            if not hub_id or type(code) ~= "table" or next(code) == nil then
                return {}
            end
            local payload = { hub_id = hub_id }
            for k, v in pairs(code) do payload[k] = v end
            return { payload }
        end,
    })
end

-- Phase 4a demo plugin. The real plugin loader (ConfigResolver below) walks
-- the device root and admitted spawn target repos; it does NOT scan
-- cli/lua/plugins/. That's deliberate — shipped demo plugins load here so
-- the substrate is always exercised, without putting user plugins on the
-- Lua `package.path`. A plugin author follows the same `surfaces.register`
-- contract regardless of where their `plugin.lua` lives.
--
-- Gated so production hubs don't ship the `/plugins/hello` route to real
-- users. Dev hubs (BOTSTER_DEV=1) and test hubs (BOTSTER_ENV=test) still
-- register the demo so the substrate is exercisable end-to-end in those
-- environments. Matches the DEV_ENV_VAR convention used by
-- `cli/src/lua/primitives/web_layout.rs` for override directory selection.
local demo_env = os.getenv("BOTSTER_DEV") == "1"
    or os.getenv("BOTSTER_ENV") == "test"
if demo_env then
    safe_require("plugins.hello_surface.plugin")
else
    log.debug("plugins/hello_surface skipped (BOTSTER_DEV!=1 and BOTSTER_ENV!=test)")
end

-- Register built-in default MCP prompts. Loaded here so they are available
-- before user.init runs, allowing users to override them by re-registering
-- prompts with the same name (last registration wins).
safe_require("hub.mcp_defaults")

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
-- Plugin Loading (device + spawn targets)
-- ============================================================================
-- Load plugins from device root and from each admitted spawn target's repo root.
-- Repo-level plugins (e.g. .botster/plugins/github/) need to load at hub startup
-- so they can subscribe to ActionCable channels and handle messages immediately.

local ConfigResolver = require("lib.config_resolver")
local state = require("hub.state")
local plugin_registry = state.get("plugin_registry", {})
local loaded_plugin_names = {}

local device_root = config.data_dir and config.data_dir() or nil

-- Store resolver opts so plugin watcher/reload can re-discover plugins
state.set("plugin_resolver_opts", {
    device_root = device_root,
})

-- Run migration if old structure detected
if ConfigResolver.needs_migration(device_root, nil) then
    log.info("Legacy config structure detected, running migration...")
    local mig_ok, mig_err = pcall(ConfigResolver.migrate, device_root, nil)
    if not mig_ok then
        log.warn(string.format("Config migration error: %s", tostring(mig_err)))
    end
end

-- Collect repo roots from admitted spawn targets
local target_repo_roots = {}
local target_registry = rawget(_G, "spawn_targets")
if target_registry and type(target_registry.list) == "function" then
    local ok, targets = pcall(target_registry.list)
    if ok and type(targets) == "table" then
        for _, target in ipairs(targets) do
            if target.enabled ~= false and target.path then
                target_repo_roots[target.path] = true
            end
        end
    end
end

local function load_plugins_from_resolved(unified)
    if not unified or not unified.plugins then return end
    for _, plugin in ipairs(unified.plugins) do
        if not plugin_registry[plugin.name] then
            plugin_registry[plugin.name] = {
                path = plugin.init_path,
                status = "pending",
                reload_count = 0,
            }

            if loader.is_disabled(plugin.name) then
                plugin_registry[plugin.name].status = "disabled"
                log.info(string.format("Plugin disabled, skipping: %s", plugin.name))
            else
                local load_ok, load_err = loader.load_plugin(plugin.init_path, plugin.name)
                if load_ok then
                    loaded_plugin_names[plugin.name] = true
                    plugin_registry[plugin.name].status = "loaded"
                    plugin_registry[plugin.name].loaded_at = os.time()
                    plugin_registry[plugin.name].reload_count = 1
                else
                    plugin_registry[plugin.name].status = "errored"
                    plugin_registry[plugin.name].error = load_err
                    plugin_registry[plugin.name].error_at = os.time()
                end
            end
        end
    end
end

-- Load device-level plugins
if device_root then
    local unified = ConfigResolver.resolve_all({
        device_root = device_root,
        repo_root = nil,
        require_agent = false,
    })
    load_plugins_from_resolved(unified)
end

-- Load repo-level plugins from each spawn target.
-- Set _loading_plugin_repo_root so hub.detect_repo() can resolve the repo
-- from the target path (the hub's CWD is $HOME, not a repo directory).
for repo_root, _ in pairs(target_repo_roots) do
    _G._loading_plugin_repo_root = repo_root
    local unified = ConfigResolver.resolve_all({
        device_root = nil,
        repo_root = repo_root,
        require_agent = false,
    })
    load_plugins_from_resolved(unified)
end
_G._loading_plugin_repo_root = nil

if not next(loaded_plugin_names) then
    log.debug("No plugins found")
end

-- ============================================================================
-- Workspace Store Init + Migration (Phase 1: Central Session Store)
-- ============================================================================
-- Ensure the workspaces directory exists and migrate any old context.json files
-- before session recovery runs so resurrected sessions find new-format manifests.

do
    local ws_data_dir = (config.data_dir and config.data_dir()) or nil
    if ws_data_dir then
        local ws_ok, ws = pcall(require, "lib.workspace_store")
        if ws_ok then
            -- Ensure top-level workspaces/ directory exists.
            pcall(ws.init_dir, ws_data_dir)
            -- Migrate any surviving legacy context.json files once, then delete them.
            -- Idempotent: already-migrated files are absent and the scan is a no-op.
            local mig_ok, mig_err = pcall(ws.migrate, ws_data_dir)
            if not mig_ok then
                log.warn(string.format("Workspace store migration error: %s", tostring(mig_err)))
            end
            -- Convert v1 workspace manifests (repo/issue_number) to name format.
            local mig2_ok, mig2_err = pcall(ws.migrate_v2, ws_data_dir)
            if not mig2_ok then
                log.warn(string.format("Workspace store v2 migration error: %s", tostring(mig2_err)))
            end
            -- Convert v2 workspace manifests (dedup_key/title) to v3 (name).
            local mig3_ok, mig3_err = pcall(ws.migrate_v3, ws_data_dir)
            if not mig3_ok then
                log.warn(string.format("Workspace store v3 migration error: %s", tostring(mig3_err)))
            end
        else
            log.warn(string.format("Could not load lib.workspace_store: %s", tostring(ws)))
        end
    else
        log.debug("No data_dir configured; skipping workspace store init and migration")
    end
end

-- Load session recovery handler (reconnects to surviving session processes on Hub restart)
safe_require("handlers.session_recovery")

-- Watch core Lua modules for hot-reload (plugins use explicit reload)
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
