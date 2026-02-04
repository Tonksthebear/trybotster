-- TUI transport handler (hot-reloadable)
--
-- Registers TUI callbacks and creates a Client with TUI transport.
-- Delegates client management to handlers.connections (shared registry).
--
-- Uses the same subscription protocol as browsers:
--   subscribe/unsubscribe/data with subscriptionId and channel
--
-- The TUI is a single client (no peer_id), using a fixed ID "tui".

local Client = require("lib.client")
local connections = require("handlers.connections")

--- Fixed peer ID for the single TUI client.
-- Used as the key in the shared connections registry.
local TUI_PEER_ID = "tui"

--- Create a TUI transport.
-- Unlike WebRTC, no peer_id needed — there's only one TUI.
-- @return Transport table with send(), send_binary(), and create_pty_forwarder() methods
local function make_tui_transport()
    return {
        send = function(msg) tui.send(msg) end,
        send_binary = function(data) tui.send_binary(data) end,
        create_pty_forwarder = function(opts)
            return tui.create_pty_forwarder(opts)
        end,
    }
end

-- ============================================================================
-- TUI Callbacks
-- ============================================================================

-- Called when TUI is ready to receive messages
tui.on_connected(function()
    log.info("TUI connected")

    local client = Client.new(TUI_PEER_ID, make_tui_transport())
    connections.register_client(TUI_PEER_ID, client)

    -- Auto-subscribe to hub channel for agent lifecycle events.
    -- This registers the TUI in Lua's subscription system, enabling
    -- broadcast_hub_event() to include TUI alongside browser clients.
    client:on_message({
        type = "subscribe",
        channel = "hub",
        subscriptionId = "tui_hub",
    })
end)

-- Called when TUI is shutting down
tui.on_disconnected(function()
    log.info("TUI disconnected")
    connections.unregister_client(TUI_PEER_ID)
end)

-- Called for each message from TuiRunner
tui.on_message(function(msg)
    local client = connections.get_client(TUI_PEER_ID)

    if not client then
        -- TUI message before on_connected fired (startup race)
        log.warn("TUI message but no client registered, creating")
        client = Client.new(TUI_PEER_ID, make_tui_transport())
        connections.register_client(TUI_PEER_ID, client)
    end

    connections.track_message()

    -- Route message to client (same protocol as browser)
    local ok, err = pcall(client.on_message, client, msg)
    if not ok then
        log.error(string.format("Error handling TUI message: %s", tostring(err)))
        client:send({
            type = "error",
            error = "Internal error processing message",
        })
    end
end)

-- ============================================================================
-- Module Interface
-- ============================================================================

local M = {
    TUI_PEER_ID = TUI_PEER_ID,
}

-- Lifecycle hooks for hot-reload
function M._before_reload()
    log.info("tui.lua reloading")
end

function M._after_reload()
    log.info("tui.lua reloaded")
end

log.info("TUI handler loaded")

return M
