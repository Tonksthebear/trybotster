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

#[test]
fn register_rejects_unreserved_non_plugin_entity_type() {
    let (lua, eb) = new_eb_lua();
    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "id").unwrap();
    let all: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    opts.set("all", all).unwrap();

    let err = register.call::<()>(("kanban_board", opts)).unwrap_err();
    assert!(err.to_string().contains("<plugin>.<type>"), "{err}");
}

#[test]
fn register_rejects_malformed_plugin_entity_type() {
    let (lua, eb) = new_eb_lua();
    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "id").unwrap();
    opts.set("owner_plugin", "kanban").unwrap();
    let all: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    opts.set("all", all).unwrap();

    let err = register.call::<()>(("kanban..board", opts)).unwrap_err();
    assert!(err.to_string().contains("<plugin>.<type>"), "{err}");
}

#[test]
fn plugin_entity_type_requires_owner_namespace_and_id_field() {
    let (lua, eb) = new_eb_lua();
    let register: Function = eb.get("register").unwrap();

    let missing_owner: Table = lua.create_table().unwrap();
    missing_owner.set("id_field", "id").unwrap();
    let all: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    missing_owner.set("all", all).unwrap();
    let err = register
        .call::<()>(("kanban.board", missing_owner))
        .unwrap_err();
    assert!(err.to_string().contains("owner_plugin"), "{err}");

    let wrong_owner: Table = lua.create_table().unwrap();
    wrong_owner.set("id_field", "id").unwrap();
    wrong_owner.set("owner_plugin", "other").unwrap();
    let all: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    wrong_owner.set("all", all).unwrap();
    let err = register
        .call::<()>(("kanban.board", wrong_owner))
        .unwrap_err();
    assert!(err.to_string().contains("namespace"), "{err}");

    let wrong_id: Table = lua.create_table().unwrap();
    wrong_id.set("id_field", "board_id").unwrap();
    wrong_id.set("owner_plugin", "kanban").unwrap();
    let all: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    wrong_id.set("all", all).unwrap();
    let err = register.call::<()>(("kanban.board", wrong_id)).unwrap_err();
    assert!(err.to_string().contains("id_field=\"id\""), "{err}");
}

#[test]
fn plugin_entity_type_rejects_cross_plugin_ownership_conflict() {
    let (lua, eb) = new_eb_lua();
    let register: Function = eb.get("register").unwrap();

    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "id").unwrap();
    opts.set("owner_plugin", "kanban").unwrap();
    let all: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    opts.set("all", all).unwrap();
    register.call::<()>(("kanban.board", opts)).unwrap();

    let same_owner: Table = lua.create_table().unwrap();
    same_owner.set("id_field", "id").unwrap();
    same_owner.set("owner_plugin", "kanban").unwrap();
    let all: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    same_owner.set("all", all).unwrap();
    register
        .call::<()>(("kanban.board", same_owner))
        .expect("same plugin hot reload should re-register");

    let hijack: Table = lua.create_table().unwrap();
    hijack.set("id_field", "id").unwrap();
    hijack.set("owner_plugin", "kanban").unwrap();
    let all: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    hijack.set("all", all).unwrap();
    let err = register.call::<()>(("other.board", hijack)).unwrap_err();
    assert!(err.to_string().contains("namespace"), "{err}");
}

