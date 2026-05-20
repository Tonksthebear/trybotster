//! Rust-hosted Lua tests for recovered session canonicality.

use mlua::Lua;
use tempfile::TempDir;

fn create_lua_vm(data_dir: &std::path::Path, repo_root: &std::path::Path) -> Lua {
    let lua = Lua::new();

    botster::lua::primitives::fs::register(&lua).expect("fs register");
    botster::lua::primitives::json::register(&lua).expect("json register");
    botster::lua::primitives::log::register(&lua).expect("log register");
    botster::lua::primitives::hook_timeout::register(&lua).expect("hook timeout register");

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
        _G.events = {{
          on = function(event, callback)
            if event == "sessions_discovered" then
              _G.session_recovery_callback = callback
            end
            return "test-subscription"
          end,
          off = function() return true end,
        }}
        _G.config = {{
          data_dir = function() return "{data_dir}" end,
          find_available_port = function() return 46000 end,
          terminfo = function() return {{ term = "xterm-256color" }} end,
        }}
        _G.hub = {{
          spawn_session = function(opts, session_uuid)
            return {{
              session_uuid = session_uuid,
              dimensions = function() return opts.rows or 24, opts.cols or 80 end,
              port = function() return opts.port end,
            }}
          end,
          register_session = function() return 1 end,
          unregister_session = function() return true end,
          connect_session = function()
            _G.test_connect_session_count = (_G.test_connect_session_count or 0) + 1
            return {{
              dimensions = function() return 24, 80 end,
              kill = function() end,
            }}
          end,
          update_manifest_workspaces = function() return true end,
          server_id = function() return "hub-test" end,
          hub_id = function() return "hub-test" end,
          exe_dir = function() return "" end,
        }}
        _G.hub_discovery = {{
          socket_path = function() return "{data_dir}/hub.sock" end,
          manifest_path = function() return "{data_dir}/hub-manifest.json" end,
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
              repo_root = path,
              supports_worktrees = true,
              is_git_repo = true,
            }}
          end,
        }}
        _G.test_worktree_delete_count = 0
        _G.worktree = {{
          find = function() return nil end,
          find_for_root = function() return nil end,
          list = function() return {{}} end,
          delete = function() _G.test_worktree_delete_count = _G.test_worktree_delete_count + 1 end,
        }}
        _G.test_write_counts = {{ workspace = 0, session = 0 }}
        package.preload["lib.workspace_store"] = function()
          return {{
            init_dir = function() return true end,
            ensure_workspace = function()
              return "ws-1", {{
                id = "ws-1",
                name = "Workspace",
                status = "active",
                metadata = {{}},
                created_at = "2026-05-18T00:00:00Z",
              }}, false, nil
            end,
            read_workspace = function()
              return {{
                id = "ws-1",
                name = "Workspace",
                status = "active",
                metadata = {{}},
                created_at = "2026-05-18T00:00:00Z",
              }}
            end,
            scan_recoverable_sessions = function() return {{}} end,
            write_workspace = function()
              _G.test_write_counts.workspace = _G.test_write_counts.workspace + 1
              return true
            end,
            write_session = function()
              _G.test_write_counts.session = _G.test_write_counts.session + 1
              return true
            end,
            refresh_workspace_status = function() return true end,
            append_event = function() return true end,
          }}
        end
    "#,
        data_dir = data_dir.to_str().unwrap(),
        repo_root = repo_root.to_str().unwrap(),
    ))
    .exec()
    .expect("stub globals");

    lua
}

#[test]
fn freshly_created_sessions_are_canonical() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let repo_root = dir.path().join("repo");
    let worktree_path = dir.path().join("worktree");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&repo_root).unwrap();
    std::fs::create_dir_all(&worktree_path).unwrap();
    std::fs::write(worktree_path.join(".git"), "gitdir: /tmp/example").unwrap();

    let lua = create_lua_vm(&data_dir, &repo_root);

    let created: bool = lua
        .load(format!(
            r#"
            local Agent = require("lib.agent")
            local session = Agent.new({{
              repo = "owner/repo",
              branch_name = "feature",
              worktree_path = "{worktree_path}",
              session = {{ name = "codex", command = "bash" }},
              target_id = "target-1",
              target_path = "{repo_root}",
              target_repo = "owner/repo",
            }})

            local info = session:info()
            return info.recovery_source == "created"
              and info.canonical == true
        "#,
            repo_root = repo_root.to_str().unwrap(),
            worktree_path = worktree_path.to_str().unwrap(),
        ))
        .eval()
        .expect("created session canonicality should evaluate");

    assert!(
        created,
        "fresh sessions should be canonical with source=created"
    );
}

