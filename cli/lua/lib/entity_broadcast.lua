-- Entity broadcast registry — wire protocol (delta) source of truth.
--
-- Replaces the current "rebuild + broadcast every UiNode tree on any state
-- change" pattern with "snapshot on explicit request, ship per-entity field
-- deltas thereafter." Each entity type (`session`, `workspace`,
-- `spawn_target`, `worktree`, `hub`, `connection_code`, plus plugin types
-- namespaced as `<plugin>.<type>`) registers its `id_field` and an `all()`
-- snapshot source. Mutators (`upsert`/`patch`/`remove`) construct one of the
-- four wire envelopes and hand them to the connection-layer broadcaster.
--
-- Wire envelopes (all carry `v = 2`):
--   { type = "entity_snapshot", entity_type, items, snapshot_seq }
--   { type = "entity_scoped_snapshot", entity_type, scope, items, snapshot_seq }
--   { type = "entity_upsert",   entity_type, id, entity, snapshot_seq }
--   { type = "entity_patch",    entity_type, id, patch, snapshot_seq }
--   { type = "entity_remove",   entity_type, id, snapshot_seq }
--
-- `snapshot_seq` is monotonic per entity type per hub process, seeded from a
-- wall-clock boot epoch so a reboot does not restart at 0 and trip older
-- reconnecting clients that still gate snapshots by sequence. Clients keep
-- their own `last_snapshot_seq` per type and drop out-of-order deltas. When a
-- client needs a baseline, it requests the relevant type(s) through
-- `send_snapshots_to`, which sends authoritative `entity_snapshot` frames.
--
-- The module owns NO transport — `set_broadcaster(fn)` injects the per-frame
-- send hook (wired up by `cli/lua/handlers/connections.lua` at load time).
-- That keeps EB pure-data and lets the integration test harness substitute
-- a capturing broadcaster without the full client/transport stack.
--
-- Hot-reload contract:
--   * `seq_by_type` lives in `hub.state` so reload preserves the monotonic
--     counter across module re-evaluation. Bumping the counter mid-reload
--     would cause clients to silently drop the next delta.
--   * `registry` is intentionally NOT persisted: each provider module
--     (Session, Hub, plugins…) re-registers in its own `_after_reload`,
--     and re-registration overwrites the function references that would
--     otherwise dangle if their owning module reloaded independently.
--   * `broadcaster` is similarly transient — `connections.lua` installs it
--     at load time, and this module asks the already-loaded connection layer
--     to reinstall it after an `entity_broadcast.lua` reload so entity deltas
--     keep flowing even when only this module changed.

local state = require("hub.state")

local M = {}

local PERF = os.getenv("BOTSTER_LUA_PERF") == "1"

local function elapsed_ms(started)
    return math.floor(((os.clock() - started) * 1000) + 0.5)
end

local function log_perf(message)
    if PERF and log and log.info then
        log.info("[PERF][entity_snapshot] " .. message)
    end
end

local BUILTIN_ENTITY_TYPES = {
    session = true,
    session_action = true,
    workspace = true,
    spawn_target = true,
    worktree = true,
    hub = true,
    connection_code = true,
    template = true,
}

-- entity_type -> {
--   id_field = string,
--   all = function,
--   query = function?,
--   filter = function?,
--   owner_plugin = string?,
--   default = bool?,
-- }
local registry = {}
local replay_registry = state.get("entity_broadcast.replay_registry", {})

-- entity_type -> integer (monotonic per hub process)
local seq_by_type = state.get("entity_broadcast.seq_by_type", {})

-- frame -> () . Defaults to a no-op so EB.upsert/patch/remove called before
-- connections.lua wires the real broadcaster simply drop the frame instead
-- of throwing. The unit-test harness substitutes a capturing closure.
local broadcaster = function(_frame) end

-- -------------------------------------------------------------------------
-- Internal helpers
-- -------------------------------------------------------------------------

local function next_seq(entity_type)
    local current = seq_by_type[entity_type]
    if type(current) ~= "number" then current = M.seq_epoch() end
    local n = current + 1
    seq_by_type[entity_type] = n
    state.set("entity_broadcast.seq_by_type", seq_by_type)
    return n
end

local function current_seq(entity_type)
    local n = seq_by_type[entity_type]
    if type(n) == "number" then return n end
    return M.seq_epoch()
end

local function get_entry(entity_type, op_label)
    local entry = registry[entity_type]
    if not entry then
        log.warn(string.format(
            "entity_broadcast.%s: type %q not registered",
            tostring(op_label or "op"), tostring(entity_type)))
        return nil
    end
    return entry
end

local function plugin_type_parts(entity_type)
    if type(entity_type) ~= "string" then return nil, nil end
    local plugin_name, type_name = entity_type:match("^([%w_-]+)%.([%w_][%w_.-]*)$")
    if not plugin_name or not type_name then return nil, nil end
    if type_name:find("..", 1, true) or type_name:sub(-1) == "." then
        return nil, nil
    end
    return plugin_name, type_name
end

local function plugin_entity_type_owner(entity_type)
    local plugin_name = plugin_type_parts(entity_type)
    return plugin_name
