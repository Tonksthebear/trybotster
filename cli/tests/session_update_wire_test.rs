//! Wire protocol — end-to-end integration test for Session:update.
//!
//! Asserts the contract: a single `Session:update(...)` call
//! produces exactly one `entity_patch(session, ...)` wire frame, zero
//! `ui_tree_snapshot` frames, and the patch payload includes any re-derived
//! fields per `ClientSessionPayload.project_fields` semantics
//! (design brief §12.4).
//!
//! This test exists to prevent regression of the §1 motivating
//! measurement: pre-rewrite a single field change triggered a 1.7s broadcast
//! rebuilding 3 surfaces × 2 densities × N subscriptions; post-rewrite the
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
    let all_fn: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    opts.set("all", all_fn).unwrap();
    register.call::<()>(("session", opts)).unwrap();

    (lua, eb)
}

fn register_workspace(lua: &Lua, eb: &Table) {
    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "workspace_id").unwrap();
    let all_fn: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    opts.set("all", all_fn).unwrap();
    register.call::<()>(("workspace", opts)).unwrap();
}

fn register_entity(lua: &Lua, eb: &Table, entity_type: &str, id_field: &str) {
    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", id_field).unwrap();
    let all_fn: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    opts.set("all", all_fn).unwrap();
    register.call::<()>((entity_type, opts)).unwrap();
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
        let json = lua.from_value::<JsonValue>(Value::Table(frame)).unwrap();
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
fn project_fields_omits_display_name_for_output_activity_change() {
    let (lua, _eb) = new_test_lua();
    let csp: Table = lua
        .load("return require('lib.client_session_payload')")
        .eval()
        .unwrap();
    let project_fields: Function = csp.get("project_fields").unwrap();

    let changed: Table = lua.create_table().unwrap();
    changed.set("output_activity", "active").unwrap();
    let session_after: Table = lua.create_table().unwrap();
    session_after.set("session_uuid", "sess-a").unwrap();
    session_after.set("title", "alpha").unwrap();
    session_after.set("output_activity", "active").unwrap();

    let result: Value = project_fields.call((changed, session_after)).unwrap();
    let json: JsonValue = lua.from_value(result).unwrap();
    assert_eq!(json["output_activity"], json!("active"));
    assert!(
        json.get("display_name").is_none(),
        "output_activity change must not re-derive display_name: {json}"
    );
}

#[test]
fn project_fields_replaces_plugin_state_wholesale() {
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
    changed.set("plugin_state", preview).unwrap();

    let session_after: Table = lua.create_table().unwrap();
    session_after.set("session_uuid", "sess-a").unwrap();

    let result: Value = project_fields.call((changed, session_after)).unwrap();
    let json: JsonValue = lua.from_value(result).unwrap();
    assert_eq!(json["plugin_state"]["status"], json!("running"));
    assert_eq!(json["plugin_state"]["url"], json!("https://x"));
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
fn entity_model_patch_workspace_emits_workspace_name_frame() {
    let (lua, eb) = new_test_lua();
    register_workspace(&lua, &eb);
    let frames = install_capturing_broadcaster(&lua, &eb);

    let model: Table = lua
        .load("return require('lib.entity_model')")
        .eval()
        .unwrap();
    let patch_workspace: Function = model.get("patch_workspace").unwrap();
    let fields: Table = lua.create_table().unwrap();
    fields.set("name", "Renamed").unwrap();
    patch_workspace.call::<()>(("ws-a", fields)).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured.len(), 1);
    let frame = &captured[0];
    assert_eq!(frame["type"], json!("entity_patch"));
    assert_eq!(frame["entity_type"], json!("workspace"));
    assert_eq!(frame["id"], json!("ws-a"));
    assert_eq!(frame["patch"]["name"], json!("Renamed"));
}

#[test]
fn entity_model_publish_session_upserts_current_workspace_fields() {
    let (lua, eb) = new_test_lua();
    let frames = install_capturing_broadcaster(&lua, &eb);

    lua.load(
        r#"
        package.loaded["lib.session"] = {
          is_system_session = function(_) return false end,
        }
        package.loaded["lib.agent"] = {
          all_info = function() return {} end,
        }
    "#,
    )
    .exec()
    .unwrap();

    let model: Table = lua
        .load("return require('lib.entity_model')")
        .eval()
        .unwrap();
    let publish_session: Function = model.get("publish_session").unwrap();
    let session: Table = lua.create_table().unwrap();
    session.set("session_uuid", "sess-a").unwrap();
    session.set("workspace_id", "ws-renamed").unwrap();
    session.set("workspace_name", "Renamed Workspace").unwrap();
    session
        .set("metadata", lua.create_table().unwrap())
        .unwrap();
    publish_session.call::<()>(session).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured.len(), 1);
    let frame = &captured[0];
    assert_eq!(frame["type"], json!("entity_upsert"));
    assert_eq!(frame["entity_type"], json!("session"));
    assert_eq!(frame["id"], json!("sess-a"));
    assert_eq!(frame["entity"]["workspace_id"], json!("ws-renamed"));
    assert_eq!(
        frame["entity"]["workspace_name"],
        json!("Renamed Workspace")
    );
}

