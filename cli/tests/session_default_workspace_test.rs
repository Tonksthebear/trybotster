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
          on = function() return "test-subscription" end,
          off = function() return true end,
        }}
        _G.config = {{
          data_dir = function() return "{data_dir}" end,
          find_available_port = function() return 46000 end,
        }}
        _G.hub = {{
          last_spawn_config = nil,
          spawn_session = function(_, session_uuid)
            _G.hub.last_spawn_config = _
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
          find = function() return nil end,
          find_for_root = function() return nil end,
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
fn workspace_accessories_inherit_agent_resolved_default_workspace() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join(".botster-dev");
    let repo_root = dir.path().join("repo");
    let worktree_path = dir.path().join("feature-accessory-worktree");

    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&repo_root).unwrap();
    std::fs::create_dir_all(&worktree_path).unwrap();
    std::fs::write(worktree_path.join(".git"), "gitdir: /tmp/example").unwrap();

    let agent_dir = repo_root.join(".botster-dev/agents/codex");
    let accessory_dir = repo_root.join(".botster-dev/accessories/rails-server");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(&accessory_dir).unwrap();
    std::fs::write(agent_dir.join("initialization"), "echo agent").unwrap();
    std::fs::write(accessory_dir.join("initialization"), "echo accessory").unwrap();

    let lua = create_lua_vm(&data_dir, &repo_root);

    let inherited: bool = lua
        .load(format!(
            r#"
            worktree.find_for_root = function(_, _) return "{worktree_path}" end

            local handlers = require("handlers.agents")
            handlers.handle_create_agent(
              "feature-accessory",
              nil,
              nil,
              nil,
              "codex",
              {{ workspace_config = {{ accessories = {{ "rails-server" }} }} }},
              {{
                target_id = "target-1",
                target_path = "{repo_root}",
                target_repo = "owner/repo",
              }}
            )

            local Agent = require("lib.agent")
            local sessions = Agent.list()
            local primary, accessory
            for _, session in ipairs(sessions) do
              if session.session_type == "agent" then primary = session end
              if session.session_type == "accessory" then accessory = session end
            end

            return primary ~= nil
              and accessory ~= nil
              and primary._workspace_id ~= nil
              and primary._workspace_id ~= ""
              and accessory._workspace_id == primary._workspace_id
              and accessory._workspace_name == primary._workspace_name
              and not fs.exists("{data_dir}/workspaces/nil")
        "#,
            data_dir = data_dir.to_str().unwrap(),
            repo_root = repo_root.to_str().unwrap(),
            worktree_path = worktree_path.to_str().unwrap(),
        ))
        .eval()
        .expect("workspace accessory inheritance should evaluate");

    assert!(
        inherited,
        "workspace accessories should use the workspace resolved by the primary agent"
    );
}

#[test]
fn session_definition_dir_is_persisted_for_context_lookup() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let repo_root = dir.path().join("repo");
    let worktree_path = dir.path().join("feature-b-worktree");
    let definition_dir = dir.path().join("config/agents/codex");

    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&repo_root).unwrap();
    std::fs::create_dir_all(&worktree_path).unwrap();
    std::fs::create_dir_all(&definition_dir).unwrap();
    std::fs::write(worktree_path.join(".git"), "gitdir: /tmp/example").unwrap();

    let lua = create_lua_vm(&data_dir, &repo_root);

    let exposed: bool = lua
        .load(format!(
            r#"
            local Agent = require("lib.agent")
            local first = Agent.new({{
              repo = "owner/repo",
              branch_name = "feature-b",
              worktree_path = "{worktree_path}",
              session = {{
                name = "codex",
                command = "bash",
                definition_dir = "{definition_dir}",
              }},
              target_id = "target-1",
              target_path = "{repo_root}",
              target_repo = "owner/repo",
            }})

            return Agent.get(first.session_uuid):info().session_dir == "{definition_dir}"
        "#,
            definition_dir = definition_dir.to_str().unwrap(),
            repo_root = repo_root.to_str().unwrap(),
            worktree_path = worktree_path.to_str().unwrap(),
        ))
        .eval()
        .expect("definition dir context should evaluate");

    assert!(
        exposed,
        "session context should expose the selected definition directory"
    );
}

#[test]
fn before_pty_spawn_applies_returned_spawn_config_changes() {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let repo_root = dir.path().join("repo");
    let worktree_path = dir.path().join("feature-c-worktree");

    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&repo_root).unwrap();
    std::fs::create_dir_all(&worktree_path).unwrap();
    std::fs::write(worktree_path.join(".git"), "gitdir: /tmp/example").unwrap();

    let lua = create_lua_vm(&data_dir, &repo_root);

    let applied: bool = lua
        .load(format!(
            r#"
            hooks.intercept("before_pty_spawn", "test.mutate-spawn", function(ctx)
              ctx.command = "zsh"
              ctx.args = {{ "-lc", "echo changed" }}
              ctx.cwd = "{repo_root}"
              ctx.env.TEST_BEFORE_PTY_SPAWN = "applied"
              ctx.init_commands = {{ "echo init changed" }}
              return ctx
            end)

            local Agent = require("lib.agent")
            Agent.new({{
              repo = "owner/repo",
              branch_name = "feature-c",
              worktree_path = "{worktree_path}",
              session = {{ name = "codex", command = "bash" }},
              target_id = "target-1",
              target_path = "{repo_root}",
              target_repo = "owner/repo",
            }})

            local cfg = hub.last_spawn_config
            return cfg.command == "zsh"
              and cfg.args[1] == "-lc"
              and cfg.args[2] == "echo changed"
              and cfg.cwd == "{repo_root}"
              and cfg.env.TEST_BEFORE_PTY_SPAWN == "applied"
              and cfg.init_commands[1] == "echo init changed"
        "#,
            repo_root = repo_root.to_str().unwrap(),
            worktree_path = worktree_path.to_str().unwrap(),
        ))
        .eval()
        .expect("before_pty_spawn mutation should evaluate");

    assert!(
        applied,
        "before_pty_spawn should apply returned spawn configuration changes"
    );
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
              session = {{ name = "codex", command = "bash" }},
              target_id = "target-1",
              target_path = "{repo_root}",
              target_repo = "owner/repo",
            }})

            local second = Agent.new({{
              repo = "owner/repo",
              branch_name = "feature-a",
              worktree_path = "{worktree_path}",
              session = {{ name = "codex", command = "bash" }},
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
