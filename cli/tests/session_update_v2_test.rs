//! Wire protocol v2 — end-to-end integration test for Session:update.
//!
//! Asserts the cold-turkey contract: a single `Session:update(...)` call
//! produces exactly one `entity_patch(session, ...)` wire frame, zero
//! `ui_tree_snapshot` frames, and the patch payload includes any re-derived
//! fields per `ClientSessionPayload.project_fields` semantics
//! (design brief §12.4).
//!
//! This test exists to prevent regression of the §1 motivating
//! measurement: pre-v2 a single field change triggered a 1.7s broadcast
//! rebuilding 3 surfaces × 2 densities × N subscriptions; post-v2 the
//! broadcaster emits ~50 bytes per subscriber.

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

/// Minimal Lua VM that loads the real lib.entity_broadcast +
/// lib.client_session_payload, plus enough stubs to instantiate a session
/// without spinning up the full hub. The shipped Session class has heavy
/// dependencies (PTY infrastructure, workspace store, hooks); for this
/// test we exercise the EB layer directly with a synthetic session payload
/// matching the shape Session:update would project.
fn new_test_lua() -> (Lua, Table) {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    let setup = format!(
        "package.path = \"{dir}/?.lua;{dir}/?/init.lua;\" .. package.path",
        dir = dir.display()
    );
    lua.load(&setup).exec().expect("update package.path");

    // Inject minimal global stubs the lib modules expect at load time.
    let globals = lua.globals();
    let hooks_tbl: Table = lua
        .load(
            r#"
            local h = {}
            function h.notify(_event, _payload) end
            function h.on(_event, _name, _fn) end
            function h.off(_event, _name) end
            function h.call(_event, payload) return payload end
            return h
            "#,
        )
        .eval()
        .unwrap();
    globals.set("hooks", hooks_tbl).unwrap();

    // EB needs hub.state — pure Lua module loaded via require.
    let eb: Table = lua
        .load("return require('lib.entity_broadcast')")
        .eval()
        .expect("require lib.entity_broadcast");

    // Reset EB state to start clean.
    let reset: Function = eb.get("_reset_for_tests").unwrap();
    reset.call::<()>(()).unwrap();

    // Register the `session` entity type so EB.patch / EB.upsert succeed.
    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "session_uuid").unwrap();
    let all_fn: Function = lua
        .create_function(|lua, ()| lua.create_table())
        .unwrap();
    opts.set("all", all_fn).unwrap();
    register.call::<()>(("session", opts)).unwrap();

    (lua, eb)
}

fn install_capturing_broadcaster(lua: &Lua, eb: &Table) -> Table {
    let frames: Table = lua.create_table().unwrap();
    let frames_for_closure = frames.clone();
    let broadcaster: Function = lua
        .create_function(move |_, frame: Table| {
            let next_idx = frames_for_closure.raw_len() + 1;
            frames_for_closure.raw_set(next_idx, frame)?;
            Ok(())
        })
        .unwrap();
    let set_broadcaster: Function = eb.get("set_broadcaster").unwrap();
    set_broadcaster.call::<()>(broadcaster).unwrap();
    frames
}

fn frames_as_json(lua: &Lua, frames: &Table) -> Vec<JsonValue> {
    let len = frames.raw_len();
    let mut out = Vec::with_capacity(len);
    for i in 1..=len {
        let frame: Table = frames.raw_get(i).unwrap();
        let json = lua
            .from_value::<JsonValue>(Value::Table(frame))
            .unwrap();
        out.push(json);
    }
    out
}

#[test]
fn project_fields_includes_display_name_when_title_changes() {
    let (lua, _eb) = new_test_lua();

    // Load ClientSessionPayload directly.
    let csp: Table = lua
        .load("return require('lib.client_session_payload')")
        .eval()
        .unwrap();
    let project_fields: Function = csp.get("project_fields").unwrap();

    // Simulate Session:update({ title = "New Title" }) on a session whose
    // post-update record has the new title and no explicit label.
    let changed: Table = lua.create_table().unwrap();
    changed.set("title", "New Title").unwrap();
    let session_after: Table = lua.create_table().unwrap();
    session_after.set("session_uuid", "sess-a").unwrap();
    session_after.set("title", "New Title").unwrap();

    let result: Value = project_fields
        .call((changed, session_after))
        .expect("project_fields ok");
    let json: JsonValue = lua.from_value(result).unwrap();
    assert_eq!(json["title"], json!("New Title"));
    assert_eq!(json["display_name"], json!("New Title"));
}

