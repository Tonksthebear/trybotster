-- WebRTC transport handler (hot-reloadable)
--
-- Registers WebRTC peer callbacks and creates clients with WebRTC transport.
-- Delegates client management to handlers.connections (shared registry).
--
-- This module only contains WebRTC-specific logic:
-- - Transport factory (webrtc.send / webrtc.send_binary)
-- - Peer connect/disconnect/message callbacks

local Client = require("lib.client")
local connections = require("handlers.connections")

--- Create a WebRTC transport for a given peer.
-- @param peer_id The peer identifier for routing messages
-- @return Transport table with send(), send_binary(), create_pty_forwarder(), and type
local function make_webrtc_transport(peer_id)
    return {
        type = "webrtc",
        send = function(msg) webrtc.send(peer_id, msg) end,
        send_binary = function(data) webrtc.send_binary(peer_id, data) end,
        create_pty_forwarder = function(opts)
            opts.peer_id = peer_id
            return webrtc.create_pty_forwarder(opts)
        end,
    }
end

-- ============================================================================
-- WebRTC Peer Callbacks
-- ============================================================================

-- Called when WebRTC peer connects (ICE complete, DataChannel ready)
webrtc.on_peer_connected(function(peer_id)
    log.info(string.format("WebRTC peer connected: %s...", peer_id:sub(1, 8)))

    local client = Client.new(peer_id, make_webrtc_transport(peer_id))
    connections.register_client(peer_id, client)

    -- The one-time key in the DeviceKeyBundle was consumed during the Olm
    -- handshake.  Regenerate immediately so the next QR / share-link is fresh.
    -- Fires connection_code_ready → broadcasts to TUI + browser.
    connection.regenerate()
end)

-- Called when WebRTC peer disconnects
webrtc.on_peer_disconnected(function(peer_id)
    log.info(string.format("WebRTC peer disconnected: %s...", peer_id:sub(1, 8)))
    connections.unregister_client(peer_id)
end)

-- Called for each decrypted WebRTC message
webrtc.on_message(function(peer_id, msg)
    local client = connections.get_client(peer_id)

    if not client then
        -- Client not found. Can happen if:
        -- 1. Peer connected before Lua callbacks were registered (startup race)
        -- 2. Browser refresh where disconnect/reconnect happens quickly
        log.warn(string.format("Message from unknown WebRTC peer %s..., creating client",
            peer_id:sub(1, 8)))
        client = Client.new(peer_id, make_webrtc_transport(peer_id))
        connections.register_client(peer_id, client)
    end

    connections.track_message()

    -- Route message to client (with error handling)
    local ok, err = pcall(client.on_message, client, msg)
    if not ok then
        log.error(string.format("Error handling message from %s...: %s",
            peer_id:sub(1, 8), tostring(err)))
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
    log.info("webrtc.lua reloading")
end

function M._after_reload()
    log.info("webrtc.lua reloaded")
end

log.info("WebRTC handler loaded")

return M
