//! Integration tests for `cli/lua/lib/entity_broadcast.lua`.
//!
//! Bootstraps a Lua VM with the `log` primitive + the on-disk `lua/` tree on
//! the require path so the module under test can `require("hub.state")` like
//! it would inside a live hub. A capturing broadcaster collects every emitted
//! frame so test assertions can read the wire shape directly.
//!
//! Naming note: this is a Rust integration test by convention — there is no
//! Lua test harness in the repo, and Lua modules are exercised exclusively
//! via Rust integration tests (see `ui_contract_lua_test.rs`,
//! `ui_contract_web_layout_test.rs`).

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

/// Build a Lua VM with `log` registered and the lua/ tree on package.path,
/// then `require("lib.entity_broadcast")` and reset its state so each test
/// starts from a clean registry + zero seq counters.
fn new_eb_lua() -> (Lua, Table) {
    let lua = Lua::new();
    log::register(&lua).expect("register log primitive");

    let dir = lua_src_dir();
    let setup = format!(
        "package.path = \"{dir}/?.lua;{dir}/?/init.lua;\" .. package.path",
        dir = dir.display()
    );
    lua.load(&setup).exec().expect("update package.path");

    let eb: Table = lua
        .load("return require('lib.entity_broadcast')")
        .eval()
        .expect("require lib.entity_broadcast");

    let reset: Function = eb.get("_reset_for_tests").expect("_reset_for_tests fn");
    reset.call::<()>(()).expect("reset EB state");

    (lua, eb)
}

/// Install a capturing broadcaster: every emitted frame is appended to a
/// shared Lua table that the caller can later read back as a JSON array.
/// Returns the table reference so the caller can poll `#frames` etc.
fn install_capturing_broadcaster(lua: &Lua, eb: &Table) -> Table {
    let frames: Table = lua.create_table().expect("create frames table");
    let frames_for_closure = frames.clone();
    let broadcaster: Function = lua
        .create_function(move |_, frame: Table| {
            let next_idx = frames_for_closure.raw_len() + 1;
            frames_for_closure.raw_set(next_idx, frame)?;
            Ok(())
        })
        .expect("create broadcaster fn");

    let set_broadcaster: Function = eb.get("set_broadcaster").expect("set_broadcaster fn");
    set_broadcaster
        .call::<()>(broadcaster)
        .expect("install broadcaster");

    frames
}

fn frames_as_json(lua: &Lua, frames: &Table) -> Vec<JsonValue> {
    // mlua serializes an empty Lua table to `{}` (object), not `[]` (array).
    // Iterate raw_get(1..len) instead so empty == [] regardless of shape.
    let len = frames.raw_len();
    let mut out = Vec::with_capacity(len);
    for i in 1..=len {
        let frame: Table = frames.raw_get(i).expect("frames[i] is a table");
        let json = lua
            .from_value::<JsonValue>(Value::Table(frame))
            .expect("frame -> json");
        out.push(json);
    }
    out
}

fn register_session_type(lua: &Lua, eb: &Table) {
    let register: Function = eb.get("register").expect("register fn");
    let opts: Table = lua.create_table().expect("opts table");
    opts.set("id_field", "session_uuid").unwrap();
    let all_fn: Function = lua
        .create_function(|lua, ()| {
            // Return a fixed two-item snapshot. Tests that need a different
            // shape register their own entry.
            let arr = lua.create_table()?;
            let a = lua.create_table()?;
            a.set("session_uuid", "sess-a")?;
            a.set("title", "alpha")?;
            arr.set(1, a)?;
            let b = lua.create_table()?;
            b.set("session_uuid", "sess-b")?;
            b.set("title", "beta")?;
            arr.set(2, b)?;
            Ok(arr)
        })
        .unwrap();
    opts.set("all", all_fn).unwrap();
    register
        .call::<()>(("session", opts))
        .expect("register session");
}

// =============================================================================
// register / introspection
// =============================================================================

#[test]
fn register_then_is_registered_returns_true() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);

    let is_registered: Function = eb.get("is_registered").unwrap();
    let registered: bool = is_registered.call(("session",)).unwrap();
    assert!(registered, "session should be registered");

    let registered_other: bool = is_registered.call(("not_registered",)).unwrap();
    assert!(!registered_other);
}

