-- Shared queue policy for hub orchestration commands invoked from plugin workers.
--
-- Orchestration commands can emit lifecycle hooks, spawn processes, touch
-- worktrees, or otherwise reenter plugin workers. Plugin workers must receive a
-- quick request acknowledgement and correlate completion through events instead
-- of blocking across the worker-parent hub boundary.

local M = {}

local QUEUED_COMMANDS = {
    create_agent = true,
}

function M.is_queued_command(command)
    return type(command) == "table" and QUEUED_COMMANDS[command.type] == true
end

function M.ack(command)
    local metadata = command.metadata or {}
    return {
        ok = true,
        status = "queued",
        request_id = command.request_id,
        assignment_id = command.assignment_id or metadata.assignment_id,
    }
end

function M.parent_request(command)
    return {
        type = "hub_orchestration",
        command = command,
    }
end

function M.enqueue(command, dispatch)
    local request_id = command.request_id

    local function run_command()
        local ok, err = pcall(dispatch, command)
        if not ok then
            log.warn(string.format("Hub orchestration command failed: %s request_id=%s error=%s",
                tostring(command.type), tostring(request_id), tostring(err)))
        end
    end

    if type(timer) == "table" and type(timer.after) == "function" then
        timer.after(0, run_command)
    else
        log.warn(string.format("Hub orchestration command not dispatched because timer.after is unavailable: %s request_id=%s",
            tostring(command.type), tostring(request_id)))
    end

    return M.ack(command)
end

return M
