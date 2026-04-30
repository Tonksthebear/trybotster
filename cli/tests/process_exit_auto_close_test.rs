//! Regression tests for generic session process-exit handling.
//!
//! `handlers.connections` used to look up only Agent sessions on
//! `process_exited`. Plugin-owned accessories are generic Session instances,
//! so auto-close policy must route through `Session.get`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    reason = "test-code brevity"
)]

use std::path::PathBuf;

use mlua::Lua;

fn lua_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua")
}

fn new_lua() -> Lua {
    let lua = Lua::new();
    let dir = lua_src_dir();
    let setup = format!(
        r#"package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path"#,
        dir = dir.display()
    );
    lua.load(&setup).exec().expect("set package.path");
    lua
}

fn run_process_exited_case(auto_close_literal: &str) -> (i64, i64, String) {
    let lua = new_lua();
    let script = format!(
        r#"
        local callbacks = {{}}

        log = {{
          debug = function(...) end,
          info = function(...) end,
          warn = function(...) end,
          error = function(...) end,
        }}

        hooks = {{
          on = function(...) end,
          off = function(...) end,
          notify = function(...) end,
        }}

        timer = {{
          after_idle = function(_key, _delay, fn) fn() end,
        }}

        events = {{
          on = function(name, fn)
            callbacks[name] = fn
            return name .. ":sub"
          end,
          off = function(_id) end,
        }}

        package.loaded["hub.state"] = {{
          get = function(_key, default) return default end,
        }}
        package.loaded["lib.agent"] = {{
          get = function(_uuid) return nil end,
          list = function() return {{}} end,
        }}
        package.loaded["lib.entity_model"] = {{
          publish_session = function(...) end,
          remove_session = function(...) end,
          patch_session = function(...) end,
          upsert_session_workspace = function(...) end,
          upsert_workspace = function(...) end,
          patch_workspace = function(...) end,
          upsert_connection_code = function(...) end,
          upsert_hub = function(...) end,
        }}
        package.loaded["lib.pty_clients"] = {{
          set_focused = function(...) end,
          get_focused_sessions = function(...) return {{}} end,
          is_any_focused = function(...) return false end,
        }}
        package.loaded["lib.entity_broadcast"] = {{
          set_broadcaster = function(...) end,
        }}
        package.loaded["lib.surfaces"] = {{
          build_route_registry_payload = function()
            return {{ type = "ui_route_registry", routes = {{}} }}
          end,
          path = function(...) return nil end,
        }}
        local calls = {{ close = 0, update = 0, status = "" }}
        local session = {{
          get_meta = function(_self, key)
            if key == "auto_close_on_exit" then return {auto_close_literal} end
            return nil
          end,
          close = function(_self, delete_worktree)
            calls.close = calls.close + 1
            calls.delete_worktree = tostring(delete_worktree)
          end,
          update = function(_self, fields)
            calls.update = calls.update + 1
            calls.status = fields.status or ""
          end,
        }}
        package.loaded["lib.session"] = {{
          get = function(uuid)
            if uuid == "sess-accessory" then return session end
            return nil
          end,
          is_system_session = function(...) return false end,
        }}

        require("handlers.connections")
        callbacks.process_exited({{ session_uuid = "sess-accessory", exit_code = 0 }})

        return calls.close, calls.update, calls.status
        "#,
    );
    lua.load(&script).eval().expect("run process_exited case")
}

#[test]
fn process_exited_auto_closes_generic_session_when_metadata_true() {
    let (close_count, update_count, status) = run_process_exited_case("true");

    assert_eq!(close_count, 1);
    assert_eq!(update_count, 0);
    assert_eq!(status, "");
}

#[test]
fn process_exited_marks_generic_session_exited_without_auto_close() {
    let (close_count, update_count, status) = run_process_exited_case("false");

    assert_eq!(close_count, 0);
    assert_eq!(update_count, 1);
    assert_eq!(status, "exited");
}

#[test]
fn process_exited_accepts_string_true_for_auto_close_metadata() {
    let (close_count, update_count, status) = run_process_exited_case(r#""true""#);

    assert_eq!(close_count, 1);
    assert_eq!(update_count, 0);
    assert_eq!(status, "");
}
