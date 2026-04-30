-- Internal command ingress for hub/application/plugin-originated commands.
--
-- External clients reach command handlers through lib.client via WebRTC, TUI,
-- or socket transports. Internal producers (Rails/GitHub adapters, MCP tools,
-- hub-to-hub RPC) use this module so they enter the same Client:on_message
-- path and hit the same before_hub_command / before_command hooks.

local state = require("hub.state")
local Client = require("lib.client")

local M = {}

local clients = state.get("internal_clients", {})

local function make_transport(holder)
    return {
        type = "internal",
        send = function(msg)
            holder.frames[#holder.frames + 1] = msg
        end,
        send_binary = function(data)
            holder.binary[#holder.binary + 1] = data
        end,
        create_pty_forwarder = function()
            return {
                stop = function() end,
                is_active = function() return false end,
            }
        end,
    }
end

local function client_for(source)
    source = source or "system"
    local peer_id = "internal:" .. tostring(source)
    local existing = clients[peer_id]
    if existing then return existing end

    local holder = { frames = {}, binary = {} }
    local client = Client.new(peer_id, make_transport(holder))
    client._internal_transport_holder = holder
    clients[peer_id] = client
    return client
end

--- Dispatch a canonical command through Client:on_message.
-- @param source string Short source label, e.g. "github" or "mcp".
-- @param command table Command envelope with `type`.
-- @param opts table? { subscription_id = string }
-- @return table { client, frames, binary }
function M.dispatch(source, command, opts)
    assert(type(command) == "table", "internal_client.dispatch requires a command table")
    assert(type(command.type or command.command) == "string", "internal command requires type or command")

    opts = opts or {}
    local client = client_for(source)
    local holder = client._internal_transport_holder or { frames = {}, binary = {} }
    holder.frames = {}
    holder.binary = {}
    client._internal_transport_holder = holder

    local sub_id = opts.subscription_id or "internal_hub"
    local previous_subscription = client.subscriptions[sub_id]
    client.subscriptions[sub_id] = previous_subscription or {
        channel = "hub",
    }

    local ok, err = pcall(function()
        client:on_message({
            subscriptionId = sub_id,
            data = command,
        })
    end)

    if previous_subscription then
        client.subscriptions[sub_id] = previous_subscription
    else
        client.subscriptions[sub_id] = nil
    end

    if not ok then
        error(err)
    end

    return {
        client = client,
        frames = holder.frames,
        binary = holder.binary,
    }
end

return M