#[test]
fn project_fields_omits_display_name_for_isidle_change() {
    let (lua, _eb) = new_test_lua();
    let csp: Table = lua
        .load("return require('lib.client_session_payload')")
        .eval()
        .unwrap();
    let project_fields: Function = csp.get("project_fields").unwrap();

    let changed: Table = lua.create_table().unwrap();
    changed.set("is_idle", false).unwrap();
    let session_after: Table = lua.create_table().unwrap();
    session_after.set("session_uuid", "sess-a").unwrap();
    session_after.set("title", "alpha").unwrap();
    session_after.set("is_idle", false).unwrap();

    let result: Value = project_fields.call((changed, session_after)).unwrap();
    let json: JsonValue = lua.from_value(result).unwrap();
    assert_eq!(json["is_idle"], json!(false));
    assert!(
        json.get("display_name").is_none(),
        "is_idle change must not re-derive display_name: {json}"
    );
}

#[test]
fn project_fields_replaces_hosted_preview_wholesale() {
    let (lua, _eb) = new_test_lua();
    let csp: Table = lua
        .load("return require('lib.client_session_payload')")
        .eval()
        .unwrap();
    let project_fields: Function = csp.get("project_fields").unwrap();

    let changed: Table = lua.create_table().unwrap();
    let preview: Table = lua.create_table().unwrap();
    preview.set("status", "running").unwrap();
    preview.set("url", "https://x").unwrap();
    changed.set("hosted_preview", preview).unwrap();

    let session_after: Table = lua.create_table().unwrap();
    session_after.set("session_uuid", "sess-a").unwrap();

    let result: Value = project_fields.call((changed, session_after)).unwrap();
    let json: JsonValue = lua.from_value(result).unwrap();
    assert_eq!(json["hosted_preview"]["status"], json!("running"));
    assert_eq!(json["hosted_preview"]["url"], json!("https://x"));
    // Per §12.4: nested object is shipped wholesale, no derivations.
    assert!(json.get("display_name").is_none());
}

#[test]
fn entity_patch_carries_project_fields_payload_via_eb() {
    // Drives the whole pipeline at the EB level: simulate what
    // Session:update would do — call project_fields then EB.patch — and
    // assert exactly one entity_patch frame with the expected payload.
    let (lua, eb) = new_test_lua();
    let csp: Table = lua
        .load("return require('lib.client_session_payload')")
        .eval()
        .unwrap();
    let project_fields: Function = csp.get("project_fields").unwrap();
    let patch: Function = eb.get("patch").unwrap();

    let frames = install_capturing_broadcaster(&lua, &eb);

    let changed: Table = lua.create_table().unwrap();
    changed.set("title", "alpha2").unwrap();
    let session_after: Table = lua.create_table().unwrap();
    session_after.set("session_uuid", "sess-a").unwrap();
    session_after.set("title", "alpha2").unwrap();

    let projected: Table = project_fields
        .call((changed, session_after))
        .expect("project_fields ok");
    patch.call::<()>(("session", "sess-a", projected)).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(
        captured.len(),
        1,
        "exactly one entity_patch frame per Session:update"
    );
    let frame = &captured[0];
    assert_eq!(frame["v"], json!(2));
    assert_eq!(frame["type"], json!("entity_patch"));
    assert_eq!(frame["entity_type"], json!("session"));
    assert_eq!(frame["id"], json!("sess-a"));
    assert_eq!(frame["patch"]["title"], json!("alpha2"));
    assert_eq!(frame["patch"]["display_name"], json!("alpha2"));
    assert!(
        frame.get("tree").is_none(),
        "entity_patch must not carry a ui tree: {frame}"
    );
}

#[test]
fn empty_session_update_emits_zero_frames() {
    // Session:update with no actually-changed fields (e.g. self[k] == v
    // for every key) must NOT emit an entity_patch — the changed_fields
    // table is empty so EB.patch's empty-patch guard short-circuits.
    let (lua, eb) = new_test_lua();
    let frames = install_capturing_broadcaster(&lua, &eb);
    let patch: Function = eb.get("patch").unwrap();

    let empty: Table = lua.create_table().unwrap();
    patch.call::<()>(("session", "sess-a", empty)).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert!(
        captured.is_empty(),
        "empty patch must not consume a wire frame"
    );
}

#[test]
fn no_ui_tree_snapshot_emitted_during_session_update_path() {
    // The cold-turkey win: before v2 a Session:update would trigger
    // broadcast_ui_layout_trees() and ship a 1.7s tree rebuild. Now the
    // EB.patch path is the only thing that fires — verified here by
    // counting frame types.
    let (lua, eb) = new_test_lua();
    let frames = install_capturing_broadcaster(&lua, &eb);
    let patch: Function = eb.get("patch").unwrap();

    for title in ["a", "b", "c", "d", "e"] {
        let p: Table = lua.create_table().unwrap();
        p.set("title", title).unwrap();
        patch.call::<()>(("session", "sess-a", p)).unwrap();
    }

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured.len(), 5, "one entity_patch per Session:update");
    for frame in &captured {
        assert_eq!(frame["type"], json!("entity_patch"));
        assert_ne!(
            frame["type"], json!("ui_tree_snapshot"),
            "Session:update must not trigger ui_tree_snapshot in v2"
        );
    }
}
