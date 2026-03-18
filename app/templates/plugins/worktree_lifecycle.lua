-- @template Worktree Lifecycle
-- @description Copy files into new worktrees and clean up on deletion
-- @category plugins
-- @dest plugins/worktree-lifecycle/init.lua
-- @scope device
-- @version 3.0.0

-- Worktree Lifecycle plugin
--
-- Hooks into worktree_created and worktree_deleted events.
-- Uncomment the examples below to match your project's needs.

local hooks = require("hub.hooks")
-- json, fs, config, worktree, log are Rust-provided globals (not requireable)

--- Called when a new worktree is created.
-- ctx fields: path, branch, repo, agent_key, metadata
local function on_worktree_created(ctx)
    local repo_root = ctx.repo or worktree.repo_root()
    if not repo_root then return end

    -- Example: auto-trust worktree in Claude Code so it doesn't prompt
    -- json.file_set("~/.claude.json",
    --     "projects." .. ctx.path .. ".hasTrustDialogAccepted", true)

    -- Example: copy .env from main repo into the worktree
    -- local src = repo_root .. "/.env"
    -- local dst = ctx.path .. "/.env"
    -- if fs.exists(src) then
    --     fs.copy(src, dst)
    --     log.info("[worktree-lifecycle] Copied .env to " .. ctx.path)
    -- end

    -- Example: copy credential files
    -- local creds = fs.glob(repo_root .. "/config/credentials/*.key")
    -- for _, src in ipairs(creds or {}) do
    --     local name = src:match("([^/]+)$")
    --     fs.copy(src, ctx.path .. "/config/credentials/" .. name)
    -- end

    -- Example: copy from a patterns file (one glob per line)
    -- worktree.copy_from_patterns(repo_root, ctx.path, repo_root .. "/.botster/worktree_include")
end

--- Called when a worktree is about to be deleted.
-- ctx fields: path, branch, agent_key, session_uuid
local function on_worktree_deleted(ctx)
    -- Example: remove worktree from Claude's trusted projects
    -- json.file_delete("~/.claude.json", "projects." .. ctx.path)

    -- Example: clean up temp files
    -- local tmp = ctx.path .. "/tmp/botster"
    -- if fs.exists(tmp) then
    --     fs.remove_dir(tmp)
    -- end
end

hooks.on("worktree_created", "worktree_lifecycle.created", function(ctx)
    local ok, err = pcall(on_worktree_created, ctx)
    if not ok then
        log.warn("[worktree-lifecycle] worktree_created error: " .. tostring(err))
    end
end)

hooks.on("worktree_deleted", "worktree_lifecycle.deleted", function(ctx)
    local ok, err = pcall(on_worktree_deleted, ctx)
    if not ok then
        log.warn("[worktree-lifecycle] worktree_deleted error: " .. tostring(err))
    end
end)

log.info("[worktree-lifecycle] Plugin loaded")

return {}
