-- Entity broadcast registry — wire protocol (delta) source of truth.
--
-- Replaces the current "rebuild + broadcast every UiNode tree on any state
-- change" pattern with "snapshot once on subscribe, ship per-entity field
-- deltas thereafter." Each entity type (`session`, `workspace`,
-- `spawn_target`, `worktree`, `hub`, `connection_code`, plus plugin types
-- namespaced as `<plugin>.<type>`) registers its `id_field` and an `all()`
-- snapshot source. Mutators (`upsert`/`patch`/`remove`) construct one of the
-- four wire envelopes and hand them to the connection-layer broadcaster.
--
-- Wire envelopes (all carry `v = 2`):
--   { type = "entity_snapshot", entity_type, items, snapshot_seq }
--   { type = "entity_upsert",   entity_type, id, entity, snapshot_seq }
--   { type = "entity_patch",    entity_type, id, patch, snapshot_seq }
--   { type = "entity_remove",   entity_type, id, snapshot_seq }
--
-- `snapshot_seq` is monotonic per entity type per hub process, seeded from a
-- wall-clock boot epoch so a reboot does not restart at 0 and trip older
-- reconnecting clients that still gate snapshots by sequence. Clients keep
-- their own `last_snapshot_seq` per type and drop out-of-order deltas. On
-- subscribe, the hub re-ships an `entity_snapshot` for every registered
-- type (see `send_snapshots_to`), which resets the client's baseline.
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
--   * `broadcaster` is similarly transient — `connections.lua` re-installs
--     it from its `_after_reload` so the function reference always points
--     at the live transport layer.

local state = require("hub.state")

local M = {}

-- entity_type -> { id_field = string, all = function, filter = function? }
local registry = {}

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
--- subscribe snapshots greater than any ordinary pre-reboot delta sequence,
--- which protects clients that have not yet learned that snapshots are
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
---   all = function -> array,  -- snapshot source called on subscribe
---   filter = function? -> bool, -- optional per-item gate (true = include)
--- }
---
--- Re-registration overwrites the prior entry; a warning is logged so the
--- "two providers fighting over one type" footgun is at least visible.
function M.register(entity_type, opts)
    assert(type(entity_type) == "string" and entity_type ~= "",
        "entity_broadcast.register: entity_type must be a non-empty string")
    assert(type(opts) == "table", "entity_broadcast.register: opts table required")
    assert(type(opts.id_field) == "string" and opts.id_field ~= "",
        "entity_broadcast.register: opts.id_field must be a non-empty string")
    assert(type(opts.all) == "function",
        "entity_broadcast.register: opts.all must be a function")
    if opts.filter ~= nil and type(opts.filter) ~= "function" then
        error("entity_broadcast.register: opts.filter must be a function or nil")
    end

    if registry[entity_type] then
        log.warn(string.format(
            "entity_broadcast: re-registering type %q", entity_type))
    end
    registry[entity_type] = {
        id_field = opts.id_field,
        all = opts.all,
        filter = opts.filter,
    }
end

--- Drop a registration. Used by plugin teardown and by tests.
function M.unregister(entity_type)
    registry[entity_type] = nil
end

-- -------------------------------------------------------------------------
-- Public API: mutators
-- -------------------------------------------------------------------------

--- Emit `entity_upsert`. Called when a new entity arrives or when the entity
--- record is being replaced wholesale (e.g. agent_created hook handler).
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
--- objects (e.g. `hosted_preview = { ... }`) replace the prior value
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
    if type(fields) ~= "table" or next(fields) == nil then return end
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

-- -------------------------------------------------------------------------
-- Subscribe-time priming
-- -------------------------------------------------------------------------

local function registered_type_names()
    local names = {}
    for name in pairs(registry) do names[#names + 1] = name end
    -- Stable order so test assertions and on-the-wire logs are reproducible.
    table.sort(names)
    return names
end

local function snapshot_items(entry, entity_type)
    local ok, items = pcall(entry.all)
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
    if not entry.filter then return items end
    local kept = {}
    for _, item in ipairs(items) do
        local ok_f, keep = pcall(entry.filter, item)
        if ok_f and keep then kept[#kept + 1] = item end
    end
    return kept
end

--- Send one `entity_snapshot` per registered type to a single subscriber.
--- Called from `Client:handle_subscribe` on the hub channel BEFORE the
--- structural `ui_tree_snapshot` frames so trees may safely reference
--- entities (the client store will already be populated).
function M.send_snapshots_to(client, sub_id)
    assert(client and type(client.send) == "function",
        "entity_broadcast.send_snapshots_to: client must support :send(msg)")
    local sent = 0
    for _, entity_type in ipairs(registered_type_names()) do
        local entry = registry[entity_type]
        local items = snapshot_items(entry, entity_type)
        local frame = {
            v = 2,
            type = "entity_snapshot",
            entity_type = entity_type,
            items = items,
            snapshot_seq = current_seq(entity_type),
        }
        if sub_id ~= nil then frame.subscriptionId = sub_id end
        client:send(frame)
        sent = sent + 1
        log.info(string.format(
            "entity_broadcast.snapshot: type=%s items=%d seq=%s sub=%s",
            tostring(entity_type),
            #items,
            tostring(frame.snapshot_seq),
            tostring(sub_id or "nil")))
    end
    log.info(string.format(
        "entity_broadcast.snapshot: sent %d type snapshot(s) to sub=%s",
        sent,
        tostring(sub_id or "nil")))
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
    return registered_type_names()
end

-- -------------------------------------------------------------------------
-- Hot-reload + test reset
-- -------------------------------------------------------------------------

function M._before_reload()
    log.info("entity_broadcast.lua reloading")
end

function M._after_reload()
    log.info("entity_broadcast.lua reloaded")
end

--- Wipe registry, broadcaster, and seq counters. Test-only — production
--- hot-reload preserves the seq counters via `state` precisely so we never
--- trigger this path on a live hub.
function M._reset_for_tests()
    for k in pairs(registry) do registry[k] = nil end
    for k in pairs(seq_by_type) do seq_by_type[k] = nil end
    state.set("entity_broadcast.seq_by_type", seq_by_type)
    state.set("entity_broadcast.seq_epoch", 0)
    broadcaster = function(_frame) end
end

return M
