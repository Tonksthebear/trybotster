//! Rust-hosted Lua tests for client-facing session payload decoration.
//!
//! These tests exercise the hub-side presenter/policy seam that decides whether
//! clients should offer destructive worktree cleanup when closing a session.

use mlua::Lua;

/// Create a minimal Lua VM with enough globals/modules to load the client
/// payload and close-policy modules.
fn create_lua_vm() -> Lua {
    let lua = Lua::new();

    botster::lua::primitives::fs::register(&lua).expect("fs register");
    botster::lua::primitives::json::register(&lua).expect("json register");
    botster::lua::primitives::log::register(&lua).expect("log register");

    let lua_dir = std::env::current_dir()
        .unwrap()
        .join("lua")
        .to_str()
        .unwrap()
        .to_string();
    lua.load(format!(
        r#"package.path = "{lua_dir}/?.lua;{lua_dir}/?/init.lua;" .. package.path"#
    ))
    .exec()
    .expect("set package.path");

    lua.load(
        r#"
        _G.hooks = require("hub.hooks")
        _G.config = {
          data_dir = function() return nil end,
        }
        _G.worktree = { list = function() return {} end }
    "#,
    )
    .exec()
    .expect("stub globals");

    lua
}

fn load_modules(lua: &Lua) {
    lua.load(
        r#"
        payload = require("lib.client_session_payload")
        policy = require("lib.session_close_policy")
    "#,
    )
    .exec()
    .expect("load client payload modules");
}

#[test]
fn single_worktree_session_exposes_delete_worktree_action() {
    let lua = create_lua_vm();
    load_modules(&lua);

    let can_delete: bool = lua
        .load(
            r#"
            local sessions = {
              {
                id = "sess-1",
                session_uuid = "sess-1",
                workspace_id = "ws-1",
                worktree_path = "/tmp/ws-1",
                in_worktree = true,
                metadata = {},
              },
            }

            local rendered = payload.build_many(sessions)
            return rendered[1].close_actions.can_delete_worktree == true
              and rendered[1].close_actions.delete_worktree_reason == nil
              and rendered[1].close_actions.other_active_sessions == 0
        "#,
        )
        .eval()
        .expect("single-session payload should evaluate");

    assert!(
        can_delete,
        "single worktree session should expose delete-worktree capability"
    );
}

#[test]
fn second_visible_session_in_same_workspace_blocks_delete_worktree_action() {
    let lua = create_lua_vm();
    load_modules(&lua);

    let blocked: bool = lua
        .load(
            r#"
            local sessions = {
              {
                id = "sess-1",
                session_uuid = "sess-1",
                workspace_id = "ws-1",
                worktree_path = "/tmp/ws-1",
                in_worktree = true,
                metadata = {},
              },
              {
                id = "sess-2",
                session_uuid = "sess-2",
                workspace_id = "ws-1",
                worktree_path = "/tmp/ws-1-helper",
                in_worktree = true,
                metadata = {},
              },
            }

            local rendered = payload.build_many(sessions)
            local close = rendered[1].close_actions
            return close.can_delete_worktree == false
              and close.delete_worktree_reason == "other_sessions_active"
              and close.other_active_sessions == 1
        "#,
        )
        .eval()
        .expect("multi-session payload should evaluate");

    assert!(
        blocked,
        "another visible session in the workspace should block worktree deletion"
    );
}

#[test]
fn hidden_system_sessions_do_not_block_delete_worktree_action() {
    let lua = create_lua_vm();
    load_modules(&lua);

    let ignored: bool = lua
        .load(
            r#"
            local sessions = {
              {
                id = "sess-1",
                session_uuid = "sess-1",
                workspace_id = "ws-1",
                worktree_path = "/tmp/ws-1",
                in_worktree = true,
                metadata = {},
              },
              {
                id = "sys-1",
                session_uuid = "sys-1",
                workspace_id = "ws-1",
                worktree_path = "/tmp/ws-1",
                in_worktree = true,
                metadata = {
                  system_session = true,
                  system_kind = "hosted_preview_connector",
                },
                system_session = true,
              },
            }

            local rendered = payload.build_many(sessions)
            local close = rendered[1].close_actions
            return close.can_delete_worktree == true
              and close.delete_worktree_reason == nil
              and close.other_active_sessions == 0
        "#,
        )
        .eval()
        .expect("system-session payload should evaluate");

    assert!(
        ignored,
        "system sessions should be ignored when computing delete-worktree capability"
    );
}

#[test]
fn same_worktree_blocks_delete_even_when_workspace_ids_differ() {
    let lua = create_lua_vm();
    load_modules(&lua);

    let blocked: bool = lua
        .load(
            r#"
            local sessions = {
              {
                id = "sess-1",
                session_uuid = "sess-1",
                workspace_id = "ws-1",
                worktree_path = "/tmp/shared-worktree",
                in_worktree = true,
                metadata = {},
              },
              {
                id = "sess-2",
                session_uuid = "sess-2",
                workspace_id = "ws-2",
                worktree_path = "/tmp/shared-worktree",
                in_worktree = true,
                metadata = {},
              },
            }

            local rendered = payload.build_many(sessions)
            local close = rendered[1].close_actions
            return close.can_delete_worktree == false
              and close.delete_worktree_reason == "other_sessions_active"
              and close.other_active_sessions == 1
        "#,
        )
        .eval()
        .expect("shared-worktree payload should evaluate");

    assert!(
        blocked,
        "another session on the same worktree must block worktree deletion even if workspace IDs differ"
    );
}
