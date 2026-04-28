-- @template Rails Worktree Lifecycle
-- @description Copy Rails repo-local files into new worktrees and trust Mise
-- @category plugins
-- @dest plugins/rails-worktree-lifecycle/init.lua
-- @scope repo
-- @version 1.0.0

local hooks = require("hub.hooks")

local db = plugin.db{
    version = 1,
    models = {
        worktrees = {
            id = true,
            path = { "text", required = true, unique = true },
            branch = { "text" },
            prefix = { "text", required = true },
            created_at = { "integer", required = true },
        },
    },
}

local copied_files = {
    ".gitignore",
    ".ruby-lsp/.gitignore",
}

local copied_directories = {
    "tmp/tailwindplus_elements_previews",
}

local env_begin = "# BEGIN BOTSTER RAILS WORKTREE"
local env_end = "# END BOTSTER RAILS WORKTREE"
local mise_env_begin = "# BEGIN BOTSTER RAILS WORKTREE ENV"
local mise_env_end = "# END BOTSTER RAILS WORKTREE ENV"

local function dirname(path)
    return path:match("^(.*)/[^/]+$") or "."
end

local function basename(path)
    return tostring(path):gsub("/+$", ""):match("([^/]+)$") or "rails_app"
end

local function shell_quote(value)
    return "'" .. tostring(value):gsub("'", "'\\''") .. "'"
end

local function toml_quote(value)
    return '"' .. tostring(value):gsub("\\", "\\\\"):gsub('"', '\\"') .. '"'
end

local function sanitize_identifier(value)
    local sanitized = tostring(value):lower():gsub("[^a-z0-9_]+", "_"):gsub("^_+", ""):gsub("_+$", "")
    if sanitized == "" then sanitized = "worktree" end
    if sanitized:match("^[0-9]") then sanitized = "wt_" .. sanitized end
    return sanitized
end

local function truncate_identifier(value, max_length)
    if #value <= max_length then return value end
    return value:sub(1, max_length):gsub("_+$", "")
end

local function source_repo(ctx)
    return (ctx.metadata and ctx.metadata.target_path) or worktree.repo_root()
end

local function run_command(command, success_message, failure_message)
    local ok = os.execute(command)
    if ok == true or ok == 0 then
        log.info(success_message)
        return true
    end

    log.warn(failure_message)
    return false
end

local function database_names(prefix)
    return {
        prefix .. "_development",
        prefix .. "_development_cache",
        prefix .. "_development_queue",
        prefix .. "_development_cable",
        prefix .. "_test",
        prefix .. "_test_cache",
        prefix .. "_test_queue",
        prefix .. "_test_cable",
    }
end

local function prepare_node_dependencies(repo_root, worktree_path)
    local dest_node_modules = worktree_path .. "/node_modules"
    if fs.exists(dest_node_modules) then
        log.info("[rails-worktree-lifecycle] node_modules already present in " .. worktree_path)
        return
    end

    local src_node_modules = repo_root .. "/node_modules"
    if fs.exists(src_node_modules) then
        local command = "cp -cR " .. shell_quote(src_node_modules) .. " " .. shell_quote(dest_node_modules) .. " >/dev/null 2>&1"
        if run_command(
            command,
            "[rails-worktree-lifecycle] Copied node_modules to " .. worktree_path,
            "[rails-worktree-lifecycle] cp -cR node_modules failed for " .. worktree_path
        ) then
            return
        end
    end

    local install_command = nil
    if fs.exists(worktree_path .. "/bun.lock") then
        install_command = "bun install"
    elseif fs.exists(worktree_path .. "/pnpm-lock.yaml") then
        install_command = "pnpm install --frozen-lockfile"
    elseif fs.exists(worktree_path .. "/yarn.lock") then
        install_command = "yarn install --frozen-lockfile"
    elseif fs.exists(worktree_path .. "/package-lock.json") then
        install_command = "npm ci --prefer-offline --no-audit"
    elseif fs.exists(worktree_path .. "/package.json") then
        install_command = "npm install --prefer-offline --no-audit"
    end

    if not install_command then
        log.info("[rails-worktree-lifecycle] No JS package manifest in " .. worktree_path)
        return
    end

    run_command(
        "cd " .. shell_quote(worktree_path) .. " && " .. install_command .. " >/dev/null 2>&1",
        "[rails-worktree-lifecycle] Installed JS dependencies in " .. worktree_path,
        "[rails-worktree-lifecycle] JS dependency install failed in " .. worktree_path
    )
