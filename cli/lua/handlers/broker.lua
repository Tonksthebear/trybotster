-- Broker restart recovery handler.
--
-- Fires when try_connect_broker() reconnects to an *existing* broker process
-- (as opposed to spawning a fresh one). This happens when the Hub process
-- restarts while the broker is still running and holding PTY file descriptors.
--
-- Flow:
--   1. Scan all worktrees + data_dir/agents/ for context.json files written by surviving agents.
--   2. For each surviving PTY session, call hub.create_ghost_pty() to create
--      a shadow-screen-only handle and register the broker session_id routing.
--   3. Register ghost handles in HandleCache via hub.register_agent() so that
--      BrokerPtyOutput frames can be routed to the correct agent PTY.
--   4. Replay broker scrollback into each ghost handle's shadow screen so
--      connecting browsers see the correct terminal state on reconnect.

local Agent = require("lib.agent")

--- Derive the agent key from repo and branch_name using the same formula
--- used by Agent:agent_key() in lib/agent.lua.
-- @param repo string  e.g. "owner/repo"
-- @param branch_name string
-- @return string agent key
local function derive_agent_key(repo, branch_name)
    local repo_safe = (repo or ""):gsub("/", "-")
    local branch_safe = (branch_name or ""):gsub("/", "-")
    return repo_safe .. "-" .. branch_safe
end

