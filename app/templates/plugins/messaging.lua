-- @template Messaging
-- @description Agent-to-agent inbox messaging with caller-only receive access
-- @category plugins
-- @dest plugins/messaging/init.lua
-- @scope device
-- @version 1.1.0

-- Messaging plugin
--
-- Agent-to-agent communication: structured inbox messaging and PTY delivery.
--
-- For local messaging (same hub), works standalone.
-- For cross-hub messaging, Hub.get() auto-connects transparently via
-- hub_discovery - no manual hub registration or discovery needed.
--
-- Tools:
--   post_message     - post structured message to an agent's inbox (supports agent_label)
--   receive_messages - drain your inbox of pending messages

local Hub = require("lib.hub")
local Agent = require("lib.agent")

mcp.tool("post_message", {
    description = "Post a structured message to an agent's inbox. The agent receives a PTY doorbell and calls receive_messages() to get the envelope.",
    input_schema = {
        type = "object",
        properties = {
            hub_id = {
                type = "string",
                description = "Hub ID where the agent lives. Omit for local hub.",
            },
            agent_id = {
                type = "string",
                description = "Agent key/ID.",
            },
            agent_label = {
                type = "string",
                description = "Agent label (alternative to agent_id). Resolved by label lookup on the local hub.",
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
        required = { "agent_id", "payload" },
    },
}, function(params, context)
    -- Resolve target agent: prefer agent_id, fall back to label lookup
    local target_agent_id = params.agent_id
    if not target_agent_id and params.agent_label then
        for _, agent in ipairs(Agent.list()) do
            if agent.label == params.agent_label then
                target_agent_id = agent.session_uuid
                break
            end
        end
        if not target_agent_id then
            return json.encode({ error = string.format("No agent found with label '%s'", params.agent_label) })
        end
    end
    if not target_agent_id then
        return json.encode({ error = "Either agent_id or agent_label is required" })
    end

    -- Resolve sender display name: use label if available, else agent key
    local sender_key = context.session_uuid or context.agent_key or "unknown"
    local sender_display = sender_key
    if sender_key ~= "unknown" then
        local sender = Agent.get(sender_key)
        if sender and sender.label and sender.label ~= "" then
            sender_display = sender.label
        end
    end

    local result = Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):post(target_agent_id, {
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
    if params.agent_id ~= nil then
        error("receive_messages: agent_id is not allowed; only the caller inbox can be drained")
    end

    local caller_agent_id = context.session_uuid or context.agent_key
    if not caller_agent_id or caller_agent_id == "" then
        error("receive_messages: caller agent context is required")
    end

    local messages = Hub.call_safely(params.hub_id, function()
        return Hub.get(params.hub_id):receive_messages(caller_agent_id)
    end)
    return json.encode(messages)
end)

log.info("Messaging plugin loaded")

return {}