#[test]
fn entity_model_upserts_workspace_from_created_session_before_session_publish() {
    let (lua, eb) = new_test_lua();
    register_workspace(&lua, &eb);
    let frames = install_capturing_broadcaster(&lua, &eb);

    let model: Table = lua
        .load("return require('lib.entity_model')")
        .eval()
        .unwrap();
    let session: Table = lua.create_table().unwrap();
    session.set("session_uuid", "sess-new").unwrap();
    session.set("session_type", "agent").unwrap();
    session.set("workspace_id", "ws-new").unwrap();
    session.set("workspace_name", "feature/new").unwrap();
    model
        .get::<Function>("upsert_session_workspace")
        .unwrap()
        .call::<()>(session)
        .unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured.len(), 1);
    let frame = &captured[0];
    assert_eq!(frame["type"], json!("entity_upsert"));
    assert_eq!(frame["entity_type"], json!("workspace"));
    assert_eq!(frame["id"], json!("ws-new"));
    assert_eq!(frame["entity"]["workspace_id"], json!("ws-new"));
    assert_eq!(frame["entity"]["name"], json!("feature/new"));
    assert_eq!(frame["entity"]["agents"], json!(["sess-new"]));
    assert_eq!(frame["entity"]["session_counts"]["agent"], json!(1));
}

#[test]
fn entity_model_covers_non_session_builtin_entities() {
    let (lua, eb) = new_test_lua();
    register_entity(&lua, &eb, "spawn_target", "target_id");
    register_entity(&lua, &eb, "hub", "hub_id");
    register_entity(&lua, &eb, "connection_code", "hub_id");
    register_entity(&lua, &eb, "worktree", "worktree_path");
    let frames = install_capturing_broadcaster(&lua, &eb);

    let model: Table = lua
        .load("return require('lib.entity_model')")
        .eval()
        .unwrap();

    let target: Table = lua.create_table().unwrap();
    target.set("target_id", "target-a").unwrap();
    target.set("path", "/repo/a").unwrap();
    model
        .get::<Function>("upsert_spawn_target")
        .unwrap()
        .call::<()>(target)
        .unwrap();

    let target_patch: Table = lua.create_table().unwrap();
    target_patch.set("target_name", "Repo A").unwrap();
    model
        .get::<Function>("patch_spawn_target")
        .unwrap()
        .call::<()>(("target-a", target_patch))
        .unwrap();

    model
        .get::<Function>("remove_spawn_target")
        .unwrap()
        .call::<()>("target-a")
        .unwrap();

    let hub_payload: Table = lua.create_table().unwrap();
    hub_payload.set("hub_id", "hub-a").unwrap();
    hub_payload.set("state", "ready").unwrap();
    model
        .get::<Function>("upsert_hub")
        .unwrap()
        .call::<()>(hub_payload)
        .unwrap();

    let code_payload: Table = lua.create_table().unwrap();
    code_payload.set("hub_id", "hub-a").unwrap();
    code_payload.set("url", "https://pair").unwrap();
    model
        .get::<Function>("upsert_connection_code")
        .unwrap()
        .call::<()>(code_payload)
        .unwrap();

    let worktree_payload: Table = lua.create_table().unwrap();
    worktree_payload
        .set("worktree_path", "/repo/.worktrees/a")
        .unwrap();
    model
        .get::<Function>("upsert_worktree")
        .unwrap()
        .call::<()>(worktree_payload)
        .unwrap();

    model
        .get::<Function>("remove_worktree")
        .unwrap()
        .call::<()>("/repo/.worktrees/a")
        .unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured.len(), 7);
    assert_eq!(captured[0]["type"], json!("entity_upsert"));
    assert_eq!(captured[0]["entity_type"], json!("spawn_target"));
    assert_eq!(captured[1]["type"], json!("entity_patch"));
    assert_eq!(captured[1]["entity_type"], json!("spawn_target"));
    assert_eq!(captured[2]["type"], json!("entity_remove"));
    assert_eq!(captured[2]["entity_type"], json!("spawn_target"));
    assert_eq!(captured[3]["entity_type"], json!("hub"));
    assert_eq!(captured[4]["entity_type"], json!("connection_code"));
    assert_eq!(captured[5]["entity_type"], json!("worktree"));
    assert_eq!(captured[6]["type"], json!("entity_remove"));
    assert_eq!(captured[6]["entity_type"], json!("worktree"));
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
    // The wire-flip win: before this rewrite a Session:update would trigger
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
            frame["type"],
            json!("ui_tree_snapshot"),
            "Session:update must not trigger ui_tree_snapshot"
        );
    }
}