end

local function copy_file(repo_root, worktree_path, relative_path)
    local src = repo_root .. "/" .. relative_path
    local dst = worktree_path .. "/" .. relative_path

    if not fs.exists(src) then
        log.warn("[rails-worktree-lifecycle] Missing " .. relative_path .. " in " .. repo_root)
        return
    end

    local dir = dirname(dst)
    local made_dir, mkdir_err = fs.mkdir(dir)
    if not made_dir then
        log.warn("[rails-worktree-lifecycle] Could not create " .. dir .. ": " .. tostring(mkdir_err))
        return
    end

    local copied, copy_err = fs.copy(src, dst)
    if copied then
        log.info("[rails-worktree-lifecycle] Copied " .. relative_path .. " to " .. worktree_path)
    else
        log.warn("[rails-worktree-lifecycle] Could not copy " .. relative_path .. ": " .. tostring(copy_err))
    end
end

local function copy_directory(repo_root, worktree_path, relative_path)
    local src = repo_root .. "/" .. relative_path
    local dst = worktree_path .. "/" .. relative_path

    if not fs.exists(src) then
        log.info("[rails-worktree-lifecycle] No " .. relative_path .. " in " .. repo_root)
        return
    end

    local dir = dirname(dst)
    local made_dir, mkdir_err = fs.mkdir(dir)
    if not made_dir then
        log.warn("[rails-worktree-lifecycle] Could not create " .. dir .. ": " .. tostring(mkdir_err))
        return
    end

    if fs.exists(dst) then
        log.info("[rails-worktree-lifecycle] " .. relative_path .. " already present in " .. worktree_path)
        return
    end

    local command = "cp -cR " .. shell_quote(src) .. " " .. shell_quote(dst) .. " >/dev/null 2>&1"
    run_command(
        command,
        "[rails-worktree-lifecycle] Copied " .. relative_path .. " to " .. worktree_path,
        "[rails-worktree-lifecycle] Could not copy " .. relative_path .. " to " .. worktree_path
    )
end

local function write_database_env(repo_root, worktree_path, branch)
    local app_name = sanitize_identifier(basename(repo_root))
    local branch_name = sanitize_identifier(branch or basename(worktree_path))
    local prefix = truncate_identifier(app_name .. "_" .. branch_name, 40)
    local env_path = worktree_path .. "/.env"
    local content = ""

    if fs.exists(env_path) then
        local existing = fs.read(env_path)
        if existing then
            content = existing:gsub("\n?" .. env_begin .. ".-" .. env_end .. "\n?", "\n")
        end
    end

    content = content:gsub("%s*$", "")
    if content ~= "" then
        content = content .. "\n\n"
    end

    content = content
        .. env_begin .. "\n"
        .. "RAILS_WORKTREE_DATABASE_PREFIX=" .. prefix .. "\n"
        .. env_end .. "\n"

    local written, write_err = fs.write(env_path, content)
    if written then
        db.worktrees:remove{ where = { path = worktree_path } }
        db.worktrees:insert{
            path = worktree_path,
            branch = branch,
            prefix = prefix,
            created_at = os.time(),
        }
        log.info("[rails-worktree-lifecycle] Wrote database env prefix " .. prefix .. " to " .. env_path)
    else
        log.warn("[rails-worktree-lifecycle] Could not write database env: " .. tostring(write_err))
    end

    local mise_local_path = worktree_path .. "/mise.local.toml"
    local mise_local = ""
    if fs.exists(mise_local_path) then
        local existing = fs.read(mise_local_path)
        if existing then
            mise_local = existing:gsub("\n?" .. mise_env_begin .. ".-" .. mise_env_end .. "\n?", "\n")
        end
    end

    mise_local = mise_local:gsub("%s*$", "")
    if mise_local ~= "" then
        mise_local = mise_local .. "\n\n"
    end

    mise_local = mise_local
        .. mise_env_begin .. "\n"
        .. "[env]\n"
        .. "RAILS_WORKTREE_DATABASE_PREFIX = " .. toml_quote(prefix) .. "\n"
        .. mise_env_end .. "\n"

    local mise_written, mise_write_err = fs.write(mise_local_path, mise_local)
    if mise_written then
        log.info("[rails-worktree-lifecycle] Wrote Mise database env prefix " .. prefix .. " to " .. mise_local_path)
    else
        log.warn("[rails-worktree-lifecycle] Could not write Mise database env: " .. tostring(mise_write_err))
    end

    return prefix
