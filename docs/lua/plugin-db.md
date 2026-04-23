# `plugin.db{}` — per-plugin persistence

`plugin.db{}` gives Lua plugins a private sqlite database with a declarative
schema and a migration runner. One call at plugin load time returns a handle
that survives hot-reload and closes cleanly on hub shutdown.

The underlying library is [vendored sqlite.lua](../../cli/lua/vendor/sqlite/)
(bundled in PR B.1); `plugin.db` is the Botster-specific wrapper around it.

## Shape

```lua
-- plugins/messaging/init.lua
local db = plugin.db{
  version = 1,
  models = {
    messages = {
      id         = true,                              -- INTEGER PRIMARY KEY
      channel_id = { 'integer', required = true },
      author     = { 'text',    required = true },
      body       = { 'text',    required = true },
      created_at = { 'integer', required = true },
    },
    channels = {
      id       = true,
      name     = { 'text',    required = true, unique = true },
      archived = { 'integer', default = 0 },
    },
  },
}

-- CRUD
db.messages:insert{
  channel_id = 1,
  author = 'jason',
  body = 'hi',
  created_at = os.time(),
}
local rows = db.messages:get{ where = { channel_id = 1 } }
db.messages:update{ where = { id = 1 }, set = { body = 'edited' } }
db.messages:remove{ where = { id = 1 } }

-- Multi-statement transaction: BEGIN on entry, COMMIT on clean return,
-- ROLLBACK on any raised error. Nested calls to `db.t:insert`/etc. run
-- inside the same transaction.
db:execute(function(db)
  db.channels:insert{ name = 'general' }
  db.messages:insert{ channel_id = 1, author = 'bot',
                      body = 'welcome', created_at = os.time() }
end)

-- Escape hatch: raw SQL with `?` placeholders
local rows = db:eval('SELECT COUNT(*) AS n FROM messages WHERE author = ?',
                     'jason')
```

## File layout

`plugin.db` derives `{config.data_dir()}/plugins/<plugin_name>/db.sqlite`
and creates the parent directory if missing. `<plugin_name>` is whatever
the hub registered for the loading plugin (matches the plugin's directory
name). The file is NOT deleted when the plugin is disabled or unloaded —
only the in-memory handle is.

## Default PRAGMAs

Every open applies, in order:

| PRAGMA          | Value    | Why                                   |
|-----------------|----------|---------------------------------------|
| `journal_mode`  | `WAL`    | Concurrent readers + single writer    |
| `synchronous`   | `NORMAL` | Safe under WAL; faster than `FULL`    |
| `foreign_keys`  | `ON`     | Enforce `reference = '<tbl>.<col>'`   |
| `busy_timeout`  | `5000`   | Wait 5s on write contention           |

You can override or add to these via `pragmas = { ... }` on the spec:

```lua
plugin.db{
  version = 1,
  models = { ... },
  pragmas = { cache_size = -20000 },  -- 20 MB page cache
}
```

## Schema DSL

Each field in `models.<table>` accepts one of:

| Form                                  | Meaning                                  |
|---------------------------------------|------------------------------------------|
| `true`                                | `INTEGER PRIMARY KEY`                    |
| `'text'` / `'integer'` / `'real'` / `'blob'` | bare column of that type          |
| `{ 'text', required = true, ... }`    | typed column with options                |
| `{ type = 'text', required = true, ... }` | same as positional type             |

Options (in addition to positional or named `type`):

| Key          | Meaning                                                        |
|--------------|----------------------------------------------------------------|
| `required`   | `NOT NULL`                                                     |
| `primary`    | `PRIMARY KEY`                                                  |
| `unique`     | `UNIQUE`                                                       |
| `default`    | default value (strings are quoted; booleans map to 0/1)        |
| `reference`  | `"other_table.col"` — foreign key                              |
| `on_delete`  | `'cascade'` \| `'null'` \| `'default'` \| `'restrict'`          |
| `on_update`  | same values as `on_delete`                                     |

### No declarative indexes

