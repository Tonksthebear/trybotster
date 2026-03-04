-- Per-session client dimension tracking
--
-- Tracks which clients are connected to each PTY session and their dimensions.
-- Keyed by session_uuid (single PTY per session).
-- When a client disconnects, auto-resizes to the next client's most
-- recently updated dims.
--
-- This module is hot-reloadable; state is persisted via hub.state.

local state = require("hub.state")

local M = {}

-- Persistent state: { [session_uuid] = { { peer_id, rows, cols, updated_at }, ... } }
local function get_store()
    local s = state.get("pty_clients")
    if not s then
        s = {}
        state.set("pty_clients", s)
    end
    return s
end

--- Find a client entry in the list for a given session.
-- @return entry, index or nil, nil
local function find_entry(list, peer_id)
    for i, entry in ipairs(list) do
        if entry.peer_id == peer_id then
            return entry, i
        end
    end
    return nil, nil
end

--- Find the most recently updated client for a session.
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

--- Register a client for a session with initial dimensions.
-- Called when a client subscribes to a terminal channel.
-- Resizes the PTY to the new client's dimensions.
-- @param session_uuid string Session UUID
-- @param peer_id Client peer ID
-- @param rows Number of rows
-- @param cols Number of columns
function M.register(session_uuid, peer_id, rows, cols)
    rows = rows or 24
    cols = cols or 80

    local store = get_store()

    if not store[session_uuid] then
        store[session_uuid] = {}
    end

    -- Remove existing entry for this peer (re-subscribe)
    local _, idx = find_entry(store[session_uuid], peer_id)
    if idx then
        table.remove(store[session_uuid], idx)
    end

    table.insert(store[session_uuid], {
        peer_id = peer_id,
        rows = rows,
        cols = cols,
        focused = false,
        updated_at = os.clock(),
    })

    -- Resize PTY to the new client's dimensions
    hub.resize_pty(session_uuid, rows, cols)

    log.debug(string.format("pty_clients.register: %s -> %s (%dx%d, %d clients)",
        peer_id:sub(1, 8), session_uuid:sub(1, 16), cols, rows, #store[session_uuid]))
end

--- Update a client's dimensions for a session.
-- Called when a client sends a resize through the terminal channel.
-- Resizes the PTY immediately.
-- @param session_uuid string Session UUID
-- @param peer_id Client peer ID
-- @param rows Number of rows
-- @param cols Number of columns
function M.update(session_uuid, peer_id, rows, cols)
    rows = rows or 24
    cols = cols or 80

    local store = get_store()

    if not store[session_uuid] then
        M.register(session_uuid, peer_id, rows, cols)
        return
    end

    local entry = find_entry(store[session_uuid], peer_id)
    if entry then
        local old_rows, old_cols = entry.rows, entry.cols
        entry.rows = rows
        entry.cols = cols
        entry.updated_at = os.clock()

        if old_rows == rows and old_cols == cols then
            return  -- No change
        end
    else
        M.register(session_uuid, peer_id, rows, cols)
        return
    end

    hub.resize_pty(session_uuid, rows, cols)

    log.debug(string.format("pty_clients.update: %s -> %s (%dx%d)",
        peer_id:sub(1, 8), session_uuid:sub(1, 16), cols, rows))
end

--- Unregister a client from a session.
-- Called when a client unsubscribes or disconnects.
-- If other clients remain, resizes PTY to the most recently updated client's dims.
-- @param session_uuid string Session UUID
-- @param peer_id Client peer ID
-- @return rows, cols of the new active client, or nil if no clients remain
function M.unregister(session_uuid, peer_id)
    local store = get_store()

    if not store[session_uuid] then
        return nil
    end

    local _, idx = find_entry(store[session_uuid], peer_id)
    if idx then
        table.remove(store[session_uuid], idx)
    end

    -- If no clients remain, clean up
    if #store[session_uuid] == 0 then
        store[session_uuid] = nil
        log.debug(string.format("pty_clients.unregister: %s -> %s (no clients remain)",
            peer_id:sub(1, 8), session_uuid:sub(1, 16)))
        return nil
    end

    -- Resize to the most recently updated remaining client
    local best = find_most_recent(store[session_uuid])
    if best then
        hub.resize_pty(session_uuid, best.rows, best.cols)
        log.debug(string.format("pty_clients.unregister: %s -> %s, resized to %s (%dx%d, %d remain)",
            peer_id:sub(1, 8), session_uuid:sub(1, 16),
            best.peer_id:sub(1, 8), best.cols, best.rows, #store[session_uuid]))
        return best.rows, best.cols
    end

    return nil
end

--- Get the active dimensions for a session (most recently updated client).
-- @param session_uuid string Session UUID
-- @return rows, cols or nil, nil if no clients
function M.get_active_dims(session_uuid)
    local store = get_store()

    if not store[session_uuid] or #store[session_uuid] == 0 then
        return nil, nil
    end

    local best = find_most_recent(store[session_uuid])
    if best then
        return best.rows, best.cols
    end

    return nil, nil
end

--- Set a client's focused state for a session.
-- @param session_uuid string Session UUID
-- @param peer_id Client peer ID
-- @param focused boolean
function M.set_focused(session_uuid, peer_id, focused)
    local store = get_store()
    if not store[session_uuid] then return end
    local entry = find_entry(store[session_uuid], peer_id)
    if entry then entry.focused = focused end
end

--- Check if any client is currently focused on a session.
-- Used by the push notification handler to suppress notifications
-- when at least one client is actively viewing the PTY.
-- @param session_uuid string Session UUID
-- @return boolean
function M.is_any_focused(session_uuid)
    local store = get_store()
    if not store[session_uuid] then return false end
    for _, entry in ipairs(store[session_uuid]) do
        if entry.focused then return true end
    end
    return false
end

-- Lifecycle hooks for hot-reload
function M._before_reload()
    log.info("pty_clients.lua reloading (state preserved)")
end

function M._after_reload()
    log.info("pty_clients.lua reloaded")
end

return M
