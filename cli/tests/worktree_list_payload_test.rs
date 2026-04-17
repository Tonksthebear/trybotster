//! Rust-hosted Lua tests for reusable worktree picker payloads.
//!
//! The create-session flow should list detachable worktrees even when they
//! already have active sessions attached, because Botster supports multiple
//! sessions per worktree.

use mlua::Lua;

fn create_lua_vm() -> Lua {
    let lua = Lua::new();

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

    lua
}

fn load_payload(lua: &Lua) {
    lua.load(r#"payload = require("lib.worktree_list_payload")"#)
        .exec()
        .expect("load worktree list payload");
}

#[test]
fn active_worktree_sessions_are_included_even_when_git_list_is_empty() {
    let lua = create_lua_vm();
    load_payload(&lua);

    let includes_active: bool = lua
        .load(
            r#"
            local target = {
              target_id = "target-1",
              target_path = "/repo",
              target_repo = "owner/repo",
            }
            local listed = {}
            local sessions = {
              {
                id = "sess-1",
                session_uuid = "sess-1",
                target_id = "target-1",
                target_path = "/repo",
                target_repo = "owner/repo",
                branch_name = "feature-a",
                worktree_path = "/worktrees/feature-a",
                in_worktree = true,
                metadata = {},
              },
            }

            local merged = payload.build(target, listed, sessions)
            return #merged == 1
              and merged[1].path == "/worktrees/feature-a"
              and merged[1].branch == "feature-a"
              and merged[1].active_sessions == 1
        "#,
        )
        .eval()
        .expect("active session merge should evaluate");

    assert!(
        includes_active,
        "active worktree sessions should appear in the picker even without a git-listed entry"
    );
}

#[test]
fn git_list_entries_are_retained_and_count_active_sessions() {
    let lua = create_lua_vm();
    load_payload(&lua);

    let counted: bool = lua
        .load(
            r#"
            local target = {
              target_id = "target-1",
              target_path = "/repo",
              target_repo = "owner/repo",
            }
            local listed = {
              { path = "/worktrees/feature-a", branch = "feature-a" },
            }
            local sessions = {
              {
                session_uuid = "sess-1",
                target_id = "target-1",
                target_path = "/repo",
                target_repo = "owner/repo",
                branch_name = "feature-a",
                worktree_path = "/worktrees/feature-a",
                in_worktree = true,
                metadata = {},
              },
              {
                session_uuid = "sess-2",
                target_id = "target-1",
                target_path = "/repo",
                target_repo = "owner/repo",
                branch_name = "feature-a",
                worktree_path = "/worktrees/feature-a",
                in_worktree = true,
                metadata = {},
              },
            }

            local merged = payload.build(target, listed, sessions)
            return #merged == 1
              and merged[1].path == "/worktrees/feature-a"
              and merged[1].branch == "feature-a"
              and merged[1].active_sessions == 2
        "#,
        )
        .eval()
        .expect("active session counting should evaluate");

    assert!(
        counted,
        "git-listed worktrees should stay visible and report how many active sessions reuse them"
    );
}

#[test]
fn unrelated_or_non_worktree_sessions_are_ignored() {
    let lua = create_lua_vm();
    load_payload(&lua);

    let ignored: bool = lua
        .load(
            r#"
            local target = {
              target_id = "target-1",
              target_path = "/repo",
              target_repo = "owner/repo",
            }
            local listed = {}
            local sessions = {
              {
                session_uuid = "sess-main",
                target_id = "target-1",
                target_path = "/repo",
                target_repo = "owner/repo",
                branch_name = "main",
                worktree_path = "/repo",
                in_worktree = false,
                metadata = {},
              },
              {
                session_uuid = "sess-other-target",
                target_id = "target-2",
                target_path = "/other",
                target_repo = "owner/other",
                branch_name = "feature-b",
                worktree_path = "/worktrees/feature-b",
                in_worktree = true,
                metadata = {},
              },
              {
                session_uuid = "sess-system",
                target_id = "target-1",
                target_path = "/repo",
                target_repo = "owner/repo",
                branch_name = "feature-c",
                worktree_path = "/worktrees/feature-c",
                in_worktree = true,
                metadata = { system_session = true },
              },
            }

            local merged = payload.build(target, listed, sessions)
            return #merged == 0
        "#,
        )
        .eval()
        .expect("ignored session filtering should evaluate");

    assert!(
        ignored,
        "main checkout, other targets, and system sessions should not pollute reusable worktree choices"
    );
}