#[test]
fn unregister_plugin_removes_only_owned_plugin_entity_types() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);

    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "id").unwrap();
    opts.set("owner_plugin", "kanban").unwrap();
    let all: Function = lua.create_function(|lua, ()| lua.create_table()).unwrap();
    opts.set("all", all).unwrap();
    register.call::<()>(("kanban.board", opts)).unwrap();

    let unregister_plugin: Function = eb.get("unregister_plugin").unwrap();
    unregister_plugin.call::<()>(("kanban",)).unwrap();

    let is_registered: Function = eb.get("is_registered").unwrap();
    assert!(!is_registered.call::<bool>(("kanban.board",)).unwrap());
    assert!(is_registered.call::<bool>(("session",)).unwrap());
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
    p2.set("output_activity", "active").unwrap();
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
    assert_eq!(second["patch"]["output_activity"], json!("active"));
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
fn upsert_falls_back_to_id_when_registered_id_field_is_absent() {
    // Regression: spawn_target records carry `id` (from the SpawnTarget
    // struct) but the EB registration uses `id_field = "target_id"`. Hub
    // call sites used to guard `if t.target_id then EB.upsert(...)` before
    // shipping, dropping every record. EB internally falls back to
    // `payload.id` when the configured id_field is missing — this test
    // pins that contract so call sites can drop their pre-guards.
    let (lua, eb) = new_eb_lua();
    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "target_id").unwrap();
    let all_fn: Function = lua
        .create_function(|lua, ()| Ok(lua.create_table()?))
        .unwrap();
    opts.set("all", all_fn).unwrap();
    register
        .call::<()>(("spawn_target", opts))
        .expect("register spawn_target");
    let frames = install_capturing_broadcaster(&lua, &eb);

    let upsert: Function = eb.get("upsert").unwrap();
    // Mimic SpawnTarget serde shape: `id` present, `target_id` absent.
    let payload: Table = lua.create_table().unwrap();
    payload.set("id", "tgt-abc").unwrap();
    payload.set("name", "primary").unwrap();
    payload.set("path", "/tmp/repo").unwrap();
    payload.set("enabled", true).unwrap();
    upsert.call::<()>(("spawn_target", payload)).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(
        captured.len(),
        1,
        "upsert must emit one frame even without target_id"
    );
    let frame = &captured[0];
    assert_eq!(frame["type"], json!("entity_upsert"));
    assert_eq!(frame["entity_type"], json!("spawn_target"));
    assert_eq!(
        frame["id"],
        json!("tgt-abc"),
        "id should fall back to payload.id when target_id is missing"
    );
    assert_eq!(frame["entity"]["name"], json!("primary"));
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
    assert_eq!(json_frames[1]["items"][0]["workspace_id"], json!("ws-1"));
}

