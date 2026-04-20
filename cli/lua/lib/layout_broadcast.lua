-- Web layout broadcast helper (Phase 2b).
--
-- Wraps `web_layout.render(...)` (Phase 2a) with:
--   1. Two-density expansion — the web has a sidebar and a main panel
--      consuming the same state; this module renders both and returns one
--      frame per target surface.
--   2. Version hashing — a pure-Lua fingerprint over the resulting tree JSON
--      so downstream dedup can skip rebroadcasts when the tree is unchanged.
--   3. Dedup snapshot — `build_frames` only emits frames whose `version`
--      differs from the last-sent version for that target surface.
--
-- Known perf tradeoff (ack'd by orchestrator): two densities broadcast per
-- update. If hub CPU becomes hot with large session lists, we can add
-- per-subscription density selection. For now the hub is viewport-agnostic
-- and the browser picks the matching tree.
--
-- The module is pure: no `client:send`. Callers (handlers/connections.lua
-- and lib/client.lua:send_hub_layout_trees) iterate the returned frames and
-- ship them over the existing encrypted transport.

local state = require("hub.state")

local M = {}

-- Maps the logical surface name on the wire -> the state.surface density
-- hint consumed by `web/layout.lua:workspace_surface`. Keeping the map here
-- (rather than in connections.lua) lets tests reason about exactly which
-- frames will fire for a given state.
local SURFACE_TARGETS = {
    { target_surface = "workspace_sidebar", density = "sidebar" },
    { target_surface = "workspace_panel",   density = "panel" },
}

-- Surface name to feed `web_layout.render(...)` — currently a single shared
-- function that switches on `state.surface` for density variation.
local LAYOUT_SURFACE_NAME = "workspace_surface"

-- Last-sent version per `{subscription_key, target_surface}`. Selection is
-- per-browser (a click on client A must not flip the selected row on
-- client B) so the rendered tree differs across subscriptions — which
-- means dedup state has to be per-subscription too.
--
-- Callers pass `opts.subscription_key` (typically the subscription id).
-- `opts.subscription_key == nil` falls back to a single bucket, used only
-- by tests that don't care about per-subscription fanout.
--
-- Shape: { [subscription_key] = { [target_surface] = version } }
local GLOBAL_KEY = "__global__"
local versions_by_key = state.get("layout_broadcast.versions_by_key", {})

-- -------------------------------------------------------------------------
-- FNV-1a 64-bit (pure Lua) — small enough to fit here and fast enough that
-- per-render hashing is well under 1ms on realistic agent lists. We only
-- need collision resistance at the "same logical tree -> same version"
-- level; cryptographic strength is not required.
-- -------------------------------------------------------------------------

local FNV_OFFSET_HI = 0xcbf29ce4
local FNV_OFFSET_LO = 0x84222325
local FNV_PRIME_HI  = 0x00000100
local FNV_PRIME_LO  = 0x000001b3

-- Lua 5.4 has native bitwise operators; older Luas expose `bit`/`bit32`. We
-- prefer native `~`/`&`/`|` when available so the hash stays a few cycles
-- per byte, and fall back to `bit32` for older runtimes (the test harness
-- occasionally lands on stripped-down VMs that lack the library).
local FNV_MASK32 = 0xffffffff
local has_native_bitops = (load("return 1 & 1")) ~= nil
local bxor32, band32
if has_native_bitops then
    bxor32 = assert(load("return function(a, b) return (a ~ b) & 0xffffffff end"))()
    band32 = assert(load("return function(a, b) return (a & b) & 0xffffffff end"))()
else
    local fallback = rawget(_G, "bit") or rawget(_G, "bit32")
    assert(fallback, "layout_broadcast: no native bitwise ops and no bit/bit32 library")
    bxor32 = fallback.bxor
    band32 = fallback.band
end

--- Compute an FNV-1a 64-bit hash over a string. Returns a 16-char lowercase
--- hex string. Tolerates nil by returning the hash of the empty string so
--- callers don't need to guard.
local function fnv1a64_hex(s)
    s = s or ""
    local hi = FNV_OFFSET_HI
    local lo = FNV_OFFSET_LO
    for i = 1, #s do
        lo = bxor32(lo, s:byte(i))
        -- 64-bit multiply by FNV prime, split across two 32-bit halves.
        -- lo' = low32(lo * PRIME_LO)
        -- hi' = low32(hi * PRIME_LO + lo * PRIME_HI + carry(lo * PRIME_LO))
        local ll = lo * FNV_PRIME_LO
        local lh = lo * FNV_PRIME_HI
        local hl = hi * FNV_PRIME_LO
        local new_lo = band32(ll, FNV_MASK32)
        local carry = math.floor(ll / 0x100000000)
        local new_hi = band32(hl + lh + carry, FNV_MASK32)
        hi, lo = new_hi, new_lo
    end
    return string.format("%08x%08x", hi, lo)
end

-- -------------------------------------------------------------------------
-- Frame construction
-- -------------------------------------------------------------------------

--- Build a shallow copy of `state` with `surface` set to the requested
--- density. The Phase-2a layout also looks at other fields
--- (agents/open_workspaces/selected_session_uuid/hub_id) unchanged.
local function state_with_density(base_state, density)
    local out = {}
    for k, v in pairs(base_state) do out[k] = v end
    out.surface = density
    return out
end

--- Render one frame for one target surface. Returns a table
--- `{ type, target_surface, tree, version, hub_id }` ready to ship via
--- `client:send(frame)`.
local function render_one(base_state, entry)
    local density_state = state_with_density(base_state, entry.density)
    local tree_json, err = pcall(function()
        return web_layout.render(LAYOUT_SURFACE_NAME, density_state)
    end)
    -- pcall with a closure returns (ok, result); remap.
    local ok = tree_json
    local result_json = err
    if not ok then
        log.warn(string.format(
            "layout_broadcast: web_layout.render failed for %s: %s",
            entry.target_surface, tostring(result_json)))
        return nil
    end
    if type(result_json) ~= "string" then
        log.warn(string.format(
            "layout_broadcast: render returned %s for %s",
            type(result_json), entry.target_surface))
        return nil
    end

    local tree, decode_err = json.decode(result_json)
    if type(tree) ~= "table" then
        log.warn(string.format(
            "layout_broadcast: tree decode failed for %s: %s",
            entry.target_surface, tostring(decode_err)))
        return nil
    end

    -- Fingerprint from the JSON string directly — web_layout.render emits
    -- it via serde_json with a stable field order, so same inputs yield
    -- identical bytes.
    local version = fnv1a64_hex(result_json)

    return {
        type = "ui_layout_tree_v1",
        target_surface = entry.target_surface,
        tree = tree,
        version = version,
        hub_id = base_state.hub_id,
    }
end

--- Normalise a subscription key. Missing/nil keys fall back to the global
--- bucket; every caller in production should supply the actual sub id.
local function resolve_key(opts)
    if type(opts) == "table" and type(opts.subscription_key) == "string"
        and opts.subscription_key ~= "" then
        return opts.subscription_key
    end
    return GLOBAL_KEY
end

local function versions_for(key)
    local bucket = versions_by_key[key]
    if not bucket then
        bucket = {}
        versions_by_key[key] = bucket
    end
    return bucket
end

--- Build the frame list for a given `AgentWorkspaceSurfaceInputV1`-shaped
--- state. Returns an array of frames that differ from the last-sent version
--- for the given subscription + target surface.
---
--- When `opts.force` is true, always emit every frame regardless of dedup.
--- Used for priming new subscribers.
-- @param base_state table Input state (agents, open_workspaces, hub_id, selected_session_uuid, etc.)
-- @param opts table? { force = bool, subscription_key = string }
-- @return table array of frames
function M.build_frames(base_state, opts)
    opts = opts or {}
    local force = opts == true or opts.force == true

    if type(base_state) ~= "table" then
        log.warn("layout_broadcast.build_frames: non-table state")
        return {}
    end

    local key = resolve_key(opts)
    local bucket = versions_for(key)

    local frames = {}
    for _, entry in ipairs(SURFACE_TARGETS) do
        local frame = render_one(base_state, entry)
        if frame then
            if force or bucket[entry.target_surface] ~= frame.version then
                frames[#frames + 1] = frame
            end
        end
    end

    return frames
end

--- Commit the version numbers emitted by `build_frames` as the new
--- last-sent baseline for the given subscription. Call this after a
--- successful broadcast so subsequent renders skip unchanged trees.
-- @param frames table array returned from build_frames
-- @param opts table? { subscription_key = string }
function M.mark_sent(frames, opts)
    local key = resolve_key(opts)
    local bucket = versions_for(key)
    for _, frame in ipairs(frames) do
        bucket[frame.target_surface] = frame.version
    end
    state.set("layout_broadcast.versions_by_key", versions_by_key)
end

--- Read the current last-sent version for a `{subscription_key,
--- target_surface}` pair (introspection for tests).
-- @param target_surface string
-- @param opts table? { subscription_key = string }
-- @return string|nil hex version
function M.last_version(target_surface, opts)
    local key = resolve_key(opts)
    local bucket = versions_by_key[key]
    return bucket and bucket[target_surface] or nil
end

--- Forget a specific subscription's dedup state. Call from Client:disconnect
--- (or the unsubscribe path) so a reconnecting browser with a new sub id
--- starts from a primed baseline instead of inheriting a stale one.
-- @param subscription_key string
function M.forget(subscription_key)
    if type(subscription_key) ~= "string" then return end
    if versions_by_key[subscription_key] == nil then return end
    versions_by_key[subscription_key] = nil
    state.set("layout_broadcast.versions_by_key", versions_by_key)
end

--- Drop the entire dedup cache so the next `build_frames` emits both
--- densities for every subscription. Used when the input shape changes in a
--- way the hash cannot detect (e.g. a surface added) or by tests.
function M.invalidate()
    for k in pairs(versions_by_key) do versions_by_key[k] = nil end
    state.set("layout_broadcast.versions_by_key", versions_by_key)
end

--- Expose the configured target set for introspection (tests).
-- @return table copy of SURFACE_TARGETS
function M.surface_targets()
    local out = {}
    for _, e in ipairs(SURFACE_TARGETS) do
        out[#out + 1] = { target_surface = e.target_surface, density = e.density }
    end
    return out
end

-- Hot-reload lifecycle
function M._before_reload()
    log.info("layout_broadcast.lua reloading")
end

function M._after_reload()
    log.info("layout_broadcast.lua reloaded")
end

-- Test-only — allow tests to reset all dedup state. Production never needs
-- this: callers should use `forget(subscription_key)` for targeted cleanup.
function M._reset_for_tests()
    for k in pairs(versions_by_key) do versions_by_key[k] = nil end
end

-- Test-only export of the internal key constant so tests can introspect
-- the "no subscription_key" bucket without duplicating the sentinel.
M._GLOBAL_KEY = GLOBAL_KEY

M._fnv1a64_hex = fnv1a64_hex

return M