#[test]
fn registered_types_returns_sorted_names() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);

    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "workspace_id").unwrap();
    let all: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    opts.set("all", all).unwrap();
    register.call::<()>(("workspace", opts)).unwrap();

    let registered_types: Function = eb.get("registered_types").unwrap();
    let names: Vec<String> = registered_types.call(()).unwrap();
    assert_eq!(names, vec!["session".to_string(), "workspace".to_string()]);
}

#[test]
fn register_rejects_missing_id_field() {
    let (lua, eb) = new_eb_lua();
    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    let all: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    opts.set("all", all).unwrap();
    let err = register.call::<()>(("session", opts)).unwrap_err();
    assert!(err.to_string().contains("id_field"), "{err}");
}

// =============================================================================
// upsert / patch / remove emit the right wire shapes
// =============================================================================

#[test]
fn patch_emits_entity_patch_frame_with_monotonic_seq() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);
    let frames = install_capturing_broadcaster(&lua, &eb);

    let patch: Function = eb.get("patch").unwrap();
    let p1: Table = lua.create_table().unwrap();
    p1.set("title", "first").unwrap();
    patch.call::<()>(("session", "sess-a", p1)).unwrap();

    let p2: Table = lua.create_table().unwrap();
    p2.set("title", "second").unwrap();
    p2.set("is_idle", false).unwrap();
    patch.call::<()>(("session", "sess-a", p2)).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured.len(), 2, "expected 2 frames, got {captured:?}");

    let first = &captured[0];
    assert_eq!(first["v"], json!(2));
    assert_eq!(first["type"], json!("entity_patch"));
    assert_eq!(first["entity_type"], json!("session"));
    assert_eq!(first["id"], json!("sess-a"));
    assert_eq!(first["patch"]["title"], json!("first"));
    assert_eq!(first["snapshot_seq"], json!(1));

    let second = &captured[1];
    assert_eq!(second["snapshot_seq"], json!(2));
    assert_eq!(second["patch"]["title"], json!("second"));
    assert_eq!(second["patch"]["is_idle"], json!(false));
}

#[test]
fn upsert_emits_entity_upsert_frame_with_id_resolution() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);
    let frames = install_capturing_broadcaster(&lua, &eb);

    let upsert: Function = eb.get("upsert").unwrap();
    let payload: Table = lua.create_table().unwrap();
    payload.set("session_uuid", "sess-c").unwrap();
    payload.set("title", "gamma").unwrap();
    payload.set("session_type", "agent").unwrap();
    upsert.call::<()>(("session", payload)).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured.len(), 1);
    let frame = &captured[0];
    assert_eq!(frame["type"], json!("entity_upsert"));
    assert_eq!(frame["id"], json!("sess-c"));
    assert_eq!(frame["entity"]["title"], json!("gamma"));
    assert_eq!(frame["entity"]["session_type"], json!("agent"));
    assert_eq!(frame["snapshot_seq"], json!(1));
}

#[test]
fn remove_emits_entity_remove_frame() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);
    let frames = install_capturing_broadcaster(&lua, &eb);

    let remove: Function = eb.get("remove").unwrap();
    remove.call::<()>(("session", "sess-a")).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured.len(), 1);
    let frame = &captured[0];
    assert_eq!(frame["type"], json!("entity_remove"));
    assert_eq!(frame["id"], json!("sess-a"));
    assert!(frame["entity"].is_null(), "remove carries no entity body");
    assert_eq!(frame["snapshot_seq"], json!(1));
}

#[test]
fn empty_patch_drops_silently_without_consuming_seq() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);
    let frames = install_capturing_broadcaster(&lua, &eb);

    let patch: Function = eb.get("patch").unwrap();
    let empty: Table = lua.create_table().unwrap();
    patch.call::<()>(("session", "sess-a", empty)).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert!(captured.is_empty(), "empty patch should not emit");

    let snapshot_seq: Function = eb.get("snapshot_seq").unwrap();
    let n: u64 = snapshot_seq.call(("session",)).unwrap();
    assert_eq!(n, 0, "empty patch must not consume a seq");
}

// =============================================================================
// snapshot priming
// =============================================================================