#[test]
fn schedule_snapshots_to_defers_work_and_cancels_unsubscribed_clients() {
    let (lua, eb) = new_eb_lua();
    lua.globals().set("EB", eb).unwrap();

    let result: bool = lua
        .load(
            r#"
            local queue = {}
            timer = {
              after = function(_delay, fn)
                queue[#queue + 1] = fn
                return "timer-" .. tostring(#queue)
              end,
            }

            local calls = 0
            EB.register("session", {
              id_field = "session_uuid",
              all = function(_context)
                calls = calls + 1
                return { { session_uuid = "sess-1" } }
              end,
            })
            EB.register("workspace", {
              id_field = "workspace_id",
              all = function(_context)
                calls = calls + 1
                return { { workspace_id = "ws-1" } }
              end,
            })

            local captured = {}
            local client = {
              subscriptions = { ["sub-1"] = { channel = "hub" } },
              send = function(_, frame)
                captured[#captured + 1] = frame
              end,
            }

            local scheduled = EB.schedule_snapshots_to(client, "sub-1", {
              types = { "session", "workspace" },
            })
            local deferred = scheduled == 0 and #captured == 0 and #queue == 1 and calls == 0

            queue[1]()
            local sent_one = #captured == 1 and captured[1].entity_type == "session"
              and #queue == 2 and calls == 1

            client.subscriptions["sub-1"] = nil
            queue[2]()
            local canceled = #captured == 1 and calls == 1

            return deferred and sent_one and canceled
        "#,
        )
        .eval()
        .expect("scheduled snapshot script should evaluate");

    assert!(
        result,
        "scheduled snapshots should leave the command turn, step by type, and stop after unsubscribe"
    );
}

#[test]
fn schedule_snapshots_to_shares_context_across_plugin_entity_providers() {
    let (lua, eb) = new_eb_lua();
    lua.globals().set("EB", eb).unwrap();

    let result: bool = lua
        .load(
            r#"
            local queue = {}
            timer = {
              after = function(_delay, fn)
                queue[#queue + 1] = fn
                return "timer-" .. tostring(#queue)
              end,
            }

            EB.register("plugin.alpha", {
              id_field = "id",
              owner_plugin = "plugin",
              all = function(context)
                context.shared_rows = { { id = "row-1" } }
                return { { id = "alpha" } }
              end,
            })
            EB.register("plugin.beta", {
              id_field = "id",
              owner_plugin = "plugin",
              all = function(context)
                local shared = context.shared_rows and context.shared_rows[1]
                return { { id = "beta", shared_id = shared and shared.id or "missing" } }
              end,
            })

            local captured = {}
            local client = {
              subscriptions = { ["sub-1"] = { channel = "hub" } },
              send = function(_, frame)
                captured[#captured + 1] = frame
              end,
            }

            EB.schedule_snapshots_to(client, "sub-1", {
              types = { "plugin.alpha", "plugin.beta" },
            })

            queue[1]()
            queue[2]()
            return #captured == 2
              and captured[1].entity_type == "plugin.alpha"
              and captured[2].entity_type == "plugin.beta"
              and captured[2].items[1].shared_id == "row-1"
        "#,
        )
        .eval()
        .expect("scheduled context-sharing snapshot script should evaluate");

    assert!(
        result,
        "scheduled snapshots should reuse one plugin context across timer ticks"
    );
}

#[test]
fn schedule_snapshots_to_falls_back_when_timer_is_unavailable() {
    let (lua, eb) = new_eb_lua();
    lua.globals().set("EB", eb).unwrap();

    let result: bool = lua
        .load(
            r#"
            timer = nil

            EB.register("session", {
              id_field = "session_uuid",
              all = function(_context)
                return { { session_uuid = "sess-1" } }
              end,
            })

            local captured = {}
            local client = {
              send = function(_, frame)
                captured[#captured + 1] = frame
              end,
            }

            EB.schedule_snapshots_to(client, "sub-1", { types = { "session" } })
            return #captured == 1 and captured[1].entity_type == "session"
        "#,
        )
        .eval()
        .expect("timer fallback snapshot script should evaluate");

    assert!(
        result,
        "scheduled snapshots should preserve synchronous behavior when timer is unavailable"
    );
}

#[test]
fn schedule_snapshots_to_cancels_after_send_failure() {
    let (lua, eb) = new_eb_lua();
    lua.globals().set("EB", eb).unwrap();

    let result: bool = lua
        .load(
            r#"
            local queue = {}
            timer = {
              after = function(_delay, fn)
                queue[#queue + 1] = fn
                return "timer-" .. tostring(#queue)
              end,
            }

            local calls = 0
            EB.register("session", {
              id_field = "session_uuid",
              all = function(_context)
                calls = calls + 1
                return { { session_uuid = "sess-1" } }
              end,
            })
            EB.register("workspace", {
              id_field = "workspace_id",
              all = function(_context)
                calls = calls + 1
                return { { workspace_id = "ws-1" } }
              end,
            })

            local client = {
              subscriptions = { ["sub-1"] = { channel = "hub" } },
              send = function()
                error("closed transport")
              end,
            }

            EB.schedule_snapshots_to(client, "sub-1", {
              types = { "session", "workspace" },
            })

            local ok = pcall(queue[1])
            return ok and calls == 1 and #queue == 1
        "#,
        )
        .eval()
        .expect("send failure snapshot script should evaluate");

    assert!(
        result,
        "scheduled snapshots should catch send errors and stop the batch explicitly"
    );
}

#[test]
fn send_snapshots_to_shares_context_across_entity_providers() {
    let (lua, eb) = new_eb_lua();

    lua.globals().set("EB", eb).unwrap();
    let result: bool = lua
        .load(
            r#"
            EB.register("session", {
              id_field = "session_uuid",
              all = function(context)
                context["session.info"] = {
                  { session_uuid = "sess-1" },
                }
                return context["session.info"]
              end,
            })
            EB.register("session1.alpha", {
              id_field = "id",
              owner_plugin = "session1",
              all = function(context)
                context.session_info = { { session_uuid = "corrupted" } }
                context["session.info"] = { { session_uuid = "corrupted" } }
                context.plugin_marker = "shared"
                return { { id = "plugin-1" } }
              end,
            })
            EB.register("session1.beta", {
              id_field = "id",
              owner_plugin = "session1",
              all = function(context)
                return { { id = "plugin-2", marker = context.plugin_marker } }
              end,
            })
            EB.register("session2.plugin", {
              id_field = "id",
              owner_plugin = "session2",
              all = function(context)
                return { { id = "plugin-3", marker = context.plugin_marker or "clean" } }
              end,
            })
            EB.register("session_action", {
              id_field = "id",
              all = function(context)
                local session = context["session.info"] and context["session.info"][1]
                return {
                  {
                    id = session.session_uuid .. ":close",
                    session_uuid = session.session_uuid,
                  },
                }
              end,
            })

            local captured = {}
            local client = {
              send = function(_, frame)
                captured[#captured + 1] = frame
              end,
            }
            EB.send_snapshots_to(client, "sub-1", { types = {
              "session",
              "session1.alpha",
              "session1.beta",
              "session2.plugin",
              "session_action",
            } })
            local by_type = {}
            for _, frame in ipairs(captured) do
              by_type[frame.entity_type] = frame
            end
            return by_type.session.items[1].session_uuid == "sess-1"
              and by_type.session_action.items[1].id == "sess-1:close"
              and by_type["session1.beta"].items[1].marker == "shared"
              and by_type["session2.plugin"].items[1].marker == "clean"
        "#,
        )
        .eval()
        .expect("context-sharing snapshot script should evaluate");

    assert!(
        result,
        "entity providers should share one request-local context"
    );
}

#[test]
fn send_snapshots_to_core_scope_skips_plugin_types() {
    let (lua, eb) = new_eb_lua();
    register_session_type(&lua, &eb);

    let register: Function = eb.get("register").unwrap();
    let plugin_opts: Table = lua.create_table().unwrap();
    plugin_opts.set("id_field", "id").unwrap();
    plugin_opts.set("owner_plugin", "kanban").unwrap();
    let plugin_all: Function = lua
        .create_function(|lua, ()| {
            let arr = lua.create_table()?;
            let board = lua.create_table()?;
            board.set("id", "board-1")?;
            board.set("name", "Roadmap")?;
            arr.set(1, board)?;
            Ok(arr)
        })
        .unwrap();
    plugin_opts.set("all", plugin_all).unwrap();
    register.call::<()>(("kanban.board", plugin_opts)).unwrap();

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

    let opts: Table = lua.create_table().unwrap();
    opts.set("scope", "core").unwrap();
    let send_snapshots_to: Function = eb.get("send_snapshots_to").unwrap();
    send_snapshots_to
        .call::<()>((client, "sub-core", opts))
        .unwrap();

    let frames = frames_as_json(&lua, &captured);
    assert_eq!(frames.len(), 1, "core scope must omit plugin snapshots");
    assert_eq!(frames[0]["entity_type"], json!("session"));
    assert_eq!(frames[0]["subscriptionId"], json!("sub-core"));
}

#[test]
fn send_snapshots_to_owner_plugin_scope_skips_other_plugin_types() {
    let (lua, eb) = new_eb_lua();

    let register: Function = eb.get("register").unwrap();
    for (entity_type, owner_plugin, label) in [
        ("kanban.board", "kanban", "Roadmap"),
        ("pipelines.ticket", "pipelines", "Reload polish"),
    ] {
        let opts: Table = lua.create_table().unwrap();
        opts.set("id_field", "id").unwrap();
        opts.set("owner_plugin", owner_plugin).unwrap();
        let label = label.to_string();
        let all_fn: Function = lua
            .create_function(move |lua, ()| {
                let arr = lua.create_table()?;
                let item = lua.create_table()?;
                item.set("id", label.to_lowercase().replace(' ', "-"))?;
                item.set("label", label.as_str())?;
                arr.set(1, item)?;
                Ok(arr)
            })
            .unwrap();
        opts.set("all", all_fn).unwrap();
        register.call::<()>((entity_type, opts)).unwrap();
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

    let opts: Table = lua.create_table().unwrap();
    opts.set("owner_plugin", "pipelines").unwrap();
    let send_snapshots_to: Function = eb.get("send_snapshots_to").unwrap();
    send_snapshots_to
        .call::<()>((client, "sub-plugin", opts))
        .unwrap();

    let frames = frames_as_json(&lua, &captured);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["entity_type"], json!("pipelines.ticket"));
    assert_eq!(frames[0]["subscriptionId"], json!("sub-plugin"));
    assert_eq!(frames[0]["items"][0]["label"], json!("Reload polish"));
}

#[test]
fn plugin_snapshot_drops_items_without_string_id() {
    let (lua, eb) = new_eb_lua();
    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "id").unwrap();
    opts.set("owner_plugin", "kanban").unwrap();
    let all_fn: Function = lua
        .create_function(|lua, ()| {
            let arr = lua.create_table()?;
            let valid = lua.create_table()?;
            valid.set("id", "board-1")?;
            valid.set("name", "Roadmap")?;
            arr.set(1, valid)?;

            let missing = lua.create_table()?;
            missing.set("name", "No id")?;
            arr.set(2, missing)?;

            let numeric = lua.create_table()?;
            numeric.set("id", 99)?;
            numeric.set("name", "Numeric id")?;
            arr.set(3, numeric)?;

            arr.set(4, "not an entity")?;
            Ok(arr)
        })
        .unwrap();
    opts.set("all", all_fn).unwrap();
    register.call::<()>(("kanban.board", opts)).unwrap();

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
    send_snapshots_to
        .call::<()>((client, "sub-plugin"))
        .unwrap();

    let frames = frames_as_json(&lua, &captured);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["entity_type"], json!("kanban.board"));
    let items = frames[0]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "only records with string id survive");
    assert_eq!(items[0]["id"], json!("board-1"));
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
fn hub_upsert_ships_even_when_server_id_is_nil() {
    // Regression: connections.lua hub_recovery_state listener used to guard
    // `if not hub.server_id() then return end` before EB.upsert("hub", ...).
    // Fresh / unpaired hubs have no botster_id yet, so the guard dropped every
    // recovery transition and the React sidebar stayed stuck on "offline".
    //
    // The Rust event source (cli/src/hub/mod.rs:935) already populates the
    // payload's `hub_id` via Hub::server_hub_id(), which falls back to the
    // local hub_identifier when no botster_id is assigned. So as long as the
    // listener forwards the incoming payload (instead of overriding it with a
    // fresh hub.server_id() call), EB.upsert always has a stable id.
    //
    // This test pins the contract: an upsert with `hub_id` supplied in the
    // payload ships exactly as expected, regardless of server_id state.
    let (lua, eb) = new_eb_lua();
    let register: Function = eb.get("register").unwrap();
    let opts: Table = lua.create_table().unwrap();
    opts.set("id_field", "hub_id").unwrap();
    let all_fn: Function = lua
        .create_function(|lua, ()| Ok(lua.create_table()?))
        .unwrap();
    opts.set("all", all_fn).unwrap();
    register
        .call::<()>(("hub", opts))
        .expect("register hub entity");
    let frames = install_capturing_broadcaster(&lua, &eb);

    // Mimic the listener's payload after merging the Rust-supplied event.
    // hub_id comes from server_hub_id() fallback; state from the transition.
    let upsert: Function = eb.get("upsert").unwrap();
    let payload: Table = lua.create_table().unwrap();
    payload
        .set("hub_id", "local-hub-identifier-deadbeef")
        .unwrap();
    payload.set("state", "ready").unwrap();
    upsert.call::<()>(("hub", payload)).unwrap();

    let captured = frames_as_json(&lua, &frames);
    assert_eq!(captured.len(), 1, "hub upsert must emit one frame");
    let frame = &captured[0];
    assert_eq!(frame["type"], json!("entity_upsert"));
    assert_eq!(frame["entity_type"], json!("hub"));
    assert_eq!(
        frame["id"],
        json!("local-hub-identifier-deadbeef"),
        "id should resolve from payload.hub_id even when no botster_id is set"
    );
    assert_eq!(frame["entity"]["state"], json!("ready"));
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
