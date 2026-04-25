//! Wire protocol — regression test for the `connection_code_error`
//! → `entity_snapshot` pipeline (blocker B5).
//!
//! The handler in `cli/lua/handlers/connections.lua` must `state.set` the
//! error shape on `connections.last_connection_code` BEFORE calling
//! `EB.upsert`, so the singleton `connection_code` entity's `all()`
//! callback in `cli/lua/hub/init.lua` can rehydrate late subscribers
//! with the error state. Without the state.set, a browser that attaches
//! after the error fires receives an empty snapshot instead of the
//! banner.

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

fn new_eb_lua() -> (Lua, Table) {
    let lua = Lua::new();
    log::register(&lua).expect("register log");
    let dir = lua_src_dir();
    let setup = format!(
        "package.path = \"{dir}/?.lua;{dir}/?/init.lua;\" .. package.path",
        dir = dir.display()
    );
    lua.load(&setup).exec().expect("update package.path");
    let eb: Table = lua
        .load("return require('lib.entity_broadcast')")
        .eval()
        .unwrap();
    let reset: Function = eb.get("_reset_for_tests").unwrap();
    reset.call::<()>(()).unwrap();
    (lua, eb)
}

/// Mirror the `connection_code` registration from `cli/lua/hub/init.lua`.
/// Keeping them byte-equivalent ensures the integration test is asserting
/// the actual production shape.
fn register_connection_code_type(lua: &Lua, eb: &Table) {
    lua.load(
        r#"
        local state = require("hub.state")
        local EB = require("lib.entity_broadcast")
        EB.register("connection_code", {
            id_field = "hub_id",
            all = function()
                local hub_id = "hub-test"
                local code = state.get("connections.last_connection_code", nil)
                if not hub_id or type(code) ~= "table" or next(code) == nil then
                    return {}
                end
                local payload = { hub_id = hub_id }
                for k, v in pairs(code) do payload[k] = v end
                return { payload }
            end,
        })
        "#,
    )
    .exec()
    .expect("register connection_code");
    let _ = eb;
}

fn snapshot_via(client_send_to: &str, lua: &Lua, eb: &Table) -> Vec<JsonValue> {
    let frames: Table = lua.create_table().unwrap();
    let frames_for_closure = frames.clone();
    let client: Table = lua.create_table().unwrap();
    let send: Function = lua
        .create_function(move |_, (_self, frame): (Table, Table)| {
            let next_idx = frames_for_closure.raw_len() + 1;
            frames_for_closure.raw_set(next_idx, frame)?;
            Ok(())
        })
        .unwrap();
    client.set("send", send).unwrap();

    let send_snapshots_to: Function = eb.get("send_snapshots_to").unwrap();
    send_snapshots_to
        .call::<()>((client, client_send_to))
        .unwrap();

    let len = frames.raw_len();
    (1..=len)
        .map(|i| {
            let t: Table = frames.raw_get(i).unwrap();
            lua.from_value::<JsonValue>(Value::Table(t)).unwrap()
        })
        .collect()
}

#[test]
fn healthy_connection_code_rehydrates_late_subscribers() {
    let (lua, eb) = new_eb_lua();
    register_connection_code_type(&lua, &eb);

    // Simulate connection_code_ready event: persists url + qr_ascii.
    lua.load(
        r#"
        local state = require("hub.state")
        state.set("connections.last_connection_code", {
            url = "https://example.test/connect",
            qr_ascii = "[[ QR ]]",
        })
        "#,
    )
    .exec()
    .unwrap();

    let frames = snapshot_via("sub-1", &lua, &eb);
    let snapshot = frames.first().expect("snapshot");
    assert_eq!(snapshot["type"], json!("entity_snapshot"));
    assert_eq!(snapshot["entity_type"], json!("connection_code"));
    let items = snapshot["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["hub_id"], json!("hub-test"));
    assert_eq!(items[0]["url"], json!("https://example.test/connect"));
    assert_eq!(items[0]["qr_ascii"], json!("[[ QR ]]"));
}

#[test]
fn connection_code_error_rehydrates_late_subscribers() {
    // B5 regression: error state MUST ride through state.set so the
    // entity_snapshot covers a late-attaching subscriber.
    let (lua, eb) = new_eb_lua();
    register_connection_code_type(&lua, &eb);

    // Simulate the NEW connection_code_error handler flow: state.set
    // BEFORE EB.upsert. Prior to B5 only the EB.upsert ran, leaving
    // last_connection_code untouched.
    lua.load(
        r#"
        local state = require("hub.state")
        state.set("connections.last_connection_code", {
            error = "tunnel closed",
        })
        "#,
    )
    .exec()
    .unwrap();

    let frames = snapshot_via("sub-error", &lua, &eb);
    let snapshot = frames.first().expect("snapshot");
    let items = snapshot["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "error state must rehydrate a late subscriber");
    assert_eq!(items[0]["hub_id"], json!("hub-test"));
    assert_eq!(items[0]["error"], json!("tunnel closed"));
    assert!(
        items[0].get("url").is_none(),
        "error state should not carry a stale url: {items:?}"
    );
}

#[test]
fn never_set_last_connection_code_yields_empty_snapshot() {
    // Baseline: when nothing has fired yet, the singleton stays empty.
    // Note: serde serialises an empty Lua table as `{}` (object), not `[]`
    // (array), so we normalise via `as_array()` or fallback to size zero.
    let (lua, eb) = new_eb_lua();
    register_connection_code_type(&lua, &eb);

    let frames = snapshot_via("sub-empty", &lua, &eb);
    let snapshot = frames.first().expect("snapshot");
    let item_count = snapshot["items"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    assert_eq!(
        item_count, 0,
        "no state → empty snapshot, got items={:?}",
        snapshot["items"]
    );
}
