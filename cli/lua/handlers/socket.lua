-- Socket IPC transport handler (hot-reloadable)
--
-- Registers socket client callbacks and creates a Client with socket transport.
-- Delegates client management to handlers.connections (shared registry).
--
-- Uses the same subscription protocol as browsers and TUI:
--   subscribe/unsubscribe/data with subscriptionId and channel
--
-- Socket clients are multi-peer (like WebRTC), each with a unique client_id.
-- Multiple clients can connect simultaneously (multiple TUIs, CLI tools, plugins).

local Client = require("lib.client")
local hooks = require("hub.hooks")
local connections = require("handlers.connections")

local rpc_queues = {}
local rpc_scheduled = {}

--- Create a socket transport for a specific client.
-- @param client_id The unique identifier for the socket client
-- @return Transport table with send(), send_binary(), subscribe_terminal(), and type
local function make_socket_transport(client_id)
    return {
        type = "socket",
        send = function(msg) socket.send(client_id, msg) end,
        send_binary = function(data) socket.send_binary(client_id, data) end,
        subscribe_terminal = function(opts)
            opts.client_id = client_id
            return socket.subscribe_terminal(opts)
        end,
    }
end

local function drain_hub_rpc_requests(client_id)
    rpc_scheduled[client_id] = nil
    local queue = rpc_queues[client_id]
    if not queue then return end

    while queue.head <= queue.tail do
        local msg = queue.items[queue.head]
        queue.items[queue.head] = nil
        queue.head = queue.head + 1
        local ok, err = pcall(hooks.notify, "hub_rpc_request", client_id, msg)
        if not ok then
            log.error(string.format("hub_rpc_request %s: %s", client_id, tostring(err)))
        end
    end

    rpc_queues[client_id] = nil
end

local function notify_hub_rpc_request(client_id, msg)
    -- Hub-to-hub RPC handlers can do synchronous routing work (agent lookup,
    -- command dispatch, PTY snapshots). Keep that work out of the socket
    -- message callback so the socket ingress path can drain promptly, while
    -- preserving per-client FIFO ordering.
    if type(timer) == "table" and type(timer.after) == "function" then
        local queue = rpc_queues[client_id]
        if not queue then
            queue = { items = {}, head = 1, tail = 0 }
            rpc_queues[client_id] = queue
        end
        queue.tail = queue.tail + 1
        queue.items[queue.tail] = msg

        if not rpc_scheduled[client_id] then
            rpc_scheduled[client_id] = true
            timer.after(0, function()
                drain_hub_rpc_requests(client_id)
            end)
        end
    else
        hooks.notify("hub_rpc_request", client_id, msg)
    end
end

-- ============================================================================
-- Socket Callbacks
-- ============================================================================

-- Called when a new socket client connects
socket.on_client_connected(function(client_id)
    log.info("Socket client connected: " .. client_id)

    local client = Client.new(client_id, make_socket_transport(client_id))
    connections.register_client(client_id, client)
end)

-- Called when a socket client disconnects
socket.on_client_disconnected(function(client_id)
    log.info("Socket client disconnected: " .. client_id)
    rpc_queues[client_id] = nil
    rpc_scheduled[client_id] = nil
    connections.unregister_client(client_id)
end)

-- Called for each message from a socket client
socket.on_message(function(client_id, msg)
    -- Hub-to-hub RPC request: dispatch via hooks, skip subscription protocol
    if msg._mcp_rid then
        notify_hub_rpc_request(client_id, msg)
        return
    end

    local client = connections.get_client(client_id)

    if not client then
        -- Message before on_client_connected fired (shouldn't happen)
        log.warn("Socket message but no client registered for " .. client_id .. ", creating")
        client = Client.new(client_id, make_socket_transport(client_id))
        connections.register_client(client_id, client)
    end

    connections.track_message()

    -- Route message to client (same protocol as browser and TUI)
    local ok, err = pcall(client.on_message, client, msg)
    if not ok then
        log.error(string.format("Error handling socket message from %s: %s", client_id, tostring(err)))
        client:send({
            type = "error",
            error = "Internal error processing message",
        })
    end
end)

-- ============================================================================
-- Module Interface
-- ============================================================================

local M = {}

-- Lifecycle hooks for hot-reload
function M._before_reload()
    log.info("socket.lua reloading")
    rpc_queues = {}
    rpc_scheduled = {}
end

function M._after_reload()
    log.info("socket.lua reloaded")
end

log.info("Socket handler loaded")

return M