#[test]
fn send_snapshots_to_emits_one_snapshot_per_registered_type() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);

    let register: Function = eb.get("register").unwrap();
    let ws_opts: Table = lua.create_table().unwrap();
    ws_opts.set("id_field", "workspace_id").unwrap();
    let ws_all: Function = lua
        .create_function(|lua, ()| {
            let arr = lua.create_table()?;
            let w = lua.create_table()?;
            w.set("workspace_id", "ws-1")?;
            w.set("name", "first")?;
            arr.set(1, w)?;
            Ok(arr)
        })
        .unwrap();
    ws_opts.set("all", ws_all).unwrap();
    register.call::<()>(("workspace", ws_opts)).unwrap();

    // Mock client: collects every :send(msg) into an array. send() must be
    // a method (`self, msg`) because EB calls `client:send(frame)`.
    let captured: Table = lua.create_table().unwrap();
    let client: Table = lua.create_table().unwrap();
    let captured_for_send = captured.clone();
    let send: Function = lua
        .create_function(move |_, (_self, frame): (Table, Table)| {
            let next_idx = captured_for_send.raw_len() + 1;
            captured_for_send.raw_set(next_idx, frame)?;
            Ok(())
        })
        .unwrap();
    client.set("send", send).unwrap();

    let send_snapshots_to: Function = eb.get("send_snapshots_to").unwrap();
    send_snapshots_to.call::<()>((client, "sub-1")).unwrap();

    let json_frames = frames_as_json(&lua, &captured);
    assert_eq!(json_frames.len(), 2, "one snapshot per type");

    // Sorted alphabetically: session before workspace.
    assert_eq!(json_frames[0]["type"], json!("entity_snapshot"));
    assert_eq!(json_frames[0]["entity_type"], json!("session"));
    assert_eq!(json_frames[0]["items"].as_array().unwrap().len(), 2);
    assert_eq!(json_frames[0]["subscriptionId"], json!("sub-1"));

    assert_eq!(json_frames[1]["entity_type"], json!("workspace"));
    assert_eq!(json_frames[1]["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        json_frames[1]["items"][0]["workspace_id"],
        json!("ws-1")
    );
}

#[test]
fn send_snapshots_to_carries_current_snapshot_seq() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);
    install_capturing_broadcaster(&lua, &eb);

    // Two patches bump session's seq to 2.
    let patch: Function = eb.get("patch").unwrap();
    for title in ["one", "two"] {
        let p: Table = lua.create_table().unwrap();
        p.set("title", title).unwrap();
        patch.call::<()>(("session", "sess-a", p)).unwrap();
    }

    let captured: Table = lua.create_table().unwrap();
    let client: Table = lua.create_table().unwrap();
    let captured_for_send = captured.clone();
    let send: Function = lua
        .create_function(move |_, (_self, frame): (Table, Table)| {
            let next_idx = captured_for_send.raw_len() + 1;
            captured_for_send.raw_set(next_idx, frame)?;
            Ok(())
        })
        .unwrap();
    client.set("send", send).unwrap();

    let send_snapshots_to: Function = eb.get("send_snapshots_to").unwrap();
    send_snapshots_to.call::<()>((client, "sub-x")).unwrap();

    let frames = frames_as_json(&lua, &captured);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["snapshot_seq"], json!(2));
}

#[test]
fn fresh_type_sequences_start_from_process_epoch_floor() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);

    lua.load("require('hub.state').set('entity_broadcast.seq_epoch', 1234)")
        .exec()
        .unwrap();

    let captured: Table = lua.create_table().unwrap();
    let client: Table = lua.create_table().unwrap();
    let captured_for_send = captured.clone();
    let send: Function = lua
        .create_function(move |_, (_self, frame): (Table, Table)| {
            let next_idx = captured_for_send.raw_len() + 1;
            captured_for_send.raw_set(next_idx, frame)?;
            Ok(())
        })
        .unwrap();
    client.set("send", send).unwrap();

    let send_snapshots_to: Function = eb.get("send_snapshots_to").unwrap();
    send_snapshots_to.call::<()>((client, "sub-epoch")).unwrap();
    let frames = frames_as_json(&lua, &captured);
    assert_eq!(frames[0]["snapshot_seq"], json!(1234));

    let frames = install_capturing_broadcaster(&lua, &eb);
    let patch: Function = eb.get("patch").unwrap();
    let p: Table = lua.create_table().unwrap();
    p.set("title", "after epoch").unwrap();
    patch.call::<()>(("session", "sess-a", p)).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured[0]["snapshot_seq"], json!(1235));
}