#[test]
fn manifest_recovered_sessions_are_canonical_and_sync_manifests() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let repo_root = dir.path().join("repo");
    let worktree_path = dir.path().join("worktree");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&repo_root).unwrap();
    std::fs::create_dir_all(&worktree_path).unwrap();
    std::fs::write(worktree_path.join(".git"), "gitdir: /tmp/example").unwrap();

    let lua = create_lua_vm(&data_dir, &repo_root);

    let canonical: bool = lua
        .load(format!(
            r#"
            local Agent = require("lib.agent")
            local session = Agent.from_recovery({{
              session_uuid = "sess-manifest",
              session_type = "agent",
              session_name = "codex",
              repo = "owner/repo",
              target_id = "target-1",
              target_path = "{repo_root}",
              target_repo = "owner/repo",
              branch_name = "feature",
              worktree_path = "{worktree_path}",
              workspace_id = "ws-1",
              workspace_name = "Workspace",
              recovery_source = "manifest",
              canonical = true,
              handle = {{
                dimensions = function() return 24, 80 end,
              }},
            }})

            session:_sync_workspace_manifest()
            session:_sync_session_manifest()

            local info = session:info()
            return info.recovery_source == "manifest"
              and info.canonical == true
              and _G.test_write_counts.workspace == 1
              and _G.test_write_counts.session == 1
        "#,
            repo_root = repo_root.to_str().unwrap(),
            worktree_path = worktree_path.to_str().unwrap(),
        ))
        .eval()
        .expect("manifest recovery canonicality should evaluate");

    assert!(
        canonical,
        "manifest recovery should be canonical and writable"
    );
}

#[test]
fn process_identity_recovery_skips_non_admitted_spawn_targets() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let repo_root = dir.path().join("repo");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&repo_root).unwrap();

    let lua = create_lua_vm(&data_dir, &repo_root);

    let skipped: bool = lua
        .load(
            r#"
            _G.spawn_targets.get = function() return nil end
            _G.test_connect_session_count = 0

            require("handlers.session_recovery")
            assert(_G.session_recovery_callback ~= nil, "session recovery callback registered")

            _G.session_recovery_callback({
              sockets = {
                {
                  session_uuid = "sess-other-hub",
                  socket_path = "/tmp/botster/sessions/sess-other-hub.sock",
                  recovery_identity = {
                    schema_version = 1,
                    session_uuid = "sess-other-hub",
                    session_type = "agent",
                    session_name = "codex",
                    target_id = "unadmitted-target",
                    target_path = "/unadmitted/repo",
                  },
                },
              },
            })

            return _G.test_connect_session_count == 0
        "#,
        )
        .eval()
        .expect("process identity admission guard should evaluate");

    assert!(
        skipped,
        "session recovery should not connect to sockets whose target is not admitted"
    );
}

#[test]
fn process_identity_recovered_sessions_are_noncanonical_and_do_not_sync_manifests() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let repo_root = dir.path().join("repo");
    let worktree_path = dir.path().join("worktree");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&repo_root).unwrap();
    std::fs::create_dir_all(&worktree_path).unwrap();
    std::fs::write(worktree_path.join(".git"), "gitdir: /tmp/example").unwrap();

    let lua = create_lua_vm(&data_dir, &repo_root);

    let noncanonical: bool = lua
        .load(format!(
            r#"
            local Agent = require("lib.agent")
            local session = Agent.from_recovery({{
              session_uuid = "sess-process-identity",
              session_type = "agent",
              session_name = "codex",
              repo = "owner/repo",
              target_id = "target-1",
              target_path = "{repo_root}",
              target_repo = "owner/repo",
              branch_name = "feature",
              worktree_path = "{worktree_path}",
              workspace_id = "ws-1",
              workspace_name = "Workspace",
              recovery_source = "process_identity",
              canonical = false,
              handle = {{
                dimensions = function() return 24, 80 end,
              }},
            }})

            local moved, move_err = session:move_to_workspace({{ workspace_name = "Fresh Workspace" }})
            session:_sync_workspace_manifest()
            session:_sync_session_manifest()
            session:close(true)

            local info = session:info()
            return info.recovery_source == "process_identity"
              and info.canonical == false
              and moved == nil
              and string.find(tostring(move_err), "non%-canonical") ~= nil
              and _G.test_write_counts.workspace == 0
              and _G.test_write_counts.session == 0
              and _G.test_worktree_delete_count == 0
        "#,
            repo_root = repo_root.to_str().unwrap(),
            worktree_path = worktree_path.to_str().unwrap(),
        ))
        .eval()
        .expect("process identity recovery canonicality should evaluate");

    assert!(
        noncanonical,
        "process identity recovery should be non-canonical and read-only to manifests"
    );
}