--- Process a single context.json file and append a ghost_info to the list.
-- @param context_path string  Absolute path to the context.json file
-- @param in_worktree boolean  True for worktree agents, false for main-branch agents
-- @param ghost_infos table    Array to append the resulting ghost_info to (modified in place)
-- @param seen_keys table      Set of agent_keys already processed (prevents duplicates)
local function process_context_file(context_path, in_worktree, ghost_infos, seen_keys)
    local ok, content = pcall(fs.read, context_path)
    if not ok or not content then
        log.debug(string.format("[broker] Could not read %s: %s", context_path, tostring(content)))
        return
    end

    local ok2, ctx = pcall(json.decode, content)
    if not ok2 or not ctx or not ctx.metadata then
        log.debug(string.format("[broker] Skipping %s: no metadata", context_path))
        return
    end

    local meta = ctx.metadata
    local agent_key = derive_agent_key(ctx.repo, ctx.branch_name)
    -- Skip if we could not derive a meaningful key or already processed.
    if agent_key == "-" or agent_key == "" then
        log.debug(string.format("[broker] Skipping %s: could not derive agent_key", context_path))
        return
    end
    if seen_keys[agent_key] then
        log.debug(string.format("[broker] Skipping %s: already processed as %s", context_path, agent_key))
        return
    end
    seen_keys[agent_key] = true

    -- Collect ghost handles for all surviving PTY sessions.
    -- broker_session_N keys are 0-based sequential integers; we stop
    -- at the first missing index so there are no gaps in the handles array.
    local ghost_handles = {}
    local pty_index = 0
    while true do
        local session_id = tonumber(meta["broker_session_" .. pty_index])
        if not session_id then break end

        local rows = tonumber(meta["broker_pty_rows_" .. pty_index]) or 24
        local cols = tonumber(meta["broker_pty_cols_" .. pty_index]) or 80

        local ok3, handle = pcall(
            hub.create_ghost_pty, agent_key, pty_index, session_id, rows, cols
        )
        if ok3 and handle then
            ghost_handles[#ghost_handles + 1] = handle
        else
            log.warn(string.format(
                "[broker] create_ghost_pty failed for %s pty=%d: %s",
                agent_key, pty_index, tostring(handle)
            ))
            break
        end
        pty_index = pty_index + 1
    end

    if #ghost_handles == 0 then
        log.debug(string.format("[broker] No broker sessions found for %s", agent_key))
        return
    end

    -- Register ghost handles in HandleCache.
    -- This must happen before any BrokerPtyOutput frames can arrive so
    -- get_agent_by_key() finds the handles during routing.
    local ok4, agent_idx = pcall(hub.register_agent, agent_key, ghost_handles)
    if not ok4 then
        log.warn(string.format(
            "[broker] register_agent failed for %s: %s",
            agent_key, tostring(agent_idx)
        ))
        return
    end

    -- Replay broker scrollback into each ghost handle's vt100 shadow screen.
    -- Browsers that connect before the real agent respawns will see the
    -- correct terminal state via get_snapshot().
    for i, handle in ipairs(ghost_handles) do
        local ghost_pty_index = i - 1
        local session_id = tonumber(meta["broker_session_" .. ghost_pty_index])
        if session_id then
            local snapshot = hub.get_pty_snapshot_from_broker(session_id)
            if snapshot and #snapshot > 0 then
                local ok5, err = pcall(function() handle:feed_output(snapshot) end)
                if ok5 then
                    log.info(string.format(
                        "[broker] Replayed %d bytes → %s pty=%d",
                        #snapshot, agent_key, ghost_pty_index
                    ))
                else
                    log.warn(string.format(
                        "[broker] feed_output failed for %s pty=%d: %s",
                        agent_key, ghost_pty_index, tostring(err)
                    ))
                end
            end
        end
    end

    log.info(string.format(
        "[broker] Ghost agent registered: %s (%d ptys, index=%d, in_worktree=%s)",
        agent_key, #ghost_handles, agent_idx, tostring(in_worktree)
    ))

    -- Use worktree_path from context if available.
    -- Old context files (written before worktree_path was added) will have nil here;
    -- for worktree agents we can derive the path by stripping "/.botster/context.json"
    -- from the context_path itself, since that's where we found the file.
    local wt_path = ctx.worktree_path
    if not wt_path and in_worktree then
        wt_path = context_path:match("^(.+)/%.botster/context%.json$")
    end

    -- Build a ghost info table matching Agent:info() structure so the TUI
    -- can render this agent. Status "ghost" lets clients style it differently.
    local ghost_info = {
        id           = agent_key,
        display_name = ctx.branch_name or agent_key,
        title        = nil,
        cwd          = wt_path,
        profile_name = ctx.profile_name,
        repo         = ctx.repo,
        metadata     = meta,
        branch_name  = ctx.branch_name,
        worktree_path = wt_path,
        in_worktree  = in_worktree,
        status       = "ghost",
        sessions     = {},
        notification = false,
        has_server_pty = false,
        server_running = false,
        port         = nil,
        created_at   = nil,
    }
    ghost_infos[#ghost_infos + 1] = ghost_info
end

events.on("broker_reconnected", function()
    log.info("[broker] Hub restarted — scanning for surviving agents")

    local ghost_infos = {}
    -- Track processed agent keys to skip duplicates (e.g., if a worktree context
    -- and a data_dir context somehow both exist for the same agent).
    local seen_keys = {}

    -- Scan worktree agents: each worktree may have <path>/.botster/context.json
    local worktrees = worktree.list()
    for _, wt in ipairs(worktrees) do
        local context_path = wt.path .. "/.botster/context.json"
        if fs.exists(context_path) then
            process_context_file(context_path, true, ghost_infos, seen_keys)
        end
    end

    -- Scan main-branch agents: stored in <data_dir>/.botster/agents/<key>/context.json
    local data_dir = config.data_dir and config.data_dir() or nil
    if data_dir then
        local agents_dir = data_dir .. "/.botster/agents"
        if fs.exists(agents_dir) then
            local ok_ls, entries = pcall(fs.list_dir, agents_dir)
            if ok_ls and entries then
                for _, entry_name in ipairs(entries) do
                    local context_path = agents_dir .. "/" .. entry_name .. "/context.json"
                    if fs.exists(context_path) then
                        process_context_file(context_path, false, ghost_infos, seen_keys)
                    end
                end
            else
                log.debug(string.format("[broker] Could not list %s: %s", agents_dir, tostring(entries)))
            end
        end
    end

    -- Surface ghost agents in the TUI by firing agent_created for each and
    -- then broadcasting a combined agent_list (real + ghosts).
    if #ghost_infos > 0 then
        -- Required inside the handler body (hot-reload safe).
        local state = require("hub.state")
        local connections = require("handlers.connections")

        -- Persist ghost infos so Agent.all_info() serves them to late-connecting clients.
        local ghost_registry = state.get("ghost_agent_registry", {})
        for _, gi in ipairs(ghost_infos) do
            ghost_registry[gi.id] = gi
        end

        local ok, err = pcall(function()
            for _, ghost_info in ipairs(ghost_infos) do
                hooks.notify("agent_created", ghost_info)
            end
            -- Agent.all_info() now includes ghosts from the registry — no manual merge needed.
            connections.broadcast_hub_event("agent_list", { agents = Agent.all_info() })
        end)

        if not ok then
            log.warn(string.format("[broker] Failed to broadcast ghost agents: %s", tostring(err)))
        else
            log.info(string.format(
                "[broker] Broadcast %d ghost agent(s) to TUI", #ghost_infos
            ))
        end
    end
end)
