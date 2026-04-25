-- Wire protocol — structural tree-snapshot broadcaster.
--
-- Replaces the v1 `layout_broadcast.lua`. Two changes from v1:
--
--   1. Dedup is now keyed on `(surface, subpath)` GLOBALLY, not per
--      subscription. Selection moved to the client (`ui-presentation-store`
--      on web, `widget_state.rs` on the TUI), so the same tree ships to
--      every subscriber. One bucket of versions for the whole hub process.
--
--   2. The wire envelope name is `ui_tree_snapshot` (was
--      `ui_layout_tree_v1`). Cold-turkey rename — no fallback to the old
--      name. Both clients accept only the new name as of commit 7.
--
-- Builds frames via `web_layout.render(surface, state)`. Surfaces declare
-- their own `input_builder` if they need state; otherwise they receive a
-- skeletal default.

local state = require("hub.state")

local M = {}

-- (surface_name, subpath) -> version. Single global table; reload-safe via
-- hub.state.
local versions = state.get("tree_snapshot.versions", {})

-- -------------------------------------------------------------------------
-- FNV-1a 64-bit (pure Lua) — reused from layout_broadcast for stability so
-- the same tree JSON produces the same version hash across the rename. This
-- preserves dedup across hot-reload at the boundary.
-- -------------------------------------------------------------------------

local FNV_OFFSET_HI = 0xcbf29ce4
local FNV_OFFSET_LO = 0x84222325
local FNV_PRIME_HI  = 0x00000100
local FNV_PRIME_LO  = 0x000001b3
local FNV_MASK32    = 0xffffffff

local has_native_bitops = (load("return 1 & 1")) ~= nil
local bxor32, band32
if has_native_bitops then
    bxor32 = assert(load("return function(a, b) return (a ~ b) & 0xffffffff end"))()
    band32 = assert(load("return function(a, b) return (a & b) & 0xffffffff end"))()
else
    local fallback = rawget(_G, "bit") or rawget(_G, "bit32")
    assert(fallback, "tree_snapshot: no native bitwise ops and no bit/bit32 library")
    local bxor, band = fallback.bxor, fallback.band
    bxor32 = function(a, b) return bxor(a, b) % 0x100000000 end
    band32 = function(a, b) return band(a, b) % 0x100000000 end
end

local function fnv1a64_hex(s)
    s = s or ""
    local hi = FNV_OFFSET_HI
    local lo = FNV_OFFSET_LO
    for i = 1, #s do
        lo = bxor32(lo, s:byte(i))
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

local function bucket_key(surface_name, subpath)
    return tostring(surface_name) .. "\0" .. tostring(subpath or "/")
end

local function render_one(surface_name, surface_state)
    local ok, result_json = pcall(function()
        return web_layout.render(surface_name, surface_state)
    end)
    if not ok then
        log.warn(string.format(
            "tree_snapshot: web_layout.render failed for %s: %s",
            surface_name, tostring(result_json)))
        return nil
    end
    if type(result_json) ~= "string" then
        log.warn(string.format(
            "tree_snapshot: render returned %s for %s",
            type(result_json), surface_name))
        return nil
    end

    local tree, decode_err = json.decode(result_json)
    if type(tree) ~= "table" then
        log.warn(string.format(
            "tree_snapshot: tree decode failed for %s: %s",
            surface_name, tostring(decode_err)))
        return nil
    end

    local subpath = (type(surface_state) == "table") and surface_state.path or nil
    local tree_version = fnv1a64_hex(result_json)

    return {
        v = 2,
        type = "ui_tree_snapshot",
        target_surface = surface_name,
        tree = tree,
        tree_version = tree_version,
        hub_id = (type(surface_state) == "table") and surface_state.hub_id or nil,
        subpath = subpath,
    }
end

local function resolve_subpath(client, surface_name)
    if type(client) ~= "table" then return "/" end
    local paths = client.surface_subpaths
    if type(paths) ~= "table" then return "/" end
    local sub = paths[surface_name]
    if type(sub) == "string" and sub ~= "" then return sub end
    return "/"
