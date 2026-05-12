-- UI Action envelope registry (Phase 2b).
--
-- Plugins register handlers for semantic action ids emitted by browser
-- clients via the `ui_action` message type. Every handler in the chain
-- runs (handlers observe; they don't short-circuit each other). A handler
-- that wants to suppress the legacy-command fallback must return
-- `action.HANDLED` or `action.result{...}`. Any other return value —
-- including `nil`, `false`, or an unmarked table — is treated as
-- "observed, not consumed", so the
-- fallback still fires for action ids that have one.
--
-- This matters because the common plugin shape is an observer (log the
-- envelope, maybe forward somewhere, and return nothing). Without the
-- sentinel rule, adding ANY observer for `botster.session.select` would
-- silently disable the legacy `select_agent` command and regress Phase 1
-- selection behavior.
--
-- Envelope shape (from cross-client-ui-primitives.md):
--
--     { id = "botster.session.select", payload = { sessionUuid = "..." } }
--
-- Fallback map: if no handler returned `HANDLED`, a curated set of action
-- ids forwards to the corresponding legacy hub command. Most `.request`
-- actions have no hub fallback — those remain browser-local per spec.

local state = require("hub.state")

-- Registry is `action_id -> { { name = ..., handler = ... }, ... }` so the
-- same id can have multiple ordered handlers.
local registry = state.get("ui_action_registry", {})

local M = {}

local function current_plugin_key()
    local key = rawget(_G, "_loading_plugin_key") or rawget(_G, "_loading_plugin_name")
    if type(key) == "string" and key ~= "" then return key end
    return nil
end

local function worker_ctx(ctx)
    ctx = ctx or {}
    return {
        sub_id = ctx.sub_id,
        target_surface = ctx.target_surface,
        action_request_id = ctx.action_request_id,
    }
end

--- Sentinel returned by a handler to claim ownership of an envelope and
--- suppress the legacy-command fallback. Kept as a unique table so it
--- cannot collide with any user-defined truthy value (e.g., a boolean
--- `true` a plugin might reasonably return for diagnostic flow).
M.HANDLED = setmetatable({}, { __tostring = function() return "action.HANDLED" end })

--- Return from a handler to claim an action and attach result metadata for
--- the browser's generic pending/result lifecycle.
function M.result(fields)
    fields = fields or {}
    fields.__ui_action_result = true
    return fields
end

-- -------------------------------------------------------------------------
-- Fallback routing — semantic action id -> legacy hub command + payload map
-- -------------------------------------------------------------------------
--
-- Only actions whose Phase-1 adapter behavior ends in `hub.*(...)` map here.
-- `.request` actions open browser modals and are handled locally; their hub
-- follow-up is a SEPARATE, fully-resolved command (e.g. `rename_workspace`
-- with the final name) that arrives via the legacy hub channel and is not
-- re-routed through this registry.

--- Build the fallback command table for a given envelope. Returns `{ type,
--- ... }` suitable for `commands.dispatch`, or `nil` if the action has no
--- hub-side fallback.
local function fallback_command(envelope)
    local payload = envelope.payload or {}
    local id = envelope.id

    if id == "botster.session.select" then
        return {
            type = "select_agent",
            id = payload.sessionUuid or payload.sessionId,
            session_uuid = payload.sessionUuid,
            session_id = payload.sessionId,
        }
    end

    if id == "botster.session.action.execute" then
        return {
            type = "execute_session_action",
            session_uuid = payload.sessionUuid,
            action_id = payload.actionId,
            params = payload.params,
        }
    end

    -- Everything else: local UI concern (toggle/rename.request/move.request/
    -- delete.request/preview.open/menu.open/create.request). Browser handles
    -- locally; hub follow-up commands arrive as their own legacy commands.
    return nil
end

-- -------------------------------------------------------------------------
-- Public API
-- -------------------------------------------------------------------------

--- Register a handler for a semantic action id.
---
--- Handlers receive `(envelope, ctx)` where `ctx` is `{ client, sub_id,
--- target_surface, action_request_id }` and may be used to send responses on the same channel.
---
--- All registered handlers run for every envelope (observer semantics).
--- Return `action.HANDLED` to suppress the legacy-command fallback; return
--- `action.result{ ok = false, error = "..." }` to suppress fallback and
--- attach generic result metadata. Other values leave the fallback path intact.
---
--- Idempotent: registering the same `name` twice replaces the earlier entry
--- so hot-reloadable modules can re-register without duplication.
---
-- @param action_id string Semantic Botster action id (e.g. "botster.session.select")
-- @param name string Caller-provided tag for replace-in-place semantics
-- @param handler function (envelope, ctx) -> action.HANDLED|any|nil
function M.on(action_id, name, handler, opts)
    assert(type(action_id) == "string", "action_id must be a string")
    assert(type(name) == "string", "name must be a string")
    assert(type(handler) == "function", "handler must be a function")
    opts = opts or {}

    local slot = registry[action_id] or {}
    local owner_plugin = current_plugin_key()
    -- Replace existing entry with the same name so repeated calls from a
    -- hot-reloaded module don't stack handlers.
    for i, entry in ipairs(slot) do
        if entry.name == name then
            slot[i] = {
                name = name,
                handler = handler,
                owner_plugin = owner_plugin,
                timeout_ms = opts.timeout_ms or 2000,
            }
            registry[action_id] = slot
            log.debug(string.format("action.on: replaced handler %s for %s", name, action_id))
            return
        end
    end
    slot[#slot + 1] = {
        name = name,
        handler = handler,
        owner_plugin = owner_plugin,
        timeout_ms = opts.timeout_ms or 2000,
    }
    registry[action_id] = slot
    log.debug(string.format("action.on: registered handler %s for %s", name, action_id))
end

--- Remove a handler by action id + name. Returns true iff a handler was
--- removed. Safe to call from plugin teardown.
-- @param action_id string
-- @param name string
function M.off(action_id, name)
    local slot = registry[action_id]
    if not slot then return false end
    for i, entry in ipairs(slot) do
        if entry.name == name then
            table.remove(slot, i)
            if #slot == 0 then registry[action_id] = nil end
            return true
        end
    end
    return false
end

function M._invoke_registered(action_id, name, envelope, ctx)
    local slot = registry[action_id]
    if not slot then error("ui action not registered: " .. tostring(action_id)) end
    for _, entry in ipairs(slot) do
        if entry.name == name then
            local result = entry.handler(envelope, ctx or {})
            if result == M.HANDLED then
                return { __plugin_action_status = "handled" }
            end
            if type(result) == "table" and result.__ui_action_result == true then
                result.__plugin_action_status = "result"
                return result
            end
            return { __plugin_action_status = "observed" }
        end
    end
    error("ui action handler not registered: " .. tostring(action_id) .. ":" .. tostring(name))
end

function M.unregister_by_plugin(plugin_key)
    if type(plugin_key) ~= "string" or plugin_key == "" then return 0 end
    local removed = 0
    for action_id, slot in pairs(registry) do
        local keep = {}
        for _, entry in ipairs(slot) do
            if entry.owner_plugin == plugin_key then
                removed = removed + 1
            else
                keep[#keep + 1] = entry
            end
        end
        if #keep == 0 then
            registry[action_id] = nil
        elseif #keep ~= #slot then
            registry[action_id] = keep
        end
    end
    return removed
end

--- List registered action ids (for introspection and tests).
-- @return table array of action ids
function M.registered_ids()
    local ids = {}
    for id in pairs(registry) do ids[#ids + 1] = id end
    table.sort(ids)
    return ids
end

--- Dispatch an envelope through every registered handler, then route to
--- the Phase-1 fallback command when the action id has one AND no handler
--- claimed ownership via `action.HANDLED`.
---
--- Semantics:
---   * Every registered handler runs; a handler that raises is logged and
---     the chain continues so one broken plugin doesn't shadow others.
---   * A handler returning `action.HANDLED` records ownership. Multiple
---     handlers may return `HANDLED`; any occurrence suppresses fallback.
---   * All other return values (nil, false, arbitrary tables) are treated
---     as observation — fallback still fires when present.
---
--- ctx typically carries `{ client, sub_id, target_surface }` so handlers
--- can reply on the same subscription channel; tests may pass `{}`.
---
--- Returns a table `{ handled, via, handler_count, handled_count, ok }` for
--- callers/tests that need to observe the dispatch outcome. `via` is one
--- of "handler" (any handler returned HANDLED), "fallback" (legacy command
--- dispatched), or "unhandled" (neither). `handler_count` counts all
--- handlers that ran; `handled_count` counts those that returned HANDLED.
-- @param envelope table { id, payload?, disabled? }
-- @param ctx table? optional routing context
-- @return table { handled = bool, via, ok, handler_count, handled_count }
function M.dispatch(envelope, ctx)
    if type(envelope) ~= "table" or type(envelope.id) ~= "string" then
        log.warn("action.dispatch: invalid envelope (missing .id)")
        return {
            handled = false, via = "unhandled", ok = false,
            error = "Invalid UI action envelope.",
            handler_count = 0, handled_count = 0,
        }
    end
    ctx = ctx or {}

    local handler_count = 0
    local handled_count = 0
    local ok_result = true
    local message = nil
    local error_message = nil
    local navigate = nil
    local presentation = nil

    local slot = registry[envelope.id]
    if slot then
        local supervisor = require("lib.plugin_supervisor")
        for _, entry in ipairs(slot) do
            handler_count = handler_count + 1
            local ok, result = supervisor.invoke(
                entry.owner_plugin,
                "ui_action:" .. envelope.id .. ":" .. entry.name,
                entry.handler,
                {
                    timeout_ms = entry.timeout_ms,
                    handler_kind = "ui_action",
                    handler_id = envelope.id,
                    handler_name = entry.name,
                    payload = { envelope = envelope, ctx = worker_ctx(ctx) },
                },
                envelope,
                ctx)
            if not ok then
                log.warn(string.format(
                    "action handler %s for %s raised: %s",
                    entry.name, envelope.id, tostring(result)))
                ok_result = false
                error_message = tostring(result)
            elseif type(result) == "table" and result.__plugin_action_status == "handled" then
                handled_count = handled_count + 1
            elseif type(result) == "table" and result.__plugin_action_status == "result" then
                handled_count = handled_count + 1
                if result.ok == false then ok_result = false end
                if type(result.message) == "string" then message = result.message end
                if type(result.error) == "string" then error_message = result.error end
                if type(result.navigate) == "table" then navigate = result.navigate end
                if type(result.presentation) == "table" then presentation = result.presentation end
            elseif result == M.HANDLED then
                handled_count = handled_count + 1
            elseif type(result) == "table" and result.__ui_action_result == true then
                handled_count = handled_count + 1
                if result.ok == false then ok_result = false end
                if type(result.message) == "string" then message = result.message end
                if type(result.error) == "string" then error_message = result.error end
                if type(result.navigate) == "table" then navigate = result.navigate end
                if type(result.presentation) == "table" then presentation = result.presentation end
            end
            -- Any other return value: observed but not consumed. Fall
            -- through to remaining handlers (observer semantics).
        end
    end

    if handled_count > 0 then
        return {
            handled = true, via = "handler", ok = ok_result,
            message = message, error = error_message, navigate = navigate, presentation = presentation,
            handler_count = handler_count, handled_count = handled_count,
        }
    end

    -- Fallback: mirror Phase-1 behavior for action ids the browser already
    -- knew how to emit as legacy hub commands. Runs regardless of whether
    -- observers exist, as long as none claimed HANDLED.
    local cmd = fallback_command(envelope)
    if cmd then
        local commands = require("lib.commands")
        local ok, err = pcall(commands.dispatch, ctx.client, ctx.sub_id, cmd)
        return {
            handled = ok, via = "fallback", ok = ok and ok_result,
            error = ok and error_message or tostring(err),
            handler_count = handler_count, handled_count = handled_count,
        }
    end

    if handler_count == 0 then
        log.debug(string.format("action.dispatch: unhandled %s", envelope.id))
    end
    return {
        handled = false, via = "unhandled", ok = false,
        error = error_message or "No hub handler handled this action.",
        handler_count = handler_count, handled_count = handled_count,
    }
end

--- Emit an envelope from hub-side code. Currently a thin wrapper over
--- `dispatch` so hub plugins can simulate browser-initiated actions without
--- needing a client. Kept for API symmetry with `action.on`; may grow a
--- broadcast path in future phases if TUI needs to observe hub-emitted
--- actions.
-- @param envelope table
-- @param ctx table?
function M.emit(envelope, ctx)
    return M.dispatch(envelope, ctx or {})
end

-- -------------------------------------------------------------------------
-- Hot-reload lifecycle
-- -------------------------------------------------------------------------

function M._before_reload()
    log.info(string.format(
        "action.lua reloading (%d registered ids)", #M.registered_ids()))
end

function M._after_reload()
    log.info("action.lua reloaded")
end

-- Test-only: wipe the registry. Not exposed on the public surface; reached
-- via `require("lib.action")._reset_for_tests()`.
function M._reset_for_tests()
    for k in pairs(registry) do registry[k] = nil end
end

return M
