-- Helpers for explicit spawn-target identity threading.
--
-- Botster is moving from ambient repo/cwd assumptions to explicit spawn targets.
-- This module keeps the target_id / target_path / target_repo rules in one place
-- so command handlers, spawn logic, and manifest persistence stay consistent.

local M = {}

local function copy_table(src)
    local out = {}
    if src then
        for k, v in pairs(src) do
            out[k] = v
        end
    end
    return out
end

local function normalize_string(value)
    if value == nil then return nil end
    local normalized = tostring(value):match("^%s*(.-)%s*$")
    if normalized == "" then return nil end
    return normalized
end

local function path_basename(path)
    local normalized = normalize_string(path)
    if not normalized then return nil end
    local trimmed = normalized:gsub("/+$", "")
    return trimmed:match("([^/]+)$") or trimmed
end

function M.copy_table(src)
    return copy_table(src)
end

function M.default_repo_label(target)
    if target and target.target_repo then
        return target.target_repo
    end
    if target and target.target_path then
        return path_basename(target.target_path) or "local-target"
    end
    if target and target.target_id then
        return target.target_id
    end
    return "local-target"
end

local function registry()
    local global_registry = rawget(_G, "spawn_targets")
    if type(global_registry) == "table" then
        return global_registry
    end
    return nil
end

local function lookup_admitted_target(target_id)
    local target_registry = registry()
    if not target_id then
        return nil, nil
    end
    if not target_registry or type(target_registry.get) ~= "function" then
        return nil, "spawn target registry is unavailable"
    end

    local ok, admitted = pcall(target_registry.get, target_id)
    if not ok then
        return nil, string.format("failed to resolve target_id %s: %s", target_id, tostring(admitted))
    end
    if admitted == nil then
        return nil, string.format("unknown target_id: %s", target_id)
    end
    if admitted.enabled == false then
        return nil, string.format("spawn target %s is disabled", target_id)
    end
    return admitted, nil
end

local function inspect_target(path)
    local target_registry = registry()
    if not path or not target_registry or type(target_registry.inspect) ~= "function" then
        return nil
    end

    local ok, inspection = pcall(target_registry.inspect, path)
    if not ok then
        return nil
    end
    return inspection
end

local function normalize_repo(repo)
    return normalize_string(repo)
end

function M.resolve(opts)
    opts = opts or {}

    local explicit = opts.explicit or {}
    local command = opts.command or {}
    local metadata = opts.metadata or {}
    local explicit_target_path = normalize_string(explicit.target_path or command.target_path or metadata.target_path)
    local explicit_target_repo = normalize_string(
        explicit.target_repo or command.target_repo or metadata.target_repo
            or command.repo or metadata.repo or opts.default_target_repo
    )

    local target = {
        target_id = normalize_string(
            explicit.target_id or command.target_id or metadata.target_id
        ),
        target_path = explicit_target_path,
        target_repo = explicit_target_repo,
    }

    local admitted_target = nil
    if target.target_id then
        local target_err = nil
        admitted_target, target_err = lookup_admitted_target(target.target_id)
        if not admitted_target then
            return nil, target_err
        end
        if target.target_path and admitted_target.path ~= target.target_path then
            return nil, string.format("target_path does not match admitted target %s", target.target_id)
        end
        target.target_path = admitted_target.path
    elseif explicit_target_path or explicit_target_repo then
        return nil, "target_id is required for explicit target selection"
    end

    if not target.target_repo and target.target_path then
        local inspection = inspect_target(target.target_path)
        if inspection and inspection.repo_name then
            target.target_repo = normalize_string(inspection.repo_name)
        end
    end

    if opts.require_target_id and not target.target_id then
        return nil, "target_id is required"
    end
    if opts.require_target_path and not target.target_path then
        return nil, "target_path is required"
    end

    target.repo = M.default_repo_label(target)
    return target
end

function M.find_by_repo(repo)
    local normalized_repo = normalize_repo(repo)
    if not normalized_repo then
        return nil, "target_repo is required"
    end

    local target_registry = registry()
    if not target_registry or type(target_registry.list) ~= "function" then
        return nil, "spawn target registry is unavailable"
    end

    local ok, admitted_targets = pcall(target_registry.list)
    if not ok then
        return nil, string.format("failed to list admitted spawn targets: %s", tostring(admitted_targets))
    end

    local matches = {}
    for _, admitted in ipairs(admitted_targets or {}) do
        if admitted.enabled ~= false then
            local inspection = inspect_target(admitted.path)
            local repo_name = inspection and normalize_repo(inspection.repo_name) or nil
            if repo_name == normalized_repo then
                matches[#matches + 1] = {
                    target_id = admitted.id,
                    target_path = admitted.path,
                    target_repo = repo_name,
                    repo = repo_name,
                }
            end
        end
    end

    if #matches == 0 then
        return nil, string.format("no admitted spawn target matches repo %s", normalized_repo)
    end
    if #matches > 1 then
        return nil, string.format("multiple admitted spawn targets match repo %s", normalized_repo)
    end
    return matches[1], nil
end

function M.with_metadata(metadata, target)
    local out = copy_table(metadata)
    if not target then return out end

    if target.target_id then out.target_id = target.target_id end
    if target.target_path then out.target_path = target.target_path end
    if target.target_repo then out.target_repo = target.target_repo end
    return out
end

function M.from_session(session)
    local metadata = session and session.metadata or {}
    local target = {
        target_id = normalize_string((session and session.target_id) or metadata.target_id),
        target_path = normalize_string(
            (session and session.target_path) or metadata.target_path
                or (session and session.worktree_path)
        ),
        target_repo = normalize_string(
            (session and session.target_repo) or metadata.target_repo
                or (session and session.repo) or metadata.repo
        ),
    }
    target.repo = M.default_repo_label(target)
    return target
end

function M.from_manifest(manifest)
    local metadata = manifest and manifest.metadata or {}
    local target = {
        target_id = normalize_string((manifest and manifest.target_id) or metadata.target_id),
        target_path = normalize_string(
            (manifest and manifest.target_path) or metadata.target_path or (manifest and manifest.worktree_path)
        ),
        target_repo = normalize_string(
            (manifest and manifest.target_repo) or metadata.target_repo or (manifest and manifest.repo) or metadata.repo
        ),
    }
    target.repo = M.default_repo_label(target)
    return target
end

function M.matches(candidate, target)
    local left = M.from_manifest(candidate or {})
    local right = M.from_manifest(target or {})

    if right.target_id then
        if left.target_id then
            return left.target_id == right.target_id
        end
        if right.target_path and left.target_path then
            return left.target_path == right.target_path
        end
        if right.target_repo and left.target_repo then
            return left.target_repo == right.target_repo
        end
        return false
    end
    if right.target_path and left.target_path then
        return left.target_path == right.target_path
    end
    if right.target_repo and left.target_repo then
        return left.target_repo == right.target_repo
    end
    return not right.target_id and not right.target_path and not right.target_repo
end

return M