The v1 DSL does not include indexes. If you need one, create it in a
migration function (or at plugin top-level, since `IF NOT EXISTS` makes the
statement idempotent):

```lua
db:eval('CREATE INDEX IF NOT EXISTS messages_channel_idx ON messages (channel_id)')
```

## Migrations

`version` defaults to `1`. On load, `plugin.db` reads `PRAGMA user_version`:

- **`current < version`** — run each step from `current+1 .. version` inside
  its own BEGIN/COMMIT. Each step applies **additive** schema changes
  implied by `models` (new tables, new columns), invokes `migrations[i]` if
  provided, then writes `PRAGMA user_version = i`. A single failure rolls
  that step back and refuses plugin load — the database stays at the
  previous version.
- **`current == version`** — reconcile the live schema against `models`
  and apply any additive changes (new tables, new columns with a default
  or nullable). Any non-additive drift (removed column, type change,
  nullability change) refuses plugin load with a printable diff.
- **`current > version`** — refuse plugin load. Downgrades are not
  supported; bump the declared `version` to match or delete the db file to
  reset.

### Example migration

```lua
plugin.db{
  version = 3,
  models = {
    messages = {
      id = true,
      author = { 'text', required = true, default = '' },
      body = { 'text', required = true, default = '' },
      reactions = { 'text', default = '[]' },  -- new in v3
    },
  },
  migrations = {
    [2] = function(db)
      -- normalise existing author values
      db:eval('UPDATE messages SET author = lower(author)')
    end,
    [3] = function(db)
      -- backfill the new column for existing rows
      db:eval("UPDATE messages SET reactions = '[]' WHERE reactions IS NULL")
    end,
  },
}
```

Each migration function receives the raw sqlite db; prefer `db:eval(...)`
for individual statements. **Do not call `db:execute(function() end)`
inside a migration** — the step is already inside a transaction and sqlite
rejects nested `BEGIN`.

### Adding a required column

Sqlite refuses `ALTER TABLE ... ADD COLUMN <col> NOT NULL` without a
`DEFAULT`. `plugin.db` surfaces this as a clear error with two fixes:

```lua
-- Option A: give it a default
author = { 'text', required = true, default = 'anonymous' }

-- Option B: make it optional and backfill in a migration step
models = { messages = { author = { 'text' } } },
migrations = {
  [2] = function(db) db:eval("UPDATE messages SET author = 'anonymous' WHERE author IS NULL") end,
}
```

## Lifecycle

The handle cache is keyed by plugin name, so a hot-reload that calls
`plugin.db{}` again returns the **same** sqlite connection — data is
preserved, no fds leak. When the plugin is disabled, unloaded, or the hub
shuts down, the handle closes. Tests that need an ephemeral database pass
`memory = true`:

```lua
local db = plugin.db{ memory = true, version = 1, models = { ... } }
```

`memory = true` creates a `:memory:` sqlite database per plugin process.
Data is discarded on hub exit. Re-declaring a plugin with a different
`memory` flag than its cached handle raises an error; pick one.

## Constraints

- `plugin.db{}` must run **during plugin load**. Call it at the top of
  `init.lua` and capture the handle in a local; references from callbacks,
  hooks, and MCP tool handlers use the captured value.
- All plugin code runs on the hub's main thread. No locking is needed
  because concurrent access is not possible from Lua.
- `plugin.db` does not currently migrate data across a plugin rename.
  If you rename the plugin directory, copy the old file manually:
  `~/.botster/plugins/<old>/db.sqlite` → `~/.botster/plugins/<new>/db.sqlite`.

## Related

- [`cli/lua/vendor/sqlite/`](../../cli/lua/vendor/sqlite/) — vendored
  upstream library (see its `VENDOR_CHANGES.md` for local patches).
- [`cli/lua/lib/plugin_db.lua`](../../cli/lua/lib/plugin_db.lua) —
  wrapper implementation.
- [`cli/tests/plugin_db_test.rs`](../../cli/tests/plugin_db_test.rs) —
  integration tests covering the full API surface.
