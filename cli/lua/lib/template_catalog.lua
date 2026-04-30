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

function M.local_source_root()
    if config and type(config.template_catalog_path) == "function" then
        local ok, root = pcall(config.template_catalog_path)
        if ok and type(root) == "string" and root ~= "" then return root end
    end
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

local function trim_trailing_slash(value)
    return tostring(value or ""):gsub("/+$", "")
end

local function config_get(key)
    if not (config and type(config.get) == "function") then return nil end
    local ok, value = pcall(config.get, key)
    if ok and type(value) == "string" and value ~= "" then return value end
    return nil
end

local function config_env(key)
    if config and type(config.env) == "function" then
        local ok, value = pcall(config.env, key)
        if ok and type(value) == "string" and value ~= "" then return value end
    end
    local value = os.getenv(key)
    if type(value) == "string" and value ~= "" then return value end
    return nil
end

local function catalog_ref()
    return config_env("BOTSTER_TEMPLATE_CATALOG_REF")
        or config_get("template_catalog_ref")
        or "main"
end

function M.default_remote_url()
    local override = config_env("BOTSTER_TEMPLATE_CATALOG_URL")
        or config_get("template_catalog_url")
    if override then return override end

    local ref = catalog_ref()
    return "https://api.github.com/repos/Tonksthebear/trybotster/contents/catalog/templates?ref=" .. ref
end

function M.cache_path()
    local root = config and type(config.data_dir) == "function" and config.data_dir() or nil
    if not root or root == "" then return nil end
    return trim_trailing_slash(root) .. "/cache/template_catalog.json"
end

local function write_cache(templates, source)
    local path = M.cache_path()
    if not path or not json or type(json.encode) ~= "function" then return false end
    local ok, payload = pcall(json.encode, {
        version = 1,
        fetched_at = os.time(),
        source = source,
        templates = templates,
    })
    if not ok or type(payload) ~= "string" then return false end
    local written, err = fs.write(path, payload)
    if not written then
        log.warn(string.format("template_catalog: failed to write cache %s: %s", path, tostring(err)))
        return false
    end
    return true
end

local function read_cache()
    local path = M.cache_path()
    if not path or not fs.exists(path) then return nil end
    local content, read_err = fs.read(path)
    if not content then
        log.warn(string.format("template_catalog: failed to read cache %s: %s", path, tostring(read_err)))
        return nil
    end
    local ok, payload = pcall(json.decode, content)
    if not ok or type(payload) ~= "table" or type(payload.templates) ~= "table" then return nil end
    return payload.templates, payload
end

local function template_id_set(templates)
    local ids = {}
    for _, template in ipairs(templates or {}) do
        if type(template) == "table" and type(template.id) == "string" and template.id ~= "" then
            ids[template.id] = true
        end
    end
    return ids
end

local function sorted_stale_template_ids(previous_templates, refreshed_templates)
    local refreshed_ids = template_id_set(refreshed_templates)
    local stale_ids = {}
    local seen_stale = {}
    for _, template in ipairs(previous_templates or {}) do
        local id = type(template) == "table" and template.id or nil
        if type(id) == "string" and id ~= "" and not refreshed_ids[id] and not seen_stale[id] then
            seen_stale[id] = true
            stale_ids[#stale_ids + 1] = id
        end
    end
    table.sort(stale_ids)
    return stale_ids
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
        source = "local",
    }
end

local function template_from_content(relative, content, source)
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
        source = source or "github",
    }
end

local function sort_templates(out)
    table.sort(out, function(a, b)
        if a.category == b.category then return a.dest < b.dest end
        return a.category < b.category
    end)
    return out
end

