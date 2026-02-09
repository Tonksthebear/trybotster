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

local hub_ch = nil
local conn = nil

-- Connect with E2E encryption enabled (transparent OlmEnvelope handling)
conn = action_cable.connect({ crypto = true })

-- Subscribe to HubCommandChannel
hub_ch = action_cable.subscribe(conn, "HubCommandChannel",
    { hub_id = hub.server_id(), start_from = 0 },
    function(message)
        local msg_type = message.type

        if msg_type == "signal" then
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

            -- Skip legacy events
            if event_type == "terminal_connected"
                or event_type == "terminal_disconnected"
                or event_type == "browser_wants_preview" then
                log.debug("Ignoring legacy event: " .. event_type)
            else
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
            end

            -- Ack by sequence
            if message.sequence then
                action_cable.perform(hub_ch, "ack", { sequence = message.sequence })
            end
        end
    end
)

-- Application-level heartbeat (30s interval, 3 chances before 90s timeout)
-- This syncs agent status with Rails — NOT an ActionCable protocol heartbeat
timer.every(30, function()
    if hub_ch then
        action_cable.perform(hub_ch, "heartbeat", { agents = hub.agent_list() })
    end
end)

-- Relay outgoing WebRTC signals (pre-encrypted by Rust) through ActionCable
events.on("outgoing_signal", function(data)
    if hub_ch then
        action_cable.perform(hub_ch, "signal", data)
    end
end)

log.info("Hub commands plugin loaded")
return {}