end

--- Build the frame list for a hub-channel subscription.
---
--- Iterates every registered surface and emits one frame per surface whose
--- rendered tree differs from the last-sent version for that
--- `(surface, subpath)` GLOBAL pair. Selection is no longer threaded
--- through the input — the same tree ships to every subscriber and dedup is
--- global.
---
--- @param opts table? { force = bool, only_surface = string, client = any }
--- @return table array of frames
function M.build_frames(opts)
    if opts == true then
        opts = { force = true }
    end
    opts = opts or {}
    local force = opts.force == true

    local ok_surfaces, surfaces_mod = pcall(require, "lib.surfaces")
    if not ok_surfaces or type(surfaces_mod) ~= "table" then
        log.warn(string.format(
            "tree_snapshot.build_frames: surfaces module unavailable: %s",
            tostring(surfaces_mod)))
        return {}
    end

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
                local subpath = resolve_subpath(opts.client, surface_name)
                local route_ctx
                if type(surfaces_mod.resolve_route) == "function" then
                    local _compiled, params = surfaces_mod.resolve_route(surface_name, subpath)
                    route_ctx = { path = subpath, params = params or {} }
                end

                -- Build surface state. Wire protocol dropped
                -- `LayoutInput.build_for_subscription` (selection moved to
                -- the client). Surfaces with their own input_builder still
                -- get to compose their state; surfaces without one receive
                -- a minimal hub-id-only base.
                local surface_state
                if entry.input_builder then
                    local ok, built = pcall(
                        entry.input_builder,
                        opts.client,
                        opts.subscription_key,
                        route_ctx
                    )
                    if ok then
                        surface_state = built
                    else
                        log.warn(string.format(
                            "tree_snapshot: input_builder for %s threw: %s",
                            surface_name, tostring(built)))
                    end
                else
                    surface_state = {
                        hub_id = hub.server_id and hub.server_id() or nil,
                    }
                end

                if type(surface_state) == "table" then
                    if surface_state.path == nil then
                        surface_state.path = subpath
                    end
                    local frame = render_one(surface_name, surface_state)
                    if frame then
                        local key = bucket_key(surface_name, frame.subpath)
                        if force or versions[key] ~= frame.tree_version then
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
--- last-sent baseline. Call after a successful broadcast so subsequent
--- renders skip unchanged trees. Global, not per-subscription.
function M.mark_sent(frames)
    for _, frame in ipairs(frames) do
        local key = bucket_key(frame.target_surface, frame.subpath)
        versions[key] = frame.tree_version
    end
    state.set("tree_snapshot.versions", versions)
end

--- Read the current last-sent version for a `(surface, subpath)` pair.
function M.last_version(surface_name, subpath)
    return versions[bucket_key(surface_name, subpath)]
end

--- Drop dedup baselines for `surface_name` across all subpaths. Called when
--- a surface is unregistered so its entries don't accumulate forever.
function M.forget_surface(surface_name)
    if type(surface_name) ~= "string" or surface_name == "" then return 0 end
    local prefix = surface_name .. "\0"
    local removed = 0
    for k in pairs(versions) do
        if k:sub(1, #prefix) == prefix then
            versions[k] = nil
            removed = removed + 1
        end
    end
    if removed > 0 then
        state.set("tree_snapshot.versions", versions)
    end
    return removed
end

--- Drop the entire dedup cache so the next `build_frames` re-emits every
--- surface. Used by tests.
function M.invalidate()
    for k in pairs(versions) do versions[k] = nil end
    state.set("tree_snapshot.versions", versions)
end

--- Expose the registered target surfaces (tests).
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
    log.info("tree_snapshot.lua reloading")
end

function M._after_reload()
    log.info("tree_snapshot.lua reloaded")
end

-- Test-only — reset all dedup state.
function M._reset_for_tests()
    for k in pairs(versions) do versions[k] = nil end
end

M._fnv1a64_hex = fnv1a64_hex
M._bucket_key = bucket_key

return M