local function list_local(root)
    if not root or root == "" then return {} end
    if not fs.exists(root) or not fs.is_dir(root) then return {} end

    local out = {}
    for _, relative in ipairs(list_files_recursive(root)) do
        local template = template_from_file(root, relative)
        if template then out[#out + 1] = template end
    end

    return sort_templates(out)
end

function M.list(opts)
    opts = opts or {}
    if opts.source_root then return list_local(opts.source_root) end

    local cached = read_cache()
    if cached then return sort_templates(cached) end

    return list_local(M.local_source_root())
end

local function http_get(url)
    if not (http and type(http.get) == "function") then return nil, "http.get unavailable" end
    local resp, err = http.get(url, { headers = {
        ["Accept"] = "application/vnd.github+json",
        ["User-Agent"] = "botster-template-catalog",
    } })
    if not resp then return nil, err or "HTTP request failed" end
    if tonumber(resp.status) < 200 or tonumber(resp.status) >= 300 then
        return nil, string.format("HTTP %s from %s", tostring(resp.status), tostring(url))
    end
    return resp.body
end

local function strip_catalog_prefix(path)
    return tostring(path or ""):gsub("^catalog/templates/", "")
end

local fetch_github_directory

local function fetch_github_entries(entries, out, visited)
    for _, entry in ipairs(entries) do
        if entry.type == "dir" and entry.url then
            local dir_ok, dir_err = fetch_github_directory(entry.url, out, visited)
            if not dir_ok then return nil, dir_err end
        elseif entry.type == "file" and entry.download_url and is_template_file(entry.path or entry.name or "") then
            local content, file_err = http_get(entry.download_url)
            if not content then return nil, file_err end
            local relative = strip_catalog_prefix(entry.path or entry.name)
            local template = template_from_content(relative, content, "github")
            if template then
                template.source_url = entry.html_url or entry.download_url
                template.catalog_ref = catalog_ref()
                out[#out + 1] = template
            end
        end
    end
    return true
end

fetch_github_directory = function(url, out, visited)
    visited = visited or {}
    if visited[url] then return true end
    visited[url] = true

    local body, err = http_get(url)
    if not body then return nil, err end
    local ok, entries = pcall(json.decode, body)
    if not ok or type(entries) ~= "table" then return nil, "Invalid GitHub catalog JSON" end

    return fetch_github_entries(entries, out, visited)
end

function M.refresh(opts)
    opts = opts or {}
    local url = opts.url or M.default_remote_url()
    local out = {}
    local ok, err = fetch_github_directory(url, out)
    if not ok then return nil, err end
    sort_templates(out)
    write_cache(out, url)
    return out
end

local refresh_in_flight = false

function M.refresh_async(opts)
    opts = opts or {}
    if refresh_in_flight then return false, "refresh already in flight" end
    if not (http and type(http.request) == "function") then return false, "http.request unavailable" end

    refresh_in_flight = true
    local url = opts.url or M.default_remote_url()
    local templates = {}
    local previous_templates = read_cache() or {}
    local visited = {}
    local pending = 0
    local failed = false
    local completed = false
    local dispatching = 0

    local function finish_if_done()
        if pending > 0 or dispatching > 0 or failed or completed then return end
        completed = true
        refresh_in_flight = false
        sort_templates(templates)
        write_cache(templates, url)

        local EntityModel = require("lib.entity_model")
        for _, template in ipairs(templates) do EntityModel.upsert_template(template) end
        for _, id in ipairs(sorted_stale_template_ids(previous_templates, templates)) do
            EntityModel.remove_template(id)
        end
    end

    local function fail(message)
        if failed then return end
        failed = true
        refresh_in_flight = false
        log.warn(string.format("template_catalog: remote refresh failed: %s", tostring(message)))
    end

    local function request_json(request_url, callback)
        pending = pending + 1
        http.request({
            method = "GET",
            url = request_url,
            headers = {
                ["Accept"] = "application/vnd.github+json",
                ["User-Agent"] = "botster-template-catalog",
            },
        }, function(resp, err)
            pending = pending - 1
            if failed then return end
            if err or not resp or tonumber(resp.status) < 200 or tonumber(resp.status) >= 300 then
                fail(err or (resp and resp.status) or "request failed")
                return
            end
            dispatching = dispatching + 1
            callback(resp.body or "")
            dispatching = dispatching - 1
            finish_if_done()
        end)
    end

    local request_directory

    local function request_template_file(entry)
        pending = pending + 1
        http.request({
            method = "GET",
            url = entry.download_url,
            headers = {
                ["Accept"] = "text/plain",
                ["User-Agent"] = "botster-template-catalog",
            },
        }, function(resp, err)
            pending = pending - 1
            if failed then return end
            if err or not resp or tonumber(resp.status) < 200 or tonumber(resp.status) >= 300 then
                fail(err or (resp and resp.status) or "template file request failed")
                return
            end
            local relative = strip_catalog_prefix(entry.path or entry.name)
            local template = template_from_content(relative, resp.body or "", "github")
            if template then
                template.source_url = entry.html_url or entry.download_url
                template.catalog_ref = catalog_ref()
                templates[#templates + 1] = template
            end
            finish_if_done()
        end)
    end

    local function handle_entries(body)
        local ok, entries = pcall(json.decode, body or "")
        if not ok or type(entries) ~= "table" then
            fail("Invalid GitHub catalog JSON")
            return
        end

        for _, entry in ipairs(entries) do
            if entry.type == "dir" and entry.url then
                request_directory(entry.url)
            elseif entry.type == "file" and entry.download_url and is_template_file(entry.path or entry.name or "") then
                request_template_file(entry)
            end
        end
    end

    request_directory = function(request_url)
        if visited[request_url] then return end
        visited[request_url] = true
        request_json(request_url, handle_entries)
    end

    request_directory(url)

    return true
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
