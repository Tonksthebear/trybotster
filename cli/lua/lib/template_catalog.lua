-- Hub-owned template catalog discovery.
--
-- The hub is the source of truth for template catalog presentation. Rails and
-- browser clients consume the `template` entity snapshot instead of scanning
-- catalog files themselves.

local M = {}

local TEMPLATE_PATTERN = {
    [".lua"] = true,
    [".sh"] = true,
    [".md"] = true,
}

local function copy_table(value)
    local out = {}
    for k, v in pairs(value or {}) do out[k] = v end
    return out
end

local function runtime_repo_root()
    if worktree and type(worktree.repo_root) == "function" then
        local ok, root = pcall(worktree.repo_root)
        if ok and type(root) == "string" and root ~= "" then return root end
    end
    return nil
end

function M.bundled_source_root()
    if config and type(config.template_catalog_path) == "function" then
        local ok, root = pcall(config.template_catalog_path)
        if ok and type(root) == "string" and root ~= "" then return root end
    end

    local root = runtime_repo_root()
    if root then return root .. "/catalog/templates" end
    return nil
end

local function extname(path)
    return path:match("(%.[^./]+)$") or ""
end

local function basename_without_ext(relative)
    local ext = extname(relative)
    local base = ext ~= "" and relative:sub(1, #relative - #ext) or relative
    return (base:gsub("/", "-"))
end

local function is_template_file(relative)
    return TEMPLATE_PATTERN[extname(relative)] == true
end

local function list_files_recursive(root, rel, out)
    rel = rel or ""
    out = out or {}
    local path = rel == "" and root or (root .. "/" .. rel)
    local entries = fs.listdir(path) or {}
    table.sort(entries)

    for _, entry in ipairs(entries) do
        local entry_rel = rel == "" and entry or (rel .. "/" .. entry)
        local entry_path = root .. "/" .. entry_rel
        if fs.is_dir(entry_path) then
            list_files_recursive(root, entry_rel, out)
        elseif is_template_file(entry_rel) then
            out[#out + 1] = entry_rel
        end
    end

    return out
end

local function extract_template_metadata(content)
    local metadata = {}
    local normalized = tostring(content or "") .. "\n"
    for line in normalized:gmatch("([^\n]*)\n") do
        if line:sub(1, 2) == "#!" then
            -- Shebangs are not metadata, but they also are not the first body
            -- line for shell templates.
        elseif line:match("^%s*$") then
            -- Ignore leading blank comment-header spacing.
        elseif line:sub(1, 2) == "--" or line:sub(1, 1) == "#" or line:sub(1, 4) == "<!--" then
            local key, value = line:match("^%-%-%s*@(%w+)%s+(.+)")
            if not key then key, value = line:match("^#%s*@(%w+)%s+(.+)") end
            if not key then key, value = line:match("^<!%-%-%s*@(%w+)%s+(.+)%s*%-%->") end
            if key and value then metadata[key] = value:gsub("%s+$", "") end
        else
            break
        end
    end
    return metadata
end

local function template_from_file(root, relative)
    local path = root .. "/" .. relative
    local content, read_err = fs.read(path)
    if not content then
        log.warn(string.format("template_catalog: failed to read %s: %s", path, tostring(read_err)))
        return nil
    end

    local meta = extract_template_metadata(content)
    if not (meta.template and meta.category and meta.dest) then return nil end

    return {
        id = meta.category .. "-" .. basename_without_ext(relative),
        slug = meta.category .. "-" .. basename_without_ext(relative),
        name = meta.template,
        description = meta.description,
        category = meta.category,
        dest = meta.dest,
        scope = meta.scope,
        version = meta.version or "1.0.0",
        content = content,
        source = "bundled",
    }
end

function M.list(opts)
    opts = opts or {}
    local root = opts.source_root or M.bundled_source_root()
    if not root or root == "" then return {} end
    if not fs.exists(root) or not fs.is_dir(root) then return {} end

    local out = {}
    for _, relative in ipairs(list_files_recursive(root)) do
        local template = template_from_file(root, relative)
        if template then out[#out + 1] = template end
    end

    table.sort(out, function(a, b)
        if a.category == b.category then return a.dest < b.dest end
        return a.category < b.category
    end)
    return out
end

function M.group_by_category(templates)
    local grouped = {}
    for _, template in ipairs(templates or {}) do
        local category = template.category
        if category then
            grouped[category] = grouped[category] or {}
            grouped[category][#grouped[category] + 1] = copy_table(template)
        end
    end
    return grouped
end

return M
