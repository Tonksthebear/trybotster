//! End-to-end smoke test for the vendored `sqlite.lua` binding.
//!
//! Verifies the PR B.1 acceptance criteria:
//!   1. LuaJIT FFI is loadable on the main Lua state.
//!   2. `require("vendor.sqlite")` resolves to our vendored `init.lua`.
//!   3. The luv-removal patches compile cleanly.
//!   4. An in-memory sqlite database round-trips basic CRUD.
//!
//! This test does NOT exercise the `plugin.db{}` wrapper — that's PR B.2.
//! It deliberately bypasses `LuaRuntime::new()` so the test stays focused on
//! the vendored library and doesn't drag in every hub primitive.

#![expect(clippy::unwrap_used, reason = "test-code brevity")]

use std::path::PathBuf;

use mlua::{Lua, LuaOptions, StdLib};

/// Build a bare LuaJIT VM with FFI loaded and `package.path` configured so
/// `require("vendor.sqlite")` resolves against `cli/lua/`.
///
/// This mirrors what `LuaRuntime::new()` does for the vendor search paths
/// (see `setup_package_path`), minus all the primitive registrations the
/// smoke test doesn't need.
///
/// SAFETY: `Lua::new()` marks the VM safe-mode and rejects `StdLib::FFI`.
/// Production uses `Lua::unsafe_new_with(ALL_SAFE | FFI, ...)` — unsafe-marked
/// VM with safe stdlibs plus FFI, excluding DEBUG. This test mirrors that
/// configuration exactly so the vendored FFI path is exercised as-in-prod.
fn new_vendor_lua() -> Lua {
    let lua =
        unsafe { Lua::unsafe_new_with(StdLib::ALL_SAFE | StdLib::FFI, LuaOptions::default()) };

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let lua_base = manifest_dir.join("lua");
    let base = lua_base.to_string_lossy();

    // Mirror `LuaRuntime::setup_package_path`: vendor paths let
    // `require("sqlite.defs")` (upstream internal) and
    // `require("vendor.sqlite")` (public entry) both resolve against
    // `cli/lua/vendor/sqlite/`.
    let package: mlua::Table = lua.globals().get("package").unwrap();
    let current_path: String = package.get("path").unwrap();
    let new_path = format!(
        "{base}/?.lua;{base}/?/init.lua;{base}/vendor/?.lua;{base}/vendor/?/init.lua;{current_path}"
    );
    package.set("path", new_path).unwrap();

    lua
}

#[test]
fn ffi_stdlib_is_loaded() {
    let lua = new_vendor_lua();
    let loaded: bool = lua
        .load(r#"local ffi = require('ffi'); return ffi ~= nil and type(ffi.load) == 'function'"#)
        .eval()
        .expect("ffi should be requireable");
    assert!(loaded, "FFI stdlib should expose ffi.load");
}

#[test]
fn vendor_sqlite_module_resolves() {
    let lua = new_vendor_lua();
    let resolves: bool = lua
        .load(r#"local m = require('vendor.sqlite'); return type(m) == 'table'"#)
        .eval()
        .expect("vendor.sqlite should load");
    assert!(resolves, "require('vendor.sqlite') must return a table");
}

#[test]
fn vendored_sqlite_lua_round_trips_in_memory_db() {
    let lua = new_vendor_lua();
    let ok: bool = lua
        .load(
            r#"
            local sqlite = require('vendor.sqlite')
            local db = sqlite.new(':memory:')
            db:open()
            db:eval('CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)')
            db:eval("INSERT INTO t (name) VALUES ('alice'), ('bob')")
            local rows = db:eval('SELECT * FROM t ORDER BY id')
            assert(type(rows) == 'table', 'eval should return a table for SELECT')
            assert(#rows == 2, 'expected 2 rows, got ' .. tostring(#rows))
            assert(rows[1].name == 'alice', 'row 1 name mismatch: ' .. tostring(rows[1].name))
            assert(rows[2].name == 'bob', 'row 2 name mismatch: ' .. tostring(rows[2].name))
            db:close()
            return true
            "#,
        )
        .eval()
        .expect("in-memory sqlite round-trip");
    assert!(ok);
}
