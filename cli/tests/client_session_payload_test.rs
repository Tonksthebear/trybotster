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
        _G.hub = {
          register_session = function() return 1 end,
          unregister_session = function() return true end,
          update_manifest_workspaces = function() return true end,
          hub_id = function() return nil end,
        }
        _G.worktree_delete_count = 0
        _G.worktree = {
          list = function() return {} end,
          delete = function() _G.worktree_delete_count = _G.worktree_delete_count + 1 end,
        }
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
fn recovered_session_info_exposes_canonicality() {
    let lua = create_lua_vm();

    let exposed: bool = lua
        .load(
            r#"
            local Agent = require("lib.agent")

            local manifest = Agent.from_recovery({
              session_uuid = "sess-manifest",
              session_type = "agent",
              session_name = "agent",
              repo = "owner/repo",
              target_id = "target-1",
              target_path = "/tmp/repo",
              target_repo = "owner/repo",
              branch_name = "main",
              worktree_path = "/tmp/worktree",
              workspace_id = "ws-1",
              metadata = {},
              handle = {},
            })

            local degraded = Agent.from_recovery({
              session_uuid = "sess-identity",
              session_type = "agent",
              session_name = "agent",
              repo = "owner/repo",
              target_id = "target-1",
              target_path = "/tmp/repo",
              target_repo = "owner/repo",
              branch_name = "main",
              worktree_path = "/tmp/worktree",
              workspace_id = "ws-1",
              metadata = {},
              recovery_source = "process_identity",
              canonical = false,
              handle = {},
            })

            local manifest_info = manifest:info()
            local degraded_info = degraded:info()
            return manifest_info.recovery_source == "manifest"
              and manifest_info.canonical == true
              and degraded_info.recovery_source == "process_identity"
              and degraded_info.canonical == false
        "#,
        )
        .eval()
        .expect("recovery info canonicality should evaluate");

    assert!(
        exposed,
        "Session:info() should expose manifest/degraded recovery canonicality"
    );
}

#[test]
fn noncanonical_recovered_session_does_not_sync_or_move_workspaces() {
    let lua = create_lua_vm();

    let guarded: bool = lua
        .load(
            r#"
            _G.config.data_dir = function() return "/tmp/botster-test-noncanonical" end
            local Agent = require("lib.agent")
            local ws = require("lib.workspace_store")
            local writes = 0
            ws.write_session = function() writes = writes + 1 end
            ws.write_workspace = function() writes = writes + 1 end
            ws.refresh_workspace_status = function() end

            local degraded = Agent.from_recovery({
              session_uuid = "sess-identity-guard",
              session_type = "agent",
              session_name = "agent",
              repo = "owner/repo",
              target_id = "target-1",
              target_path = "/tmp/repo",
              target_repo = "owner/repo",
              branch_name = "main",
              worktree_path = "/tmp/worktree",
              workspace_id = "ws-stale",
              metadata = {},
              recovery_source = "process_identity",
              canonical = false,
              handle = {},
            })

            degraded:update({ label = "new label" })
            degraded:set_meta("workflow_id", "wf-1")
            local moved, err = degraded:move_to_workspace({ workspace_id = "ws-next" })
            degraded:close(true)

            return writes == 0
              and _G.worktree_delete_count == 0
              and moved == nil
              and type(err) == "string"
              and err:match("non%-canonical") ~= nil
        "#,
        )
        .eval()
        .expect("non-canonical guard should evaluate");

    assert!(
        guarded,
        "non-canonical recovered sessions must not write manifests or move workspaces"
    );
}

#[test]
fn session_entity_payload_preserves_recovery_canonicality() {
    let lua = create_lua_vm();
    load_modules(&lua);

    let preserved: bool = lua
        .load(
            r#"
            local rendered = payload.build({
              id = "sess-identity",
              session_uuid = "sess-identity",
              recovery_source = "process_identity",
              canonical = false,
              metadata = {},
            }, {})

            return rendered.recovery_source == "process_identity"
              and rendered.canonical == false
        "#,
        )
        .eval()
        .expect("entity payload canonicality should evaluate");

    assert!(
        preserved,
        "session entity payload should keep recovery_source and canonical fields"
    );
}

#[test]
fn noncanonical_worktree_sessions_cannot_offer_delete_worktree_action() {
    let lua = create_lua_vm();
    load_modules(&lua);

    let blocked: bool = lua
        .load(
            r#"
            local rendered = payload.build_many({
              {
                id = "sess-identity",
                session_uuid = "sess-identity",
                recovery_source = "process_identity",
                canonical = false,
                workspace_id = "ws-1",
                worktree_path = "/tmp/ws-1",
                in_worktree = true,
                metadata = {},
              },
            })
            local close = rendered[1].close_actions
            return close.can_delete_worktree == false
              and close.delete_worktree_reason == "non_canonical_recovery"
        "#,
        )
        .eval()
        .expect("non-canonical close action policy should evaluate");

    assert!(
        blocked,
        "non-canonical recovered sessions must not offer destructive worktree deletion"
    );
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
                  system_kind = "plugin_connector",
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

#[test]
fn batch_close_actions_match_scalar_policy() {
    let lua = create_lua_vm();
    load_modules(&lua);

    let matches: bool = lua
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
              {
                id = "sess-3",
                session_uuid = "sess-3",
                workspace_id = "ws-1",
                worktree_path = "/tmp/other-worktree",
                in_worktree = true,
                metadata = {},
              },
              {
                id = "sess-4",
                session_uuid = "sess-4",
                workspace_id = "ws-3",
                worktree_path = "/tmp/no-delete",
                in_worktree = false,
                metadata = {},
              },
              {
                id = "sess-5",
                session_uuid = "sess-5",
                worktree_path = "/tmp/no-workspace",
                in_worktree = true,
                metadata = {},
              },
              {
                id = "sess-6",
                session_uuid = "sess-6",
                workspace_id = "",
                worktree_path = "/tmp/empty-workspace-a",
                in_worktree = true,
                metadata = {},
              },
              {
                id = "sess-7",
                session_uuid = "sess-7",
                workspace_id = "",
                worktree_path = "/tmp/empty-workspace-b",
                in_worktree = true,
                metadata = {},
              },
              {
                id = "sess-8",
                session_uuid = "sess-8",
                workspace_id = "ws-empty-path",
                worktree_path = "",
                in_worktree = true,
                metadata = {},
              },
              {
                id = "sess-9",
                session_uuid = "sess-9",
                workspace_id = "ws-empty-path",
                worktree_path = "/tmp/empty-path-peer",
                in_worktree = true,
                metadata = {},
              },
              {
                id = "sys-1",
                session_uuid = "sys-1",
                workspace_id = "ws-1",
                worktree_path = "/tmp/shared-worktree",
                in_worktree = true,
                metadata = { system_session = true },
                system_session = true,
              },
            }

            local batched = policy.close_actions_for_sessions(sessions)
            for _, session in ipairs(sessions) do
              local scalar = policy.close_actions_for_session(session, sessions)
              local batch = batched.by_session_id[session.session_uuid]
              if scalar.can_close ~= batch.can_close
                or scalar.can_delete_worktree ~= batch.can_delete_worktree
                or scalar.delete_worktree_reason ~= batch.delete_worktree_reason
                or scalar.other_active_sessions ~= batch.other_active_sessions then
                return false
              end
            end
            return true
        "#,
        )
        .eval()
        .expect("batch close action comparison should evaluate");

    assert!(
        matches,
        "batched close-action derivation should preserve scalar policy semantics"
    );
}
