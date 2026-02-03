-- Module loader with hot-reload support
local M = {}

-- Track which modules should never be reloaded
local protected_modules = {
    ["core.state"] = true,
    ["core.hooks"] = true,
    ["core.loader"] = true,
}

-- Reload a module by path
function M.reload(module_name)
    if protected_modules[module_name] then
        log.warn(string.format("Cannot reload protected module: %s", module_name))
        return false
    end

    -- Get the module if already loaded
    local old_module = package.loaded[module_name]

    -- Call _before_reload if it exists
    if old_module and type(old_module) == "table" and old_module._before_reload then
        local ok, err = pcall(old_module._before_reload)
        if not ok then
            log.warn(string.format("_before_reload failed for %s: %s", module_name, tostring(err)))
        end
    end

    -- Unload the module
    package.loaded[module_name] = nil

    -- Reload it
    local ok, result = pcall(require, module_name)
    if not ok then
        log.error(string.format("Failed to reload %s: %s", module_name, tostring(result)))
        -- Restore old module on failure
        package.loaded[module_name] = old_module
        return false
    end

    -- Call _after_reload if it exists
    local new_module = package.loaded[module_name]
    if new_module and type(new_module) == "table" and new_module._after_reload then
        local ok2, err = pcall(new_module._after_reload)
        if not ok2 then
            log.warn(string.format("_after_reload failed for %s: %s", module_name, tostring(err)))
        end
    end

    log.info(string.format("Reloaded module: %s", module_name))
    return true
end

-- Mark a module as protected (cannot be reloaded)
function M.protect(module_name)
    protected_modules[module_name] = true
end

-- Check if a module is protected
function M.is_protected(module_name)
    return protected_modules[module_name] == true
end

return M