// =============================================================================
// filter
// =============================================================================

#[test]
fn filter_excludes_items_from_snapshot_and_upsert() {
    let (lua, eb) = new_eb_lua();
    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "session_uuid").unwrap();

    // Snapshot source returns one system + one user session.
    let all_fn: Function = lua
        .create_function(|lua, ()| {
            let arr = lua.create_table()?;
            let sys = lua.create_table()?;
            sys.set("session_uuid", "sess-sys")?;
            sys.set("system_session", true)?;
            arr.set(1, sys)?;
            let user = lua.create_table()?;
            user.set("session_uuid", "sess-user")?;
            user.set("system_session", false)?;
            arr.set(2, user)?;
            Ok(arr)
        })
        .unwrap();
    opts.set("all", all_fn).unwrap();

    let filter_fn: Function = lua
        .create_function(|_, item: Table| {
            let sys: bool = item.get::<Option<bool>>("system_session")?.unwrap_or(false);
            Ok(!sys)
        })
        .unwrap();
    opts.set("filter", filter_fn).unwrap();
    register.call::<()>(("session", opts)).unwrap();

    let frames = install_capturing_broadcaster(&lua, &eb);

    // upsert with system_session=true must be silently dropped.
    let upsert: Function = eb.get("upsert").unwrap();
    let sys_payload: Table = lua.create_table().unwrap();
    sys_payload.set("session_uuid", "sess-sys2").unwrap();
    sys_payload.set("system_session", true).unwrap();
    upsert.call::<()>(("session", sys_payload)).unwrap();

    let user_payload: Table = lua.create_table().unwrap();
    user_payload.set("session_uuid", "sess-user2").unwrap();
    user_payload.set("system_session", false).unwrap();
    upsert.call::<()>(("session", user_payload)).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured.len(), 1, "only the user session should emit");
    assert_eq!(captured[0]["id"], json!("sess-user2"));

    // Snapshot priming also filters.
    let snap: Table = lua.create_table().unwrap();
    let client: Table = lua.create_table().unwrap();
    let snap_for_send = snap.clone();
    let send: Function = lua
        .create_function(move |_, (_self, frame): (Table, Table)| {
            let next_idx = snap_for_send.raw_len() + 1;
            snap_for_send.raw_set(next_idx, frame)?;
            Ok(())
        })
        .unwrap();
    client.set("send", send).unwrap();
    let send_snapshots_to: Function = eb.get("send_snapshots_to").unwrap();
    send_snapshots_to.call::<()>((client, Value::Nil)).unwrap();

    let snap_frames = frames_as_json(&lua, &snap);
    assert_eq!(snap_frames.len(), 1);
    let items = snap_frames[0]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["session_uuid"], json!("sess-user"));
}

// =============================================================================
// safety / error handling
// =============================================================================

#[test]
fn upsert_without_registration_warns_and_drops() {
    let (lua, eb) = new_eb_lua();
    let frames = install_capturing_broadcaster(&lua, &eb);

    let upsert: Function = eb.get("upsert").unwrap();
    let payload: Table = lua.create_table().unwrap();
    payload.set("session_uuid", "sess-x").unwrap();
    upsert.call::<()>(("never_registered", payload)).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert!(captured.is_empty());
}

#[test]
fn broadcaster_throwing_does_not_propagate() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);

    let throwing: Function = lua
        .create_function(|_, _frame: Table| -> mlua::Result<()> {
            Err(mlua::Error::RuntimeError("transport down".to_string()))
        })
        .unwrap();
    let set_broadcaster: Function = eb.get("set_broadcaster").unwrap();
    set_broadcaster.call::<()>(throwing).unwrap();

    // Should NOT panic / error out the mutator path.
    let patch: Function = eb.get("patch").unwrap();
    let p: Table = lua.create_table().unwrap();
    p.set("title", "x").unwrap();
    patch.call::<()>(("session", "sess-a", p)).unwrap();

    // Seq still bumped — failure is the broadcaster's, not the mutator's.
    let snapshot_seq: Function = eb.get("snapshot_seq").unwrap();
    let n: u64 = snapshot_seq.call(("session",)).unwrap();
    assert_eq!(n, 1);
}
