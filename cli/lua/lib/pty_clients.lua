-- Per-PTY client dimension tracking
--
-- Tracks which clients are connected to each PTY and their dimensions.
-- PTY is the single source of truth for per-client dimensions.
-- When a client disconnects, auto-resizes to the next client's most
-- recently updated dims.
--
-- This module is hot-reloadable; state is persisted via hub.state.

local state = require("hub.state")

local M = {}

-- Persistent state: { ["agent_idx:pty_idx"] = { { peer_id, rows, cols, updated_at }, ... } }
local function get_store()
    local s = state.get("pty_clients")
    if not s then
        s = {}
        state.set("pty_clients", s)
    end
    return s
end

--- Make a key from agent_index and pty_index.
local function make_key(agent_idx, pty_idx)
    return tostring(agent_idx) .. ":" .. tostring(pty_idx)
end

--- Find a client entry in the list for a given PTY.
-- @return entry, index or nil, nil
local function find_entry(list, peer_id)
    for i, entry in ipairs(list) do
        if entry.peer_id == peer_id then
            return entry, i
        end
    end
    return nil, nil
end

--- Find the most recently updated client for a PTY.
-- @return entry or nil
local function find_most_recent(list)
    local best = nil
    for _, entry in ipairs(list) do
        if not best or entry.updated_at > best.updated_at then
            best = entry
        end
    end
    return best
end

--- Register a client for a PTY with initial dimensions.
-- Called when a client subscribes to a terminal channel.
-- Resizes the PTY to the new client's dimensions.
-- @param agent_idx Agent index
-- @param pty_idx PTY index
-- @param peer_id Client peer ID
-- @param rows Number of rows
-- @param cols Number of columns
function M.register(agent_idx, pty_idx, peer_id, rows, cols)
    rows = rows or 24
    cols = cols or 80

    local store = get_store()
    local key = make_key(agent_idx, pty_idx)

    if not store[key] then
        store[key] = {}
    end

    -- Remove existing entry for this peer (re-subscribe)
    local _, idx = find_entry(store[key], peer_id)
    if idx then
        table.remove(store[key], idx)
    end

    table.insert(store[key], {
        peer_id = peer_id,
        rows = rows,
        cols = cols,
        updated_at = os.clock(),
    })

    -- Resize PTY to the new client's dimensions
    hub.resize_pty(agent_idx, pty_idx, rows, cols)

    log.debug(string.format("pty_clients.register: %s -> %s (%dx%d, %d clients)",
        peer_id:sub(1, 8), key, cols, rows, #store[key]))
end

--- Update a client's dimensions for a PTY.
-- Called when a client sends a resize through the terminal channel.
-- Resizes the PTY immediately.
-- @param agent_idx Agent index
-- @param pty_idx PTY index
-- @param peer_id Client peer ID
-- @param rows Number of rows
-- @param cols Number of columns
function M.update(agent_idx, pty_idx, peer_id, rows, cols)
    rows = rows or 24
    cols = cols or 80

    local store = get_store()
    local key = make_key(agent_idx, pty_idx)

    if not store[key] then
        -- Client not registered — register on the fly
        M.register(agent_idx, pty_idx, peer_id, rows, cols)
        return
    end

    local entry = find_entry(store[key], peer_id)
    if entry then
        local old_rows, old_cols = entry.rows, entry.cols
        entry.rows = rows
        entry.cols = cols
        entry.updated_at = os.clock()

        if old_rows == rows and old_cols == cols then
            return  -- No change
        end
    else
        -- Not found — register on the fly
        M.register(agent_idx, pty_idx, peer_id, rows, cols)
        return
    end

    hub.resize_pty(agent_idx, pty_idx, rows, cols)

    log.debug(string.format("pty_clients.update: %s -> %s (%dx%d)",
        peer_id:sub(1, 8), key, cols, rows))
end

--- Unregister a client from a PTY.
-- Called when a client unsubscribes or disconnects.
-- If other clients remain, resizes PTY to the most recently updated client's dims.
-- @param agent_idx Agent index
-- @param pty_idx PTY index
-- @param peer_id Client peer ID
-- @return rows, cols of the new active client, or nil if no clients remain
function M.unregister(agent_idx, pty_idx, peer_id)
    local store = get_store()
    local key = make_key(agent_idx, pty_idx)

    if not store[key] then
        return nil
    end

    local _, idx = find_entry(store[key], peer_id)
    if idx then
        table.remove(store[key], idx)
    end

    -- If no clients remain, clean up
    if #store[key] == 0 then
        store[key] = nil
        log.debug(string.format("pty_clients.unregister: %s -> %s (no clients remain)",
            peer_id:sub(1, 8), key))
        return nil
    end

    -- Resize to the most recently updated remaining client
    local best = find_most_recent(store[key])
    if best then
        hub.resize_pty(agent_idx, pty_idx, best.rows, best.cols)
        log.debug(string.format("pty_clients.unregister: %s -> %s, resized to %s (%dx%d, %d remain)",
            peer_id:sub(1, 8), key, best.peer_id:sub(1, 8), best.cols, best.rows, #store[key]))
        return best.rows, best.cols
    end

    return nil
end

--- Get the active dimensions for a PTY (most recently updated client).
-- @param agent_idx Agent index
-- @param pty_idx PTY index
-- @return rows, cols or nil, nil if no clients
function M.get_active_dims(agent_idx, pty_idx)
    local store = get_store()
    local key = make_key(agent_idx, pty_idx)

    if not store[key] or #store[key] == 0 then
        return nil, nil
    end

    local best = find_most_recent(store[key])
    if best then
        return best.rows, best.cols
    end

    return nil, nil
end

-- Lifecycle hooks for hot-reload
function M._before_reload()
    log.info("pty_clients.lua reloading (state preserved)")
end

function M._after_reload()
    log.info("pty_clients.lua reloaded")
end

return M
