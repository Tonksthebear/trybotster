//! Lua tests for generic plugin-owned session actions.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    reason = "test-code brevity"
)]

use std::path::PathBuf;

use botster::lua::primitives::log;
use mlua::{Function, Lua, LuaSerdeExt, Table, Value};
use serde_json::{json, Value as JsonValue};

fn lua_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua")
}

fn new_lua() -> Lua {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    let setup = format!(
        "package.path = \"{dir}/?.lua;{dir}/?/init.lua;\" .. package.path",
        dir = dir.display()
    );
    lua.load(&setup).exec().expect("update package.path");
    lua
}

fn install_session_stub(lua: &Lua) {
    lua.load(
        r#"
        local sessions = {
          ["sess-a"] = {
            session_uuid = "sess-a",
            session_type = "agent",
            title = "Alpha",
            port = 46001,
            status = "running",
            info = function(self)
              return {
                id = self.session_uuid,
                session_uuid = self.session_uuid,
                session_type = self.session_type,
                title = self.title,
                port = self.port,
                status = self.status,
              }
            end,
          },
          ["sess-b"] = {
            session_uuid = "sess-b",
            session_type = "agent",
            title = "Beta",
            status = "running",
            info = function(self)
              return {
                id = self.session_uuid,
                session_uuid = self.session_uuid,
                session_type = self.session_type,
                title = self.title,
                status = self.status,
              }
            end,
          },
        }
        package.loaded["lib.session"] = {
          get = function(session_uuid) return sessions[session_uuid] end,
          all_info = function()
            return { sessions["sess-a"]:info(), sessions["sess-b"]:info() }
          end,
          is_system_session = function(_) return false end,
        }
        "#,
    )
    .exec()
    .expect("install session stub");
}

fn install_entity_broadcast(lua: &Lua) -> (Table, Table) {
    let eb: Table = lua
        .load("return require('lib.entity_broadcast')")
        .eval()
        .expect("require entity_broadcast");
    let reset: Function = eb.get("_reset_for_tests").unwrap();
    reset.call::<()>(()).unwrap();

    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "id").unwrap();
    let all: Function = lua
        .create_function(|lua, ()| {
            let actions: Table = lua
                .load("return require('lib.session_actions').all()")
                .eval()?;
            Ok(actions)
        })
        .unwrap();
    opts.set("all", all).unwrap();
    register.call::<()>(("session_action", opts)).unwrap();

    let frames: Table = lua.create_table().unwrap();
    let frames_for_closure = frames.clone();
    let broadcaster: Function = lua
        .create_function(move |_, frame: Table| {
            let idx = frames_for_closure.raw_len() + 1;
            frames_for_closure.raw_set(idx, frame)?;
            Ok(())
        })
        .unwrap();
    let set_broadcaster: Function = eb.get("set_broadcaster").unwrap();
    set_broadcaster.call::<()>(broadcaster).unwrap();

    (eb, frames)
}

fn frames_as_json(lua: &Lua, frames: &Table) -> Vec<JsonValue> {
    let mut out = Vec::new();
    for i in 1..=frames.raw_len() {
        let frame: Table = frames.raw_get(i).unwrap();
        out.push(lua.from_value::<JsonValue>(Value::Table(frame)).unwrap());
    }
    out
}

#[test]
fn registration_publishes_session_action_entities_keyed_by_session_uuid() {
    let lua = new_lua();
    install_session_stub(&lua);
    let (_eb, frames) = install_entity_broadcast(&lua);

    lua.load(
        r#"
        local actions = require("lib.session_actions")
        actions._reset_for_tests()
        actions.register("example.open", {
          plugin = "example",
          label = function(session) return "Open " .. session.title end,
          status = function(session) return session.port and "available" or "missing_port" end,
          url = function(session) return session.port and ("http://127.0.0.1:" .. session.port) or nil end,
          error = function(session) return session.port and nil or "missing port" end,
          icon = "bolt",
          enabled = function(session) return session.port ~= nil end,
          visibility = function(session) return session.session_type == "agent" end,
          run = function(_, _, _) end,
        })
        "#,
    )
    .exec()
    .expect("register action");

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(
        captured.len(),
        2,
        "expected one upsert per session: {captured:?}"
    );
    assert_eq!(captured[0]["type"], json!("entity_upsert"));
    assert_eq!(captured[0]["entity_type"], json!("session_action"));
    assert_eq!(captured[0]["id"], json!("sess-a:example.open"));
    assert_eq!(captured[0]["entity"]["session_uuid"], json!("sess-a"));
    assert_eq!(captured[0]["entity"]["action_id"], json!("example.open"));
    assert_eq!(captured[0]["entity"]["label"], json!("Open Alpha"));
    assert_eq!(captured[0]["entity"]["status"], json!("available"));
    assert_eq!(
        captured[0]["entity"]["url"],
        json!("http://127.0.0.1:46001")
    );
    assert_eq!(captured[0]["entity"]["enabled"], json!(true));

    assert_eq!(captured[1]["id"], json!("sess-b:example.open"));
    assert_eq!(captured[1]["entity"]["session_uuid"], json!("sess-b"));
    assert_eq!(captured[1]["entity"]["status"], json!("missing_port"));
    assert_eq!(captured[1]["entity"]["error"], json!("missing port"));
    assert_eq!(captured[1]["entity"]["enabled"], json!(false));

    lua.load(
        r#"
        local actions = require("lib.session_actions")
        local session = require("lib.session").get("sess-a")
        actions.publish_for_session(session)
        "#,
    )
    .exec()
    .expect("republish unchanged action");
    let republished = frames_as_json(&lua, &frames);
    assert_eq!(
        republished.len(),
        2,
        "unchanged action descriptors should not be re-upserted"
    );

    lua.load(
        r#"
        local actions = require("lib.session_actions")
        local session = require("lib.session").get("sess-a")
        actions.purge_session("sess-a")
        actions.publish_for_session(session)
        "#,
    )
    .exec()
    .expect("republish purged action");
    let republished_after_purge = frames_as_json(&lua, &frames);
    assert_eq!(
        republished_after_purge.len(),
        3,
        "purging a session should allow its descriptor to publish again"
    );
}

