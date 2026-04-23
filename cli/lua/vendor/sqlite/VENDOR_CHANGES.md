# sqlite.lua vendor changes

Upstream: https://github.com/kkharji/sqlite.lua
Pinned at commit: `50092d60feb242602d7578398c6eb53b4a8ffe7b` (2024-ish, "Use HOMEBREW_PREFIX in defs.lua").
Vendored into: `cli/lua/vendor/sqlite/`.

All files were copied verbatim from `lua/sqlite/` in the upstream tree. The
upstream `LICENSE` (MIT) is reproduced here alongside this file.

The only diffs are the ones documented below — patches to break the `luv`
(libuv binding) dependency and fix one upstream typo. `luv` is a `vim`/neovim
runtime artifact and is not present in a bare LuaJIT host; our Lua runtime
has LuaJIT's FFI enabled but no `luv` binding.

## Patches

### `defs.lua`

Upstream: uses `require "luv"` to get the `LIBSQLITE` env var, `os_uname`, and
`HOMEBREW_PREFIX`.

- Line 3 (upstream): `local luv = require "luv"` — **removed.**
- Lines 10–15 (upstream): `if vim then ... vim.g.sqlite_clib_path ... end`
  block — **removed** (vim globals are not relevant outside neovim).
- Line 18 (upstream): `path, _ = luv.os_getenv "LIBSQLITE"` — replaced with
  `os.getenv("BOTSTER_LIBSQLITE") or os.getenv("LIBSQLITE")`. `BOTSTER_LIBSQLITE`
  takes precedence so botster deployments can pin a specific library without
  clobbering any caller's own `LIBSQLITE`.
- Line 23 (upstream): `local os = luv.os_uname()` — replaced with `jit.os`
  and `jit.arch`. The `if os.sysname == "Darwin"` branch is rewritten to
  `if jit_os == "OSX"` because LuaJIT returns `"OSX"` whereas `uname` returns
  `"Darwin"`. Added aarch64 paths to the Linux candidate list so ARM64 Linux
  runners pick up `libsqlite3` without `BOTSTER_LIBSQLITE`.
- Line 51 (upstream): `luv.os_getenv "HOMEBREW_PREFIX"` → `os.getenv("HOMEBREW_PREFIX")`.
- Line 52 (upstream): `os.machine == "arm64"` → `jit_arch == "arm64"` (the
  upstream variable name `os` was a local shadow of the global; our rewrite
  uses separate `jit_os` and `jit_arch` names).

### `helpers.lua`

- Line 2 (upstream): `local luv = require "luv"` — **removed.**
- Line 76 (upstream): `o.mtime = ... luv.fs_stat(o.db.uri) ... .mtime.sec ...`
  — replaced with a defensive lookup against our `fs.stat` global primitive.
  Our `fs.stat` currently returns `{exists, type, size}` without an `mtime`
  field, so `o.mtime` will be `nil` in practice; that's acceptable because
  the mtime is only used for cache invalidation in `tbl/cache.lua`, which
  plugin.db() does not exercise today (single-writer per db).

### `utils.lua`

- Line 1 (upstream): `local luv = require "luv"` — **removed.**
- Line 106 (upstream): `expanded = luv.fs_realpath(path)` — replaced with
  `expanded = path`. Upstream used `realpath()` to canonicalize a leading-dot
  relative path. plugin.db always passes absolute paths, so canonicalization
  is cosmetic; returning the path unchanged is safe for our use case. If a
  future caller needs canonicalization, add `fs.realpath` to
  `cli/src/lua/primitives/fs.rs` and thread it through here.

### `tbl/cache.lua`

- Line 1 (upstream): `local u = require "sql.utils"` — **fixed to
  `require "sqlite.utils"`.** This is a pre-existing upstream typo (the
  module is `sqlite.utils`, not `sql.utils`). The file isn't on any current
  happy path in botster, but the typo would trip any future caller; fixing
  it inline avoids a confusing surprise later.
- Line 2 (upstream): `local luv = require "luv"` — **removed.**
- Line 65 (upstream): `local stat = luv.fs_stat(self.db.uri)` — same
  `fs.stat`-based fallback as `helpers.lua`.

## Upgrading upstream

When bumping the pin:
1. Fetch the new tag/commit from https://github.com/kkharji/sqlite.lua.
2. Copy the 13 `.lua` files verbatim, plus `LICENSE`.
3. Re-apply these patches. If upstream reshuffles `defs.lua` meaningfully,
   read the diff carefully — the clib-loading block is the fragile bit.
4. Update the pin at the top of this file.
5. Run `cargo test -p botster -- sqlite_lua_smoke` to confirm the smoke
   test still passes.
