-- GitHub event integration (pure Lua plugin)
--
-- Subscribes to Github::EventsChannel for this repo and routes
-- incoming events to the command_message event system.
-- Uses a separate ActionCable connection without crypto (GitHub
-- events are plaintext over TLS, no E2E encryption needed).

local repo = config.env("BOTSTER_REPO")
if not repo then repo = hub.detect_repo() end
if not repo then
    log.info("GitHub plugin: disabled (no repo detected)")
    return {}
end

-- No crypto flag -- GitHub events don't use E2E encryption
local conn = action_cable.connect()

local channel_id = action_cable.subscribe(conn, "Github::EventsChannel",
    { repo = repo },
    function(message)
        local payload = message.payload or {}

        if message.event_type == "agent_cleanup" then
            local repo_safe = (message.repo or ""):gsub("/", "-")
            if payload.issue_number then
                events.emit("command_message", {
                    type = "delete_agent",
                    agent_id = repo_safe .. "-" .. tostring(payload.issue_number),
                    delete_worktree = false,
                })
            end
        else
            events.emit("command_message", {
                type = "create_agent",
                issue_or_branch = payload.issue_number and tostring(payload.issue_number),
                prompt = payload.prompt or payload.context or payload.comment_body,
                repo = message.repo,
                invocation_url = payload.issue_url,
            })
        end

        action_cable.perform(channel_id, "ack", { id = message.id })
    end
)

events.on("shutdown", function()
    if conn then action_cable.close(conn) end
end)

log.info(string.format("GitHub plugin loaded for %s", repo))
return {}
