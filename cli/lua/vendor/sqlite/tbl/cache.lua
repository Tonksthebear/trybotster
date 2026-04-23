-- botster: upstream had `require "sql.utils"` (typo — the module is
-- `sqlite.utils`). The file currently isn't exercised by any smoke/test
-- path, but the typo would trip any future caller. Also drop the `luv`
-- require; see VENDOR_CHANGES.md.
local u = require "sqlite.utils"

local Cache = {}

Cache.__index = Cache

local parse_query = (function()
  local concat = function(v)
    if not u.is_list(v) and type(v) ~= "string" then
      local tmp = {}
      for _k, _v in u.opairs(v) do
        if type(_v) == "table" then
          table.insert(tmp, string.format("%s=%s", _k, table.concat(_v, ",")))
        else
          table.insert(tmp, string.format("%s=%s", _k, _v))
        end
      end
      return table.concat(tmp, "")
    else
      return table.concat(v, "")
    end
  end

  return function(query)
    local items = {}
    for k, v in u.opairs(query) do
      if type(v) == "table" then
        table.insert(items, string.format("%s=%s", k, concat(v)))
      else
        table.insert(items, k .. "=" .. v)
      end
    end
    return table.concat(items, ",")
  end
end)()

-- assert(parse_query { where = { a = { 12, 99, 32 } } } == "where=a=12,99,32")
-- assert(parse_query { where = { a = 32 } } == "where=a=32")
-- print(vim.inspect(parse_query { keys = { "b", "c" }, where = { a = 1 } }))
-- "keys=bc,select=bc,where=a=1"
---Insert to cache using query definition.
---@param query table
---@param result table
function Cache:insert(query, result)
  self.store[parse_query(query)] = result
end

---Clear cache when succ is true, else skip.
function Cache:clear(succ)
  if succ then
    self.store = {}
    self.db.modified = false
  end
end

function Cache:is_empty()
  return next(self.store) == nil
end

---Get results from cache.
---@param query sqlite_query_select
---@return table
function Cache:get(query)
  -- botster: see helpers.lua for mtime-via-fs.stat rationale. With no mtime
  -- available, the `mtime ~= self.mtime` comparison collapses to nil-vs-nil
  -- (equal), so cache invalidation falls back to the `self.db.modified` flag
  -- alone — sufficient for single-writer dbs.
  local stat = type(fs) == "table" and fs.stat and fs.stat(self.db.uri) or nil
  local mtime = (type(stat) == "table" and stat.mtime) or nil

  if self.db.modified or mtime ~= self.mtime then
    self:clear(true)
    return
  end

  return self.store[parse_query(query)]
end

setmetatable(Cache, {
  __call = function(self, db)
    return setmetatable({ db = db, store = {} }, self)
  end,
})

return Cache
