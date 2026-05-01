//! Lua tests for plugin entity publishing through `lib.hub`.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    reason = "test-code brevity"
)]

use std::path::PathBuf;

use botster::lua::primitives::log;
use mlua::{Lua, LuaSerdeExt, Table, Value};
use serde_json::{json, Value as JsonValue};

fn lua_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua")
}

fn new_lua() -> Lua {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    lua.load(format!(
        r#"
        package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path
        hub = {{
          hub_id = function() return "hub-local" end,
        }}
        package.loaded["lib.agent"] = {{
          all_info = function() return {{}} end,
          list = function() return {{}} end,
        }}
        package.loaded["lib.internal_client"] = {{
          dispatch = function() error("internal dispatch should not be used") end,
        }}
        "#,
        dir = dir.display()
    ))
    .exec()
    .expect("configure lua");

    lua
}

fn frames_as_json(lua: &Lua, frames: &Table) -> Vec<JsonValue> {
    let len = frames.raw_len();
    let mut out = Vec::with_capacity(len);
    for i in 1..=len {
        let frame: Table = frames.raw_get(i).expect("frames[i] is a table");
        out.push(
            lua.from_value::<JsonValue>(Value::Table(frame))
                .expect("frame -> json"),
        );
    }
    out
}

#[test]
fn hub_entity_methods_publish_plugin_owned_frames() {
    let lua = new_lua();

    let frames: Table = lua
        .load(
            r#"
            local EB = require("lib.entity_broadcast")
            EB._reset_for_tests()

            local frames = {}
            EB.set_broadcaster(function(frame)
              frames[#frames + 1] = frame
            end)

            local boards = {
              { id = "board-1", name = "Roadmap", status = "active" },
            }
            EB.register("kanban.board", {
              id_field = "id",
              owner_plugin = "kanban",
              all = function() return boards end,
            })

            local Hub = require("lib.hub")
            local h = Hub.get()

            h:entity_snapshot("kanban.board", boards, { owner_plugin = "kanban" })
            h:entity_upsert("kanban.board", {
              id = "board-2",
              name = "Triage",
              status = "active",
            }, { owner_plugin = "kanban" })
            h:entity_patch("kanban.board", "board-2", {
              status = "archived",
              counts = { open = 0 },
            }, { owner_plugin = "kanban" })
            h:entity_remove("kanban.board", "board-2", { owner_plugin = "kanban" })

            return frames
            "#,
        )
        .eval()
        .expect("hub entity publish script");

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured.len(), 4);

    assert_eq!(captured[0]["type"], json!("entity_snapshot"));
    assert_eq!(captured[0]["entity_type"], json!("kanban.board"));
    assert_eq!(captured[0]["items"][0]["id"], json!("board-1"));
    assert_eq!(captured[0]["snapshot_seq"], json!(1));

    assert_eq!(captured[1]["type"], json!("entity_upsert"));
    assert_eq!(captured[1]["id"], json!("board-2"));
    assert_eq!(captured[1]["entity"]["name"], json!("Triage"));

    assert_eq!(captured[2]["type"], json!("entity_patch"));
    assert_eq!(captured[2]["id"], json!("board-2"));
    assert_eq!(captured[2]["patch"]["status"], json!("archived"));

    assert_eq!(captured[3]["type"], json!("entity_remove"));
    assert_eq!(captured[3]["id"], json!("board-2"));
}

#[test]
fn hub_entity_methods_reject_cross_plugin_and_bad_ids() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local EB = require("lib.entity_broadcast")
            EB._reset_for_tests()
            EB.register("kanban.board", {
              id_field = "id",
              owner_plugin = "kanban",
              all = function() return {} end,
            })

            local Hub = require("lib.hub")
            local h = Hub.get()

            local ok, err = pcall(function()
              h:entity_upsert("kanban.board", { id = "board-1" }, { owner_plugin = "other" })
            end)
            assert(ok == false)
            assert(tostring(err):find("namespace", 1, true), tostring(err))

            ok, err = pcall(function()
              h:entity_upsert("kanban.board", { id = 42 }, { owner_plugin = "kanban" })
            end)
            assert(ok == false)
            assert(tostring(err):find("non-empty string id", 1, true), tostring(err))

            ok, err = pcall(function()
              h:entity_snapshot("session", {}, { owner_plugin = "kanban" })
            end)
            assert(ok == false)
            assert(tostring(err):find("<plugin>.<type>", 1, true), tostring(err))

            return "ok"
            "#,
        )
        .eval()
        .expect("hub entity validation script");

    assert_eq!(result, "ok");
}