#[test]
fn run_invokes_handler_with_session_uuid_and_action_id() {
    let lua = new_lua();
    install_session_stub(&lua);

    let result: JsonValue = lua
        .load(
            r#"
            local actions = require("lib.session_actions")
            actions._reset_for_tests()
            local seen = nil
            actions.register("example.restart", {
              label = "Restart",
              run = function(session_uuid, action_id, context)
                seen = {
                  session_uuid = session_uuid,
                  action_id = action_id,
                  param = context.params.force,
                  context_session_uuid = context.session.session_uuid,
                }
              end,
            })
            local ok, err = actions.run("sess-a", "example.restart", { params = { force = true } })
            return { ok = ok, err = err, seen = seen }
            "#,
        )
        .eval::<Value>()
        .map(|value| lua.from_value(value).unwrap())
        .expect("run action");

    assert_eq!(result["ok"], json!(true));
    assert_eq!(result["seen"]["session_uuid"], json!("sess-a"));
    assert_eq!(result["seen"]["action_id"], json!("example.restart"));
    assert_eq!(result["seen"]["param"], json!(true));
    assert_eq!(result["seen"]["context_session_uuid"], json!("sess-a"));
}

#[test]
fn run_preserves_plugin_returned_errors() {
    let lua = new_lua();
    install_session_stub(&lua);

    let result: JsonValue = lua
        .load(
            r#"
            local actions = require("lib.session_actions")
            actions._reset_for_tests()
            actions.register("example.fail", {
              label = "Fail",
              run = function()
                return nil, "plugin refused"
              end,
            })
            local ok, err = actions.run("sess-a", "example.fail", { params = {} })
            return { ok = ok, err = err }
            "#,
        )
        .eval::<Value>()
        .map(|value| lua.from_value(value).unwrap())
        .expect("run action");

    assert_eq!(result["ok"], JsonValue::Null);
    assert_eq!(result["err"], json!("plugin refused"));
}

#[test]
fn execute_session_action_command_routes_by_session_uuid() {
    let lua = new_lua();
    install_session_stub(&lua);

    let result: JsonValue = lua
        .load(
            r#"
            local registered = {}
            package.loaded["lib.commands"] = {
              register = function(name, handler, _opts) registered[name] = handler end,
              count = function()
                local n = 0
                for _ in pairs(registered) do n = n + 1 end
                return n
              end,
            }
            package.loaded["lib.target_context"] = { resolve = function(_) return { target_id = "t", target_path = "/tmp/t" } end }
            _G.hooks = { call = function(_, payload) return payload end, notify = function() end }

            require("handlers.commands")

            local actions = require("lib.session_actions")
            actions._reset_for_tests()
            local seen = nil
            actions.register("example.deploy", {
              label = "Deploy",
              run = function(session_uuid, action_id, context)
                seen = {
                  session_uuid = session_uuid,
                  action_id = action_id,
                  param = context.params.version,
                  sub_id = context.sub_id,
                }
              end,
            })

            registered.execute_session_action(nil, "sub-1", {
              type = "execute_session_action",
              session_uuid = "sess-a",
              action_id = "example.deploy",
              params = { version = "v1" },
            })
            return seen
            "#,
        )
        .eval::<Value>()
        .map(|value| lua.from_value(value).unwrap())
        .expect("execute command");

    assert_eq!(result["session_uuid"], json!("sess-a"));
    assert_eq!(result["action_id"], json!("example.deploy"));
    assert_eq!(result["param"], json!("v1"));
    assert_eq!(result["sub_id"], json!("sub-1"));
}
