-- Web layout broadcast helper (Phase 2b + Phase 4a).
--
-- Wraps `web_layout.render(...)` with:
--   1. Registry-driven surface expansion — iterates
--      `lib.surfaces.list()` so every registered surface (workspace_sidebar,
--      workspace_panel, plugin-authored surfaces) fans out automatically.
--      Pre-Phase-4a this was a hardcoded two-entry table for the workspace
--      densities; plugins dropping `surfaces.register(...)` now slot in
--      without changes here.
--   2. Version hashing — a pure-Lua fingerprint over the resulting tree JSON
--      so downstream dedup can skip rebroadcasts when the tree is unchanged.
--   3. Dedup snapshot — `build_frames` only emits frames whose `version`
--      differs from the last-sent version for that target surface.
--
-- Known perf tradeoff (ack'd by orchestrator): every registered surface
-- is re-rendered per broadcast. Surfaces that don't depend on the session
-- list can short-circuit in their own render fn; dedup will suppress
-- unchanged bytes downstream.
--
-- The module is pure: no `client:send`. Callers (handlers/connections.lua
-- and lib/client.lua:send_hub_layout_trees) iterate the returned frames and
-- ship them over the existing encrypted transport.

local state = require("hub.state")

local M = {}

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

--- Render one frame for `surface_name` using `surface_state` as input.
--- Returns a table `{ type, target_surface, tree, version, hub_id, subpath }`
--- ready to ship via `client:send(frame)`, or nil if rendering / decoding
--- failed.
---
--- Uses `web_layout.render(surface_name, state)` uniformly so:
---   * override files (`.botster/layout_web.lua`) keep their precedence
---   * the embedded `web.layout` module still handles `workspace_surface`
---     when the workspace wrappers delegate to it
---   * plugin-registered surfaces fall through to
---     `_G.surfaces.render_node(...)` via the Rust fallback (Phase 4a).
---
--- Phase 4b: the emitted frame echoes back `subpath` — the value the
--- dispatcher routed on — so the browser can ignore frames produced for a
--- subpath it no longer cares about. Critical for preventing the cold-load
--- flash when the hub's initial default-"/" frame would otherwise paint the
--- wrong sub-page before the browser's surface.subpath action lands.
local function render_one(surface_name, surface_state)
    local ok, result_json = pcall(function()
        return web_layout.render(surface_name, surface_state)
    end)
    if not ok then
        log.warn(string.format(
            "layout_broadcast: web_layout.render failed for %s: %s",
            surface_name, tostring(result_json)))
        return nil
    end
    if type(result_json) ~= "string" then
        log.warn(string.format(
            "layout_broadcast: render returned %s for %s",
            type(result_json), surface_name))
        return nil
    end

    local tree, decode_err = json.decode(result_json)
    if type(tree) ~= "table" then
        log.warn(string.format(
            "layout_broadcast: tree decode failed for %s: %s",
            surface_name, tostring(decode_err)))
        return nil
    end

    -- Fingerprint from the JSON string directly — web_layout.render emits
    -- it via serde_json with a stable field order, so same inputs yield
    -- identical bytes. We do NOT fold the subpath into the hash: two
    -- subpaths that render to the same tree should dedup to one frame
    -- (rare in practice, but the invariant keeps us aligned with
    -- `fnv1a64_hex(tree_json)` which downstream tests assert on).
    local subpath = (type(surface_state) == "table") and surface_state.path or nil
    local version = fnv1a64_hex(result_json)

    return {
        type = "ui_layout_tree_v1",
        target_surface = surface_name,
        tree = tree,
        version = version,
        hub_id = (type(surface_state) == "table") and surface_state.hub_id or nil,
        subpath = subpath,
    }
end

-- Resolve the subpath the client is currently viewing for this surface.
-- Callers pre-build input via `entry.input_builder(...)` OR the shared
-- `LayoutInput.build_for_subscription` default; neither of those know about
-- the current URL. We layer the URL-bound state on top here so every
-- surface's render gets the right `state.path` threaded through the
-- dispatcher built by `surfaces.lua`.
--
-- Storage: `client.surface_subpaths` is a `{ [surface_name] = subpath }`
-- map owned by the hub-side client (see `cli/lua/lib/client.lua`). Unset
-- entries fall back to "/", which matches every surface's default route.
local function resolve_subpath(client, surface_name)
    if type(client) ~= "table" then return "/" end
    local paths = client.surface_subpaths
    if type(paths) ~= "table" then return "/" end
    local sub = paths[surface_name]
    if type(sub) == "string" and sub ~= "" then return sub end
    return "/"
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

--- Build the frame list for a hub-channel subscription.
---
--- Iterates every registered surface in `lib.surfaces` and emits one frame
--- per surface whose rendered tree differs from the last-sent version for
--- this `(subscription_key, target_surface)` pair.
---
--- Input resolution (per surface, in order):
---   1. If the surface declares an `input_builder(client, sub_id)`, call it.
---   2. Otherwise, if `opts.client` is set, fall back to
---      `LayoutInput.build_for_subscription(opts.client, opts.subscription_key)`
---      — the Phase 2b default for the workspace surfaces.
---   3. Otherwise (tests / diagnostics), use `base_state` as-is. This is the
---      path the old two-density test helpers exercise.
---
--- When `opts.force` is true, emit every frame regardless of dedup. Used for
--- priming new subscribers.
---
-- @param base_state table Input state (agents, open_workspaces, hub_id, etc.).
--                         May be nil when opts.client is provided — each
--                         surface builds its own input in that case.
-- @param opts table? { force = bool, subscription_key = string, client = any }
-- @return table array of frames
function M.build_frames(base_state, opts)
    if opts == true then
        -- Legacy API: `build_frames(state, true)` used by older callers /
        -- tests to force a full emission. Normalise to the modern shape.
        opts = { force = true }
    end
    opts = opts or {}
    local force = opts.force == true

    if base_state ~= nil and type(base_state) ~= "table" then
        log.warn("layout_broadcast.build_frames: non-table state")
        return {}
    end

    local key = resolve_key(opts)
    local bucket = versions_for(key)

    -- Defer the `lib.surfaces` require to call time. Loading it at module
    -- load would create a circular dependency (surfaces → state → layout,
    -- layout → surfaces) whenever hot-reload re-evaluates either module.
    local ok_surfaces, surfaces_mod = pcall(require, "lib.surfaces")
    if not ok_surfaces or type(surfaces_mod) ~= "table" then
        log.warn(string.format(
            "layout_broadcast.build_frames: surfaces module unavailable: %s",
            tostring(surfaces_mod)))
        return {}
    end

    -- Allow callers to target a single surface. Production uses this for
    -- surface.subpath-driven re-renders — re-rendering EVERY surface per URL
    -- change would be wasteful and also churn the dedup state. Tests leave
    -- `only_surface` unset and fan out to the full registry.
    local only_surface = nil
    if type(opts.only_surface) == "string" and opts.only_surface ~= "" then
        only_surface = opts.only_surface
    end

    local frames = {}
    for _, summary in ipairs(surfaces_mod.list()) do
        local surface_name = summary.name
        if only_surface == nil or only_surface == surface_name then
            local entry = surfaces_mod.get(surface_name)
            if entry then
                local surface_state
                if entry.input_builder then
                    local ok, built = pcall(entry.input_builder, opts.client, opts.subscription_key)
                    if ok then
                        surface_state = built
                    else
                        log.warn(string.format(
                            "layout_broadcast: input_builder for %s threw: %s",
                            surface_name, tostring(built)))
                    end
                elseif opts.client then
                    local LayoutInput = require("lib.layout_input")
                    surface_state = LayoutInput.build_for_subscription(opts.client, opts.subscription_key)
                else
                    surface_state = base_state
                end

                if type(surface_state) == "table" then
                    -- Thread the current subpath through the render input so
                    -- the surface dispatcher (lib.surfaces) can route it to
                    -- the correct sub-route. `client.surface_subpaths` is
                    -- updated by the `botster.surface.subpath` action, and
                    -- primed at subscribe-time so cold-load lands on the
                    -- right sub-page without flashing "/".
                    if surface_state.path == nil then
                        surface_state.path = resolve_subpath(opts.client, surface_name)
                    end
                    local frame = render_one(surface_name, surface_state)
                    if frame then
                        if force or bucket[surface_name] ~= frame.version then
                            frames[#frames + 1] = frame
                        end
                    end
                end
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

--- Drop the dedup baselines for `surface_name` across EVERY subscription.
---
--- Called when a surface is unregistered so its per-sub version entries
--- don't accumulate forever. Also guards against the "re-register same
--- name" footgun: without this, a freshly-registered surface with the
--- same name would inherit the old surface's cached version hash for
--- every subscription that ever saw the old tree, and — if the new
--- tree's hash happened to collide — dedup would silently swallow the
--- first emission.
-- @param surface_name string
-- @return number count of subscription buckets that had an entry removed
function M.forget_surface(surface_name)
    if type(surface_name) ~= "string" or surface_name == "" then return 0 end
    local removed = 0
    for _, bucket in pairs(versions_by_key) do
        if bucket[surface_name] ~= nil then
            bucket[surface_name] = nil
            removed = removed + 1
        end
    end
    if removed > 0 then
        state.set("layout_broadcast.versions_by_key", versions_by_key)
    end
    return removed
end

--- Drop the entire dedup cache so the next `build_frames` emits both
--- densities for every subscription. Used when the input shape changes in a
--- way the hash cannot detect (e.g. a surface added) or by tests.
function M.invalidate()
    for k in pairs(versions_by_key) do versions_by_key[k] = nil end
    state.set("layout_broadcast.versions_by_key", versions_by_key)
end

--- Expose the set of currently registered target surfaces (tests).
--- Returns a deterministic array `{ target_surface = ... }` mirroring the
--- ordering produced by `lib.surfaces.list()`.
-- @return table
function M.surface_targets()
    local ok, surfaces_mod = pcall(require, "lib.surfaces")
    if not ok or type(surfaces_mod) ~= "table" then return {} end
    local out = {}
    for _, entry in ipairs(surfaces_mod.list()) do
        out[#out + 1] = { target_surface = entry.name }
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
