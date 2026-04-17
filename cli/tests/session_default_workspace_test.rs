//! Rust-hosted Lua tests for default workspace assignment during Session._init.
//!
//! Leaving workspace selection on "Default" should reuse an active workspace
//! for the same branch/target instead of creating duplicate unnamed workspaces.

use mlua::Lua;
use tempfile::TempDir;

fn create_lua_vm(data_dir: &std::path::Path, repo_root: &std::path::Path) -> Lua {
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

    lua.load(format!(
        r#"
        _G.hooks = require("hub.hooks")
        _G.config = {{
          data_dir = function() return "{data_dir}" end,
          find_available_port = function() return 46000 end,
        }}
        _G.hub = {{
          spawn_session = function(_, session_uuid)
            return {{ session_uuid = session_uuid }}
          end,
          register_session = function() return 1 end,
          update_manifest_workspaces = function() return true end,
          server_id = function() return "hub-test" end,
          hub_id = function() return "hub-test" end,
          exe_dir = function() return "" end,
        }}
        _G.hub_discovery = {{
          socket_path = function() return "{data_dir}/hub.sock" end,
          manifest_path = function() return "{data_dir}/hub-manifest.json" end,
        }}
        _G.worktree = {{
          list = function() return {{}} end,
        }}
        _G.spawn_targets = {{
          get = function(target_id)
            return {{
              id = target_id,
              path = "{repo_root}",
              enabled = true,
            }}
          end,
          inspect = function(path)
            return {{
              repo_name = "owner/repo",
              repo_root = "{repo_root}",
              supports_worktrees = true,
              is_git_repo = true,
            }}
          end,
        }}
    "#,
        data_dir = data_dir.to_str().unwrap(),
        repo_root = repo_root.to_str().unwrap(),
    ))
    .exec()
    .expect("stub globals");

    lua
}

#[test]
fn default_workspace_reuses_active_workspace_for_same_branch_and_target() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let repo_root = dir.path().join("repo");
    let worktree_path = dir.path().join("feature-a-worktree");

    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&repo_root).unwrap();
    std::fs::create_dir_all(&worktree_path).unwrap();
    std::fs::write(worktree_path.join(".git"), "gitdir: /tmp/example").unwrap();

    let lua = create_lua_vm(&data_dir, &repo_root);

    let reused: bool = lua
        .load(format!(
            r#"
            local Agent = require("lib.agent")
            local dd = "{data_dir}"

            local first = Agent.new({{
              repo = "owner/repo",
              branch_name = "feature-a",
              worktree_path = "{worktree_path}",
              session = {{ name = "claude", command = "bash" }},
              target_id = "target-1",
              target_path = "{repo_root}",
              target_repo = "owner/repo",
            }})

            local second = Agent.new({{
              repo = "owner/repo",
              branch_name = "feature-a",
              worktree_path = "{worktree_path}",
              session = {{ name = "claude", command = "bash" }},
              target_id = "target-1",
              target_path = "{repo_root}",
              target_repo = "owner/repo",
            }})

            local entries = fs.list_dir(dd .. "/workspaces") or {{}}
            return first._workspace_id == second._workspace_id
              and first._workspace_name == "feature-a"
              and second._workspace_name == "feature-a"
              and #entries == 1
        "#,
            data_dir = data_dir.to_str().unwrap(),
            repo_root = repo_root.to_str().unwrap(),
            worktree_path = worktree_path.to_str().unwrap(),
        ))
        .eval()
        .expect("default workspace reuse should evaluate");

    assert!(
        reused,
        "default workspace assignment should reuse one active workspace for the same branch and target"
    );
}