end

local function loading_plugin_name()
    -- Entity namespaces use the manifest/display name, not the loader key:
    -- repo-sourced plugin keys include paths (`repo:/...:name`) that are not
    -- valid wire namespaces.
    local display_name = rawget(_G, "_loading_plugin_display_name")
    if type(display_name) == "string" and display_name ~= "" then return display_name end
    local name = rawget(_G, "_loading_plugin_name")
    if type(name) == "string" and name ~= "" then return name end
    return nil
end

local function normalize_owner_plugin(owner_plugin)
    if type(owner_plugin) == "string" and owner_plugin ~= "" then return owner_plugin end
    return loading_plugin_name()
end

local function register_owner(entity_type, opts)
    local namespace = plugin_type_parts(entity_type)
    if not namespace then
        if BUILTIN_ENTITY_TYPES[entity_type] then return nil end
        error(string.format(
            "entity_broadcast.register: entity_type %q must be a reserved built-in type or plugin-owned <plugin>.<type>",
            tostring(entity_type)), 3)
    end

    local owner = opts.owner_plugin or opts.plugin or loading_plugin_name()
    if type(owner) ~= "string" or owner == "" then
        error(string.format(
            "entity_broadcast.register: plugin entity_type %q requires owner_plugin or plugin load context",
            entity_type), 3)
    end
    if owner ~= namespace then
        error(string.format(
            "entity_broadcast.register: plugin entity_type %q must be owned by namespace %q, got %q",
            entity_type, namespace, owner), 3)
    end
    if opts.id_field ~= "id" then
        error(string.format(
            "entity_broadcast.register: plugin entity_type %q must use id_field=\"id\"",
            entity_type), 3)
    end
    return owner
end

local function assert_plugin_publish_owner(entity_type, owner_plugin, op_label)
    local namespace = plugin_entity_type_owner(entity_type)
    if not namespace then
        error(string.format(
            "entity_broadcast.%s: entity_type %q must be plugin-owned <plugin>.<type>",
            tostring(op_label or "publish"), tostring(entity_type)), 3)
    end

    local owner = normalize_owner_plugin(owner_plugin)
    if type(owner) ~= "string" or owner == "" then
        error(string.format(
            "entity_broadcast.%s: plugin entity_type %q requires owner_plugin or plugin load context",
            tostring(op_label or "publish"), entity_type), 3)
    end
    if owner ~= namespace then
        error(string.format(
            "entity_broadcast.%s: plugin entity_type %q must be owned by namespace %q, got %q",
            tostring(op_label or "publish"), entity_type, namespace, owner), 3)
    end

    local entry = get_entry(entity_type, op_label)
    if not entry then
        error(string.format(
            "entity_broadcast.%s: plugin entity_type %q is not registered",
            tostring(op_label or "publish"), entity_type), 3)
    end
    if entry.owner_plugin ~= owner then
        error(string.format(
            "entity_broadcast.%s: plugin entity_type %q is registered to owner %q, got %q",
            tostring(op_label or "publish"), entity_type,
            tostring(entry.owner_plugin), owner), 3)
    end

    return entry, owner
end

-- Resolve the entity id from a payload using the registered id_field, with
-- `id` as a fallback. Returns nil + warns when neither is present so the
-- caller can drop the frame instead of shipping an unidentified entity.
local function resolve_id(entry, payload, op_label)
    if type(payload) ~= "table" then return nil end
    local id = payload[entry.id_field] or payload.id
    if type(id) ~= "string" or id == "" then
        log.warn(string.format(
            "entity_broadcast.%s: payload missing id (id_field=%q)",
            tostring(op_label or "op"), entry.id_field))
        return nil
    end
    return id
end

local function validate_id(id, entity_type, op_label)
    if type(id) ~= "string" or id == "" then
        error(string.format(
            "entity_broadcast.%s: entity id for %q must be a non-empty string",
            tostring(op_label or "publish"), tostring(entity_type)), 3)
    end
end

local function validate_payload_id(entry, entity_type, payload, op_label)
    if type(payload) ~= "table" then
        error(string.format(
            "entity_broadcast.%s: entity for %q must be a table",
            tostring(op_label or "publish"), tostring(entity_type)), 3)
    end
    local id = payload[entry.id_field] or payload.id
    validate_id(id, entity_type, op_label)
    return id
end

