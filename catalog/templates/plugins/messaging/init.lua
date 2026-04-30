-- @template Messaging
-- @description Session inbox messaging with caller-only receive access
-- @category plugins
-- @dest plugins/messaging/init.lua
-- @scope device
-- @version 1.1.0

-- Messaging plugin
--
-- Session communication: structured inbox messaging and PTY delivery.
--
-- For local messaging (same hub), works standalone.
-- For cross-hub messaging, Hub.get() auto-connects transparently via
-- hub_discovery - no manual hub registration or discovery needed.
--
-- Tools:
--   post_message     - post structured message to a session inbox (supports agent_label)
--   receive_messages - drain your inbox of pending messages

local Hub = require("lib.hub")
local Agent = require("lib.agent")

mcp.tool("post_message", {
    description = "Post a structured message to a session inbox. The target session receives a PTY doorbell and calls receive_messages() to get the envelope.",
    input_schema = {
        type = "object",
        properties = {
            hub_id = {
                type = "string",
                description = "Hub ID where the session lives. Omit for local hub.",
            },
            session_uuid = {
                type = "string",
                description = "Session UUID.",
            },
            agent_label = {
                type = "string",
                description = "Agent label (alternative to session_uuid). Resolved by label lookup on the local hub.",
            },
            payload = {
                description = "Message payload. Any JSON value.",
            },
            msg_type = {
                type = "string",
                description = "Message type: 'message' (default), 'task', 'result', 'query'. Use 'notify' to write directly to PTY instead of inbox.",
            },
            reply_to = {
                type = "string",
                description = "msg_id this is a reply to, for threading.",
            },
            expires_in = {
                type = "number",
                description = "Seconds until message expires from inbox (default 3600).",
            },
        },
        required = { "payload" },
    },
}, function(params, context)
    -- Resolve target session: prefer session_uuid, fall back to label lookup.
    local target_session_uuid = params.session_uuid
    if not target_session_uuid and params.agent_label then
        for _, agent in ipairs(Agent.list()) do
            if agent.label == params.agent_label then
                target_session_uuid = agent.session_uuid
                break
            end
        end
        if not target_session_uuid then
            return json.encode({ error = string.format("No agent found with label '%s'", params.agent_label) })
        end
    end
    if not target_session_uuid then
        return json.encode({ error = "Either session_uuid or agent_label is required" })
    end

    -- Resolve sender display name: use label if available, else session uuid
    local sender_key = context.session_uuid or "unknown"
    local sender_display = sender_key
    if sender_key ~= "unknown" then
        local sender = Agent.get(sender_key)
        if sender and sender.label and sender.label ~= "" then
            sender_display = sender.label
        end
    end

    local result = Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):post(target_session_uuid, {
            type          = params.msg_type,
            payload       = params.payload,
            reply_to      = params.reply_to,
            expires_in    = params.expires_in,
            from_agent_id = sender_key,
            from_label    = sender_display,
        })
    end)
    return json.encode(result)
end)

mcp.tool("receive_messages", {
    description = "Drain your inbox - returns all pending messages and clears them. Call this after receiving a botster-mcp doorbell notification in your PTY.",
    input_schema = {
        type = "object",
        properties = {
            hub_id = {
                type = "string",
                description = "Hub ID. Omit for local hub.",
            },
        },
    },
}, function(params, context)
    if params.session_uuid ~= nil then
        error("receive_messages: session_uuid is not allowed; only the caller inbox can be drained")
    end

    local caller_session_uuid = context.session_uuid
    if not caller_session_uuid or caller_session_uuid == "" then
        error("receive_messages: caller session context is required")
    end

    local messages = Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):receive_messages(caller_session_uuid)
    end)
    return json.encode(messages)
end)

log.info("Messaging plugin loaded")

return {}
