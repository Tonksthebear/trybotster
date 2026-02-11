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

local Agent = require("lib.agent")

-- Table stores channel_id for use outside the callback (heartbeat, signal relay).
-- Inside the callback, channel_id is passed as the second argument by the primitive.
local ch = {}
local conn = nil

-- Connect with E2E encryption enabled (transparent OlmEnvelope handling)
conn = action_cable.connect({ crypto = true })

-- Subscribe to HubCommandChannel
-- The callback receives (message, channel_id) from the primitive.
ch.hub = action_cable.subscribe(conn, "HubCommandChannel",
    { hub_id = hub.server_id(), start_from = 0 },
    function(message, channel_id)
        local msg_type = message.type

        if msg_type == "signal" then
            -- If Rust couldn't decrypt the envelope (Olm session mismatch),
            -- tell the browser its session is stale so it can re-pair.
            if message.decrypt_failed then
                log.warn("Signal decryption failed for browser " ..
                    tostring(message.browser_identity) .. ", sending session_invalid")
                action_cable.perform(channel_id, "signal", {
                    browser_identity = message.browser_identity,
                    envelope = { type = "session_invalid", message = "Crypto session expired. Please re-pair." }
                })
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

-- Build agent list from Lua's agent registry (the source of truth).
-- Returns the format Rails expects: [{ session_key, last_invocation_url? }]
local function agent_list()
    local result = {}
    for _, agent in ipairs(Agent.list()) do
        result[#result + 1] = {
            session_key = agent:agent_key(),
            last_invocation_url = agent.invocation_url,
        }
    end
    return result
end

-- Send heartbeat helper (used by timer and agent lifecycle hooks)
local function send_heartbeat()
    if ch.hub then
        action_cable.perform(ch.hub, "heartbeat", { agents = agent_list() })
    end
end

-- Application-level heartbeat (30s interval, 3 chances before 90s timeout)
-- This syncs agent status with Rails — NOT an ActionCable protocol heartbeat
timer.every(30, send_heartbeat)

-- Immediately sync agent list when agents are created or deleted
hooks.on("agent_created", "heartbeat_on_agent_created", function()
    send_heartbeat()
end)

hooks.on("agent_deleted", "heartbeat_on_agent_deleted", function()
    send_heartbeat()
end)

-- Relay outgoing WebRTC signals (pre-encrypted by Rust) through ActionCable
events.on("outgoing_signal", function(data)
    if ch.hub then
        action_cable.perform(ch.hub, "signal", data)
    end
end)

log.info("Hub commands plugin loaded")
return {}