local function validate_snapshot_items(entry, entity_type, items, op_label)
    if type(items) ~= "table" then
        error(string.format(
            "entity_broadcast.%s: items for %q must be a table",
            tostring(op_label or "snapshot"), tostring(entity_type)), 3)
    end

    local out = {}
    for index, item in ipairs(items) do
        if type(item) ~= "table" then
            error(string.format(
                "entity_broadcast.%s: item %d for %q must be a table",
                tostring(op_label or "snapshot"), index, tostring(entity_type)), 3)
        end
        validate_payload_id(entry, entity_type, item, op_label or "snapshot")
        out[#out + 1] = item
    end
    return out
end

local function emit(frame)
    -- Wrap broadcaster in pcall so a buggy transport hook can't take down
    -- the mutator path. Callers (Session:update, EB.patch from idle timer,
    -- etc.) are not expected to handle send failures.
    local ok, err = pcall(broadcaster, frame)
    if not ok then
        log.warn(string.format(
            "entity_broadcast: broadcaster threw on %s/%s: %s",
            tostring(frame.entity_type), tostring(frame.type), tostring(err)))
    end
end

-- -------------------------------------------------------------------------
-- Public API: registration
-- -------------------------------------------------------------------------

--- Install the per-frame transport hook. `fn(frame)` is invoked once per
--- emitted entity_snapshot/upsert/patch/remove with a Lua table ready to be
--- json-encoded and shipped to every hub-channel subscriber. Passing nil
--- restores the no-op default.
function M.set_broadcaster(fn)
    if fn == nil then
        broadcaster = function(_frame) end
        return
    end
    assert(type(fn) == "function", "entity_broadcast.set_broadcaster requires a function")
    broadcaster = fn
end

--- Sequence floor for this hub process.
---
--- The value is persisted in hub.state across Lua hot-reloads, but recomputed
--- on a real hub process reboot. Using an epoch-sized floor keeps fresh
--- snapshots greater than any ordinary pre-reboot delta sequence, which
--- protects clients that have not yet learned that snapshots are
--- authoritative resyncs.
function M.seq_epoch()
    local n = state.get("entity_broadcast.seq_epoch")
    if type(n) ~= "number" then
        n = os.time() * 1000
        state.set("entity_broadcast.seq_epoch", n)
    end
    return n
end

--- Register an entity type.
---
--- @param entity_type string Wire identifier (e.g. "session", "kanban.board").
--- @param opts table {
---   id_field = string,        -- payload field that supplies the entity id
---   all = function -> array,  -- snapshot source called on request
---   query = function? -> array, -- targeted merge-hydration source
---   filter = function? -> bool, -- optional per-item gate (true = include)
---   owner_plugin = string?,   -- required for plugin types outside plugin load
--- }
---
--- Re-registration by the same owner overwrites the prior entry for hot reload.
--- Re-registration by another plugin is rejected so ownership stays explicit.
function M.register(entity_type, opts)
    assert(type(entity_type) == "string" and entity_type ~= "",
        "entity_broadcast.register: entity_type must be a non-empty string")
    assert(type(opts) == "table", "entity_broadcast.register: opts table required")
    assert(type(opts.id_field) == "string" and opts.id_field ~= "",
        "entity_broadcast.register: opts.id_field must be a non-empty string")
    assert(type(opts.all) == "function",
        "entity_broadcast.register: opts.all must be a function")
    if opts.query ~= nil and type(opts.query) ~= "function" then
        error("entity_broadcast.register: opts.query must be a function or nil")
    end
    if opts.filter ~= nil and type(opts.filter) ~= "function" then
        error("entity_broadcast.register: opts.filter must be a function or nil")
    end
    local owner_plugin = register_owner(entity_type, opts)

    local existing = registry[entity_type]
    if existing and existing.owner_plugin and existing.owner_plugin ~= owner_plugin then
        error(string.format(
            "entity_broadcast.register: entity_type %q already owned by plugin %q",
            entity_type, existing.owner_plugin))
    end
    if existing then
        log.warn(string.format(
            "entity_broadcast: re-registering type %q", entity_type))
    end
    registry[entity_type] = {
        id_field = opts.id_field,
        all = opts.all,
        query = opts.query,
        filter = opts.filter,
        owner_plugin = owner_plugin,
        default = opts.default ~= false,
    }
    if owner_plugin then
        replay_registry[entity_type] = registry[entity_type]
    end
end

--- Drop a registration. Used by plugin teardown and by tests.
function M.unregister(entity_type)
    registry[entity_type] = nil
    replay_registry[entity_type] = nil
end

--- Drop all entity types owned by a plugin. Used by plugin unload/hot reload.
function M.unregister_plugin(owner_plugin)
    if type(owner_plugin) ~= "string" or owner_plugin == "" then return 0 end
    local removed = 0
    for entity_type, entry in pairs(registry) do
        if entry.owner_plugin == owner_plugin then
            registry[entity_type] = nil
            replay_registry[entity_type] = nil
            removed = removed + 1
        end
    end
    return removed
end

function M.unregister_by_plugin(plugin_key, metadata)
    metadata = metadata or {}
    local display_name = metadata.name or metadata.plugin_name
    if type(display_name) == "string" and display_name ~= "" then
        return M.unregister_plugin(display_name)
    end
    return M.unregister_plugin(plugin_key)
end

-- -------------------------------------------------------------------------
-- Public API: mutators
-- -------------------------------------------------------------------------

--- Emit `entity_upsert`. Called when a new entity arrives or when the entity
--- record is being replaced wholesale.
--- The payload itself is shipped as `entity` so clients can apply it without
--- re-fetching.
function M.upsert(entity_type, payload)
    local entry = get_entry(entity_type, "upsert")
    if not entry then return end
    if entry.filter then
        local ok, keep = pcall(entry.filter, payload)
        if not ok then
            log.warn(string.format(
                "entity_broadcast.upsert: filter for %q threw: %s",
                entity_type, tostring(keep)))
            return
        end
        if not keep then return end
    end
    local id = resolve_id(entry, payload, "upsert")
    if not id then return end
    emit({
        v = 2,
        type = "entity_upsert",
        entity_type = entity_type,
        id = id,
        entity = payload,
        snapshot_seq = next_seq(entity_type),
    })
end

--- Emit `entity_patch`. `fields` is a sparse table of field names to new
--- values. Clients merge field-by-field into their local entity. Nested
--- objects (e.g. `plugin_state = { ... }`) replace the prior value
--- wholesale rather than deep-merging — see §12.4 of the design brief.
---
--- Empty patches are silently dropped so a noop `Session:update({})` does
--- not consume a snapshot_seq.
function M.patch(entity_type, id, fields)
    local entry = get_entry(entity_type, "patch")
    if not entry then return end
    if type(id) ~= "string" or id == "" then
        log.warn(string.format(
            "entity_broadcast.patch: missing id for %q", entity_type))
        return
    end
    if type(fields) ~= "table" then
        log.warn(string.format(
            "entity_broadcast.patch: patch for %q/%q must be a table",
            entity_type, id))
        return
    end
    if next(fields) == nil then return end
    emit({
        v = 2,
        type = "entity_patch",
        entity_type = entity_type,
        id = id,
        patch = fields,
        snapshot_seq = next_seq(entity_type),
    })
end

--- Emit `entity_remove`. Clients drop the entity from their store and
--- discard any in-flight delta carrying a smaller `snapshot_seq`.
function M.remove(entity_type, id)
    if not get_entry(entity_type, "remove") then return end
    if type(id) ~= "string" or id == "" then
        log.warn(string.format(
            "entity_broadcast.remove: missing id for %q", entity_type))
        return
    end
    emit({
        v = 2,
        type = "entity_remove",
        entity_type = entity_type,
        id = id,
        snapshot_seq = next_seq(entity_type),
    })
end

--- Emit `entity_snapshot` for a registered entity type. This is primarily
--- used by the plugin-facing Hub API after a plugin refreshes its local
--- read model and wants clients to replace their baseline immediately.
function M.snapshot(entity_type, items)
    local entry = get_entry(entity_type, "snapshot")
    if not entry then return end
    local kept = validate_snapshot_items(entry, entity_type, items, "snapshot")
    emit({
        v = 2,
        type = "entity_snapshot",
        entity_type = entity_type,
        items = kept,
        snapshot_seq = next_seq(entity_type),
    })
end

--- Validate that a caller may publish a plugin-owned entity type. Exposed for
--- `lib.hub`, which is the polished plugin API layer over this lower-level
--- broadcaster.
function M.assert_plugin_publish_owner(entity_type, owner_plugin, op_label)
    return assert_plugin_publish_owner(entity_type, owner_plugin, op_label)
end

-- -------------------------------------------------------------------------
-- Snapshot request helpers
-- -------------------------------------------------------------------------

local function registered_type_names(opts)
    opts = opts or {}
    local scope = opts.scope or "all"
    local owner_plugin = opts.owner_plugin
    local include_non_default = opts.include_non_default == true
    local requested = nil
    if type(opts.types) == "table" then
        requested = {}
        for _, name in ipairs(opts.types) do
            if type(name) == "string" and name ~= "" then
                requested[name] = true
            end
        end
    end
    local names = {}
    for name, entry in pairs(registry) do
        if (requested == nil or requested[name])
            and (scope ~= "core" or BUILTIN_ENTITY_TYPES[name])
            and (owner_plugin == nil or entry.owner_plugin == owner_plugin)
            and (requested ~= nil or include_non_default or entry.default ~= false)
        then
            names[#names + 1] = name
        end
    end
    -- Stable order so test assertions and on-the-wire logs are reproducible.
    table.sort(names)
    return names
end

local function sorted_scope_pairs(scope)
    local pairs_out = {}
    if type(scope) ~= "table" then return pairs_out end
    for key, value in pairs(scope) do
        pairs_out[#pairs_out + 1] = tostring(key) .. "=" .. tostring(value)
    end
    table.sort(pairs_out)
    return pairs_out
end

local function snapshot_job_key(sub_id, names, requests)
    local parts = { tostring(sub_id or "__nil__") }
    parts[#parts + 1] = "types"
    for _, name in ipairs(names or {}) do
        parts[#parts + 1] = tostring(name)
    end
    parts[#parts + 1] = "requests"
    for _, request in ipairs(requests or {}) do
        parts[#parts + 1] = tostring(request.entity_type or "")
        if request.id then
            parts[#parts + 1] = "id=" .. tostring(request.id)
        end
        local scope_pairs = sorted_scope_pairs(request.where)
        for _, pair in ipairs(scope_pairs) do
            parts[#parts + 1] = "where:" .. pair
        end
    end
    return table.concat(parts, "\31")
end

local function snapshot_job_key_prefix(sub_id)
    return tostring(sub_id or "__nil__") .. "\31"
end

local function requested_entity_queries(opts)
    opts = opts or {}
    local out = {}
    local raw = opts.requests or opts.entity_requests
    if type(raw) ~= "table" then return out end
    for _, request in ipairs(raw) do
        if #out >= 50 then
            log.warn("entity_broadcast.query: dropping targeted requests over cap=50")
            break
        end
        if type(request) == "table" then
            local entity_type = request.entity_type or request.type
            if type(entity_type) == "string" and entity_type ~= "" then
                local copy = { entity_type = entity_type }
                if type(request.id) == "string" and request.id ~= "" and #request.id <= 256 then
                    copy.id = request.id
                end
                local where = request.where
                if type(where) == "table" then
                    local sanitized = {}
                    local count = 0
                    for key, value in pairs(where) do
                        count = count + 1
                        if count > 8 then
                            log.warn(string.format(
                                "entity_broadcast.query: dropping extra scope keys for %q",
                                entity_type))
                            break
                        end
                        local value_type = type(value)
                        if type(key) == "string" and key ~= ""
                            and (value_type == "string" or value_type == "number" or value_type == "boolean")
                        then
                            sanitized[key] = value
                        end
                    end
                    if next(sanitized) ~= nil then
                        copy.where = sanitized
                    end
                end
                if copy.id and copy.where then
                    log.warn(string.format(
                        "entity_broadcast.query: dropping mixed id+where request for %q",
                        entity_type))
                elseif copy.id or copy.where then
                    out[#out + 1] = copy
                else
                    log.warn(string.format(
                        "entity_broadcast.query: dropping malformed targeted request for %q",
                        entity_type))
                end
            end
        end
    end
    return out
end

local function table_has_entries(value)
    if type(value) ~= "table" then return false end
    return next(value) ~= nil
end

local function item_matches_scope(item, scope)
    if type(item) ~= "table" or type(scope) ~= "table" then return false end
    for key, value in pairs(scope) do
        if item[key] ~= value then return false end
    end
    return true
end

local function snapshot_items(entry, entity_type, context)
    local ok, items = pcall(entry.all, context)
    if not ok then
        log.warn(string.format(
            "entity_broadcast: all() for %q threw: %s",
            entity_type, tostring(items)))
        return {}
    end
    if type(items) ~= "table" then
        log.warn(string.format(
            "entity_broadcast: all() for %q returned %s, expected table",
            entity_type, type(items)))
        return {}
    end
    local kept = {}
    for _, item in ipairs(items) do
        if type(item) ~= "table" then
            log.warn(string.format(
                "entity_broadcast.snapshot: dropping non-table item for %q",
                entity_type))
        elseif resolve_id(entry, item, "snapshot") then
            if not entry.filter then
                kept[#kept + 1] = item
            else
                local ok_f, keep = pcall(entry.filter, item)
                if ok_f and keep then kept[#kept + 1] = item end
            end
        end
    end
    return kept
end

local function query_items(entry, entity_type, request, context)
    if type(entry.query) ~= "function" then
        log.warn(string.format(
            "entity_broadcast.query: %q does not support targeted hydration",
            entity_type))
        -- Unsupported query paths are non-authoritative. Emit no frame rather
        -- than clearing scoped rows or synthesizing removes for a provider that
        -- has not opted into targeted hydration.
        return nil
    end
    local ok, items = pcall(entry.query, request, context)
    if not ok then
        log.warn(string.format(
            "entity_broadcast.query: query() for %q threw: %s",
            entity_type, tostring(items)))
        return {}
    end
    if type(items) ~= "table" then
        log.warn(string.format(
            "entity_broadcast.query: query() for %q returned %s, expected table",
            entity_type, type(items)))
        return {}
    end
    local out = {}
    local requested_id = request.id
    local scope = table_has_entries(request.where) and request.where or nil
    for _, item in ipairs(items) do
        if type(item) ~= "table" then
            log.warn(string.format(
                "entity_broadcast.query: dropping non-table item for %q",
                entity_type))
        else
            local id = resolve_id(entry, item, "query")
            if id then
                local keep = true
                if requested_id and id ~= requested_id then
                    keep = false
                    log.warn(string.format(
                        "entity_broadcast.query: dropping %q item %q that does not match requested id %q",
                        entity_type, id, requested_id))
                end
                if keep and scope and not item_matches_scope(item, scope) then
                    keep = false
                    log.warn(string.format(
                        "entity_broadcast.query: dropping %q item %q outside requested scope",
                        entity_type, id))
                end
                if keep and entry.filter then
                    local ok_f, filter_keep = pcall(entry.filter, item)
                    if not ok_f then
                        keep = false
                        log.warn(string.format(
                            "entity_broadcast.query: filter for %q threw: %s",
                            entity_type, tostring(filter_keep)))
                    elseif not filter_keep then
                        keep = false
                    end
                end
                if keep then out[#out + 1] = item end
            end
        end
    end
    return out
end

--- Send `entity_snapshot` frames to a single subscriber.
---
--- `opts.scope = "core"` restricts snapshots to built-in runtime entity
--- types. Browser and attach-mode TUI can use this for a lightweight hub
--- index so plugin-owned historical stores do not block connect or consume
--- per-client memory before a plugin surface asks for them.
---
--- `opts.types = { ... }` restricts the snapshot to an explicit client request.
local function subscription_is_active(client, sub_id)
    if sub_id == nil then return true end
    local subscriptions = client and client.subscriptions
    if type(subscriptions) ~= "table" then return false end
    return subscriptions[sub_id] ~= nil
end

local function context_for_entry(context, plugin_contexts, entry)
    if entry and entry.owner_plugin then
        local provider_context = plugin_contexts[entry.owner_plugin]
        if not provider_context then
            provider_context = {}
            plugin_contexts[entry.owner_plugin] = provider_context
        end
        return provider_context
    end
    return context
end

local function send_snapshot_type(client, sub_id, entity_type, context, plugin_contexts)
    local started = os.clock()
    local entry = registry[entity_type]
    if not entry then return 0 end

    local items = snapshot_items(entry, entity_type, context_for_entry(context, plugin_contexts, entry))
    local frame = {
        v = 2,
        type = "entity_snapshot",
        entity_type = entity_type,
        items = items,
        snapshot_seq = current_seq(entity_type),
    }
    if sub_id ~= nil then frame.subscriptionId = sub_id end
    local ok_send, send_err = pcall(client.send, client, frame)
    if not ok_send then
        log.warn(string.format(
            "entity_broadcast.snapshot: send failed for type=%s sub=%s: %s",
            tostring(entity_type),
            tostring(sub_id or "nil"),
            tostring(send_err)))
        return 0, false
    end
    local elapsed = elapsed_ms(started)
    log_perf(string.format(
        "type=%s items=%d seq=%s sub=%s elapsed_ms=%d",
        tostring(entity_type),
        #items,
        tostring(frame.snapshot_seq),
        tostring(sub_id or "nil"),
        elapsed))
    if elapsed > 250 then
        log.warn(string.format(
            "entity_broadcast.snapshot_slow: type=%s items=%d sub=%s elapsed_ms=%d",
            tostring(entity_type),
            #items,
            tostring(sub_id or "nil"),
            elapsed))
    end
    log.info(string.format(
        "entity_broadcast.snapshot: type=%s items=%d seq=%s sub=%s",
        tostring(entity_type),
        #items,
        tostring(frame.snapshot_seq),
        tostring(sub_id or "nil")))
    return 1
end

local function send_query_request(client, sub_id, request, context, plugin_contexts)
    local entity_type = request.entity_type
    local started = os.clock()
    local entry = registry[entity_type]
    if not entry then return 0 end

    local items = query_items(entry, entity_type, request, context_for_entry(context, plugin_contexts, entry))
    if items == nil then
        return 0
    end
    local scope = table_has_entries(request.where) and request.where or nil
    if scope then
        local frame = {
            v = 2,
            type = "entity_scoped_snapshot",
            entity_type = entity_type,
            scope = scope,
            items = items,
            snapshot_seq = current_seq(entity_type),
        }
        if sub_id ~= nil then frame.subscriptionId = sub_id end
        local ok_send, send_err = pcall(client.send, client, frame)
        if not ok_send then
            log.warn(string.format(
                "entity_broadcast.query: send failed for type=%s sub=%s: %s",
                tostring(entity_type),
                tostring(sub_id or "nil"),
                tostring(send_err)))
            return 0, false
        end
        local elapsed = elapsed_ms(started)
        log_perf(string.format(
            "query scoped type=%s items=%d sub=%s elapsed_ms=%d",
            tostring(entity_type),
            #items,
            tostring(sub_id or "nil"),
            elapsed))
        if elapsed > 250 then
            log.warn(string.format(
                "entity_broadcast.query_slow: type=%s items=%d sub=%s elapsed_ms=%d",
                tostring(entity_type),
                #items,
                tostring(sub_id or "nil"),
                elapsed))
        end
        log.info(string.format(
            "entity_broadcast.query: type=%s scoped_items=%d sub=%s",
            tostring(entity_type),
            #items,
            tostring(sub_id or "nil")))
        return 1
    end

    local sent = 0
    local matched_requested_id = false
    for _, item in ipairs(items) do
        local id = resolve_id(entry, item, "query")
        if id then
            if request.id and id == request.id then
                matched_requested_id = true
            end
            local frame = {
                v = 2,
                type = "entity_upsert",
                entity_type = entity_type,
                id = id,
                entity = item,
                snapshot_seq = next_seq(entity_type),
            }
            if sub_id ~= nil then frame.subscriptionId = sub_id end
            local ok_send, send_err = pcall(client.send, client, frame)
            if not ok_send then
                log.warn(string.format(
                    "entity_broadcast.query: send failed for type=%s sub=%s: %s",
                    tostring(entity_type),
                    tostring(sub_id or "nil"),
                    tostring(send_err)))
                return sent, false
            end
            sent = sent + 1
        end
    end
    if request.id and not matched_requested_id then
        local frame = {
            v = 2,
            type = "entity_remove",
            entity_type = entity_type,
            id = request.id,
            snapshot_seq = next_seq(entity_type),
        }
        if sub_id ~= nil then frame.subscriptionId = sub_id end
        local ok_send, send_err = pcall(client.send, client, frame)
        if not ok_send then
            log.warn(string.format(
                "entity_broadcast.query: send failed for type=%s sub=%s: %s",
                tostring(entity_type),
                tostring(sub_id or "nil"),
                tostring(send_err)))
            return sent, false
        end
        sent = sent + 1
    end
    local elapsed = elapsed_ms(started)
    log_perf(string.format(
        "query type=%s items=%d sub=%s elapsed_ms=%d",
        tostring(entity_type),
        #items,
        tostring(sub_id or "nil"),
        elapsed))
    if elapsed > 250 then
        log.warn(string.format(
            "entity_broadcast.query_slow: type=%s items=%d sub=%s elapsed_ms=%d",
            tostring(entity_type),
            #items,
            tostring(sub_id or "nil"),
            elapsed))
    end
    log.info(string.format(
        "entity_broadcast.query: type=%s items=%d upserts=%d sub=%s",
        tostring(entity_type),
        #items,
        sent,
        tostring(sub_id or "nil")))
    return sent
end

local function log_snapshot_batch(sent, sub_id, batch_started, suffix)
    log.info(string.format(
        "entity_broadcast.snapshot%s: sent %d type snapshot(s) to sub=%s",
        suffix or "",
        sent,
        tostring(sub_id or "nil")))
    if batch_started then
        log_perf(string.format(
            "sent=%d sub=%s elapsed_ms=%d%s",
            sent,
            tostring(sub_id or "nil"),
            elapsed_ms(batch_started),
            suffix or ""))
    end
end

function M.send_snapshots_to(client, sub_id, opts)
    assert(client and type(client.send) == "function",
        "entity_broadcast.send_snapshots_to: client must support :send(msg)")
    local batch_started = PERF and os.clock() or nil
    local context = {}
    local plugin_contexts = {}
    local sent = 0
    for _, entity_type in ipairs(registered_type_names(opts)) do
        local type_sent, ok = send_snapshot_type(client, sub_id, entity_type, context, plugin_contexts)
        sent = sent + type_sent
        if ok == false then
            log_snapshot_batch(sent, sub_id, batch_started, " canceled")
            return
        end
    end
    for _, request in ipairs(requested_entity_queries(opts)) do
        local request_sent, ok = send_query_request(client, sub_id, request, context, plugin_contexts)
        sent = sent + request_sent
        if ok == false then
            log_snapshot_batch(sent, sub_id, batch_started, " canceled")
            return
        end
    end
    log_snapshot_batch(sent, sub_id, batch_started)
end

--- Schedule `entity_snapshot` frames one entity type at a time.
---
--- Browser requests use this cooperative path so expensive plugin-owned
--- entity snapshots do not monopolize the WebRTC command handler across the
--- whole requested batch. Each type still publishes the same snapshot frame as
--- `send_snapshots_to`, but the work is moved onto timer ticks so
--- higher-priority hub events can run between requested entity types. A single
--- expensive provider can still occupy its own tick until it returns; slow
--- providers are logged so they can be fixed or split.
function M.schedule_snapshots_to(client, sub_id, opts)
    assert(client and type(client.send) == "function",
        "entity_broadcast.schedule_snapshots_to: client must support :send(msg)")

    if type(timer) ~= "table" or type(timer.after) ~= "function" then
        return M.send_snapshots_to(client, sub_id, opts)
    end

    local names = registered_type_names(opts)
    local requests = requested_entity_queries(opts)
    local batch_started = PERF and os.clock() or nil
    local context = {}
    local plugin_contexts = {}
    local index = 1
    local request_index = 1
    local sent = 0
    local job_key = snapshot_job_key(sub_id, names, requests)
    client.__entity_snapshot_jobs = client.__entity_snapshot_jobs or {}
    client.__entity_snapshot_job_seq = client.__entity_snapshot_job_seq or {}
    local job_id = (client.__entity_snapshot_job_seq[job_key] or 0) + 1
    client.__entity_snapshot_job_seq[job_key] = job_id
    client.__entity_snapshot_jobs[job_key] = job_id

    local function job_is_current()
        return client.__entity_snapshot_jobs
            and client.__entity_snapshot_jobs[job_key] == job_id
    end

    local function clear_current_job_sequence()
        if client.__entity_snapshot_job_seq and client.__entity_snapshot_job_seq[job_key] == job_id then
            client.__entity_snapshot_job_seq[job_key] = nil
        end
    end

    local function clear_current_job()
        if client.__entity_snapshot_jobs and client.__entity_snapshot_jobs[job_key] == job_id then
            client.__entity_snapshot_jobs[job_key] = nil
        end
        clear_current_job_sequence()
    end

    local function step()
        if not job_is_current() then
            if not client.__entity_snapshot_jobs or client.__entity_snapshot_jobs[job_key] == nil then
                clear_current_job_sequence()
            end
            log_snapshot_batch(sent, sub_id, batch_started, " canceled_stale")
            return
        end
        if not subscription_is_active(client, sub_id) then
            clear_current_job()
            log_snapshot_batch(sent, sub_id, batch_started, " canceled")
            return
        end

        local entity_type = names[index]
        if entity_type ~= nil then
            index = index + 1
            local type_sent, ok = send_snapshot_type(client, sub_id, entity_type, context, plugin_contexts)
            sent = sent + type_sent
            if ok == false then
                clear_current_job()
                log_snapshot_batch(sent, sub_id, batch_started, " canceled")
                return
            end
            timer.after(0, step)
            return
        end

        local request = requests[request_index]
        if request ~= nil then
            request_index = request_index + 1
            local request_sent, ok = send_query_request(client, sub_id, request, context, plugin_contexts)
            sent = sent + request_sent
            if ok == false then
                clear_current_job()
                log_snapshot_batch(sent, sub_id, batch_started, " canceled")
                return
            end
            timer.after(0, step)
            return
        end

        log_snapshot_batch(sent, sub_id, batch_started, " scheduled")
        clear_current_job()
        return
    end

    timer.after(0, step)
    return 0
end

--- Clear the active scheduled snapshot markers for a subscription.
---
--- Client unsubscribe/replace paths call this so long-lived browser peers do
--- not keep stale snapshot jobs alive after the subscription contract ends.
--- Sequence counters are left in place until their queued ticks drain so a new
--- job cannot reuse the stale job id and make an old tick look current.
function M.clear_scheduled_snapshots(client, sub_id)
    if not client then return end
    if type(client.__entity_snapshot_jobs) == "table" then
        local prefix = snapshot_job_key_prefix(sub_id)
        for key in pairs(client.__entity_snapshot_jobs) do
            if key:sub(1, #prefix) == prefix then
                client.__entity_snapshot_jobs[key] = nil
            end
        end
    end
end

-- -------------------------------------------------------------------------
-- Introspection (tests + diagnostics)
-- -------------------------------------------------------------------------

function M.is_registered(entity_type)
    return registry[entity_type] ~= nil
end

function M.snapshot_seq(entity_type)
    return current_seq(entity_type)
end

function M.registered_types()
    return registered_type_names({ include_non_default = true })
end

-- -------------------------------------------------------------------------
-- Hot-reload + test reset
-- -------------------------------------------------------------------------

function M._before_reload()
    log.info("entity_broadcast.lua reloading")
end

function M._after_reload()
    log.info("entity_broadcast.lua reloaded")
    local ok, core_entities = pcall(require, "hub.core_entities")
    if ok and type(core_entities) == "table" and type(core_entities.register) == "function" then
        local registered, err = pcall(core_entities.register)
        if not registered then
            log.warn(string.format(
                "entity_broadcast.lua failed to re-register built-in entities: %s",
                tostring(err)))
        end
    else
        log.warn(string.format(
            "entity_broadcast.lua could not load built-in entity providers: %s",
            tostring(core_entities)))
    end
    local replayed = 0
    for entity_type, entry in pairs(replay_registry) do
        if type(entry) == "table" and type(entry.owner_plugin) == "string" then
            local registered, err = pcall(M.register, entity_type, {
                id_field = entry.id_field,
                all = entry.all,
                query = entry.query,
                filter = entry.filter,
                owner_plugin = entry.owner_plugin,
                default = entry.default,
            })
            if registered then
                replayed = replayed + 1
            else
                log.warn(string.format(
                    "entity_broadcast.lua failed to replay plugin entity %s: %s",
                    tostring(entity_type),
                    tostring(err)))
            end
        end
    end
    if replayed > 0 then
        log.info(string.format(
            "entity_broadcast.lua replayed %d plugin entity provider(s)",
            replayed))
    end
    local ok_connections, connections = pcall(require, "handlers.connections")
    if ok_connections and type(connections) == "table" and type(connections.install_entity_broadcaster) == "function" then
        local installed, err = pcall(connections.install_entity_broadcaster, M)
        if not installed then
            log.warn(string.format(
                "entity_broadcast.lua failed to reinstall broadcaster: %s",
                tostring(err)))
        end
    else
        log.warn(string.format(
            "entity_broadcast.lua could not reinstall broadcaster: %s",
            tostring(connections)))
    end
end

--- Wipe registry, broadcaster, and seq counters. Test-only — production
--- hot-reload preserves the seq counters via `state` precisely so we never
--- trigger this path on a live hub.
function M._reset_for_tests()
    for k in pairs(registry) do registry[k] = nil end
    for k in pairs(replay_registry) do replay_registry[k] = nil end
    for k in pairs(seq_by_type) do seq_by_type[k] = nil end
    state.set("entity_broadcast.seq_by_type", seq_by_type)
    state.set("entity_broadcast.seq_epoch", 0)
    broadcaster = function(_frame) end
end

local ok_hooks, hooks = pcall(require, "hub.hooks")
if ok_hooks and hooks and type(hooks.on) == "function" then
    hooks.on("plugin_unloading", "entity_broadcast.unregister_plugin", function(info)
        if type(info) ~= "table" then return end
        M.unregister_plugin(info.plugin_name or info.name or info.key)
    end)
end

return M
