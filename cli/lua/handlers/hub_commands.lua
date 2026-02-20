-- Hub Command Channel (Lua plugin)
--
-- Manages the HubCommandChannel subscription:
--   - Routes decrypted signals to WebRTC primitives
--   - Routes command messages to Lua event system
--   - Acks commands by sequence number
--   - Sends application-level heartbeat every 30s (agent status sync)
--   - Relays outgoing WebRTC signals through encrypted ActionCable pipe
--
-- NOTE: ActionCable protocol pings are handled automatically by the
-- action_cable primitive (Rust). The 30s heartbeat here is application-
-- level HubCommandChannel business logic, NOT protocol-level.
--
-- Hot-reload safe: connection, subscription, timer, and event listener
-- are stored in hub.state so they survive reloads without orphaning.

local state = require("hub.state")
local Agent = require("lib.agent")

-- Persistent handles across hot-reloads
local handles = state.get("hub_commands.handles", {})

-- Skip network connections in unit test mode (BOTSTER_ENV=test)
if config.env("BOTSTER_ENV") == "test" then
    log.info("Test mode: skipping ActionCable connection")
    return {}
end

-- Reuse existing connection or create a new one
if not handles.conn then
    handles.conn = action_cable.connect({ crypto = true })
end

-- Subscribe to HubCommandChannel (reuse existing or create new)
-- The callback is always replaced on reload so routing logic stays current.
if handles.channel then
    action_cable.unsubscribe(handles.channel)
end

handles.channel = action_cable.subscribe(handles.conn, "HubCommandChannel",
    { hub_id = hub.server_id(), start_from = 0 },
    function(message, channel_id)
        local msg_type = message.type

        if msg_type == "signal" then
            -- If Rust couldn't decrypt the envelope (Olm session mismatch),
            -- tell the browser its session is stale so it can re-pair.
            if message.decrypt_failed then
                log.warn("Signal decryption failed for browser " ..
                    tostring(message.browser_identity) .. ", requesting ratchet restart")
                hub.request_ratchet_restart(message.browser_identity)
                return
            end

            -- Primitive already decrypted the OlmEnvelope.
            -- message.envelope is now the decrypted plaintext JSON:
            --   { type = "offer"|"ice"|"answer", sdp = ..., candidate = ... }
            local signal_data = message.envelope
            local signal_type = signal_data and signal_data.type

            if signal_type == "offer" then
                hub.handle_webrtc_offer(
                    message.browser_identity,
                    signal_data.sdp
                )
            elseif signal_type == "ice" then
                hub.handle_ice_candidate(
                    message.browser_identity,
                    signal_data.candidate
                )
            else
                log.warn("Unknown signal type: " .. tostring(signal_type))
            end

        elseif msg_type == "message" then
            local event_type = message.event_type or ""

            if event_type == "create_agent" or event_type == "agent_cleanup" then
                local payload = message.payload or {}
                events.emit("command_message", {
                    type = (event_type == "agent_cleanup") and "delete_agent" or "create_agent",
                    event_type = event_type,
                    issue_or_branch = payload.issue_number and tostring(payload.issue_number),
                    prompt = payload.prompt or payload.context or payload.comment_body,
                    repo = config.env("BOTSTER_REPO") or hub.detect_repo(),
                    invocation_url = payload.issue_url,
                    agent_id = payload.issue_number and
                        ((config.env("BOTSTER_REPO") or hub.detect_repo() or ""):gsub("/", "-")
                        .. "-" .. tostring(payload.issue_number)),
                    delete_worktree = false,
                })
            else
                log.warn("Unhandled command event_type: " .. event_type)
            end

            -- Ack by sequence
            if message.sequence then
                action_cable.perform(channel_id, "ack", { sequence = message.sequence })
            end
        end
    end
)

-- Send heartbeat helper (used by timer)
local function send_heartbeat()
    if handles.channel then
        action_cable.perform(handles.channel, "heartbeat", {})
    end
end

-- Cancel old heartbeat timer before creating a new one
if handles.heartbeat_timer then
    timer.cancel(handles.heartbeat_timer)
end

-- Application-level heartbeat (30s interval, 3 chances before 90s timeout)
-- Keeps the hub marked alive in Rails — NOT an ActionCable protocol heartbeat
handles.heartbeat_timer = timer.every(30, send_heartbeat)

-- Unsubscribe old event listener before re-registering
if handles.signal_event_sub then
    events.off(handles.signal_event_sub)
end

-- Relay outgoing WebRTC signals (pre-encrypted by Rust) through ActionCable
handles.signal_event_sub = events.on("outgoing_signal", function(data)
    if handles.channel then
        action_cable.perform(handles.channel, "signal", data)
    end
end)

-- ============================================================================
-- Module Interface
-- ============================================================================

local M = {}

function M._before_reload()
    -- Connection and subscription are stored in hub.state — no cleanup needed
    -- here. The module top-level code unsubscribes/resubscribes the channel
    -- and cancels/recreates the timer on reload.
    log.info("hub_commands.lua reloading")
end

function M._after_reload()
    log.info("hub_commands.lua reloaded")
end

log.info("Hub commands plugin loaded")
return M