end

local function trust_mise(worktree_path)
    local trusted_any = false
    for _, filename in ipairs({ "mise.toml", "mise.local.toml" }) do
        local config_path = worktree_path .. "/" .. filename
        if fs.exists(config_path) then
            local command = "mise trust --yes --quiet " .. shell_quote(config_path) .. " >/dev/null 2>&1"
            local ok = os.execute(command)
            if ok == true or ok == 0 then
                trusted_any = true
                log.info("[rails-worktree-lifecycle] Trusted Mise config " .. config_path)
            else
                log.warn("[rails-worktree-lifecycle] mise trust failed for " .. config_path)
            end
        end
    end

    if not trusted_any then
        log.info("[rails-worktree-lifecycle] No mise.toml in " .. worktree_path)
    end
end

local function on_worktree_created(ctx)
    local repo_root = source_repo(ctx)
    if not repo_root or not ctx.path then return end

    for _, relative_path in ipairs(copied_files) do
        copy_file(repo_root, ctx.path, relative_path)
    end

    for _, relative_path in ipairs(copied_directories) do
        copy_directory(repo_root, ctx.path, relative_path)
    end

    write_database_env(repo_root, ctx.path, ctx.branch)
    trust_mise(ctx.path)
    prepare_node_dependencies(repo_root, ctx.path)
end

local function on_agent_created(agent)
    if not agent or not agent.in_worktree or not agent.worktree_path then return end

    local repo_root = (agent.metadata and agent.metadata.target_path) or agent.target_path
    if not repo_root then return end

    on_worktree_created{
        path = agent.worktree_path,
        branch = agent.branch_name,
        metadata = {
            target_path = repo_root,
        },
    }
end

local function drop_databases(prefix)
    local names = database_names(prefix)
    local list_command = "psql -Atqc "
        .. shell_quote("select datname from pg_database where datname like '" .. prefix .. "_test-%'")
        .. " postgres 2>/dev/null"
    local handle = io.popen(list_command)
    if handle then
        for name in handle:lines() do
            if name and name ~= "" then
                names[#names + 1] = name
            end
        end
        handle:close()
    end

    for _, name in ipairs(names) do
        local command = "dropdb --if-exists --force " .. shell_quote(name) .. " >/dev/null 2>&1"
        local ok = os.execute(command)
        if ok == true or ok == 0 then
            log.info("[rails-worktree-lifecycle] Dropped database if present: " .. name)
        else
            log.warn("[rails-worktree-lifecycle] dropdb failed for " .. name)
        end
    end
end

local function on_worktree_deleted(ctx)
    if not ctx.path then return end

    local rows = db.worktrees:get{ where = { path = ctx.path } }
    if not rows or not rows[1] then
        log.info("[rails-worktree-lifecycle] No database ledger entry for " .. ctx.path)
        return
    end

    drop_databases(rows[1].prefix)
    db.worktrees:remove{ where = { path = ctx.path } }
end

hooks.on("worktree_created", "rails_worktree_lifecycle.created", function(ctx)
    local ok, err = pcall(on_worktree_created, ctx)
    if not ok then
        log.warn("[rails-worktree-lifecycle] worktree_created error: " .. tostring(err))
    end
end)

hooks.on("after_agent_create", "rails_worktree_lifecycle.after_agent_create", function(agent)
    local ok, err = pcall(on_agent_created, agent)
    if not ok then
        log.warn("[rails-worktree-lifecycle] after_agent_create error: " .. tostring(err))
    end
end)

hooks.on("worktree_deleted", "rails_worktree_lifecycle.deleted", function(ctx)
    local ok, err = pcall(on_worktree_deleted, ctx)
    if not ok then
        log.warn("[rails-worktree-lifecycle] worktree_deleted error: " .. tostring(err))
    end
end)

log.info("[rails-worktree-lifecycle] Plugin loaded")

return {}
