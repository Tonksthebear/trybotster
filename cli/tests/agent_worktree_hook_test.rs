//! Regression tests for agent/worktree lifecycle hooks.
//!
//! Run with: `cd cli && ./test.sh -- agent_worktree_hook`.

#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test-code brevity: focused Lua integration assertions"
)]

use std::path::PathBuf;

use mlua::Lua;

fn cli_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn new_agents_lua_vm() -> Lua {
    let lua = Lua::new();
    let lua_base = cli_manifest_dir().join("lua");
    let base = lua_base.to_string_lossy();

    lua.load(format!(
        r#"
        package.path = "{base}/?.lua;{base}/?/init.lua;" .. package.path

        _G.__events = {{}}
        _G.__notifications = {{}}
        _G.__order = {{}}

        local function push_order(value)
            table.insert(_G.__order, value)
        end

        _G.log = {{
            info = function(_) end,
            warn = function(_) end,
            error = function(_) end,
            debug = function(_) end,
        }}

        _G.config = {{
            data_dir = function() return "/tmp/botster-test-data" end,
        }}

        _G.hooks = {{
            notify = function(event, payload)
                table.insert(_G.__notifications, {{ event = event, payload = payload }})
                if event == "worktree_created" then
                    push_order("worktree_created")
                end
            end,
            call = function(_, params) return params end,
        }}

        _G.events = {{
            on = function(event, fn)
                _G.__events[event] = fn
                return event .. "-sub"
            end,
        }}

        _G.spawn_targets = {{
            inspect = function(path)
                return {{
                    is_git_repo = true,
                    repo_root = path,
                }}
            end,
        }}

        _G.worktree = {{
            repo_root = function() return "/repos/current-runtime" end,
            find_for_root = function(_, _) return nil end,
            create_for_root = function(root, branch)
                return root .. "/.worktrees/" .. branch
            end,
        }}

        package.preload["lib.target_context"] = function()
            local M = {{}}
            function M.resolve(opts)
                local source = opts.explicit or opts.metadata or {{}}
                local target_path = source.target_path
                if opts.require_target_path and not target_path then
                    return nil, "missing target_path"
                end
                return {{
                    target_id = source.target_id or "target-jupiter",
                    target_path = target_path,
                    target_repo = source.target_repo or "acme/jupiter",
                }}
            end
            function M.default_repo_label(target)
                return target.target_repo or target.target_path
            end
            function M.matches(_, _) return true end
            function M.with_metadata(metadata, target)
                local result = {{}}
                for k, v in pairs(metadata or {{}}) do result[k] = v end
                result.target_id = target.target_id
                result.target_path = target.target_path
                result.target_repo = target.target_repo
                return result
            end
            return M
        end

        package.preload["lib.config_resolver"] = function()
            return {{
                list_agents = function()
                    return {{ "codex" }}
                end,
                resolve_all = function()
                    return {{
                        agents = {{
                            codex = {{
                                initialization = "echo ready",
                                dir = "/tmp/codex-agent",
                            }},
                        }},
                        accessories = {{}},
                    }}
                end,
            }}
        end

        package.preload["lib.agent"] = function()
            return {{
                count = function() return 0 end,
                new = function(config)
                    push_order("agent_new")
                    return {{
                        info = function()
                            return {{
                                session_uuid = "agent-1",
                                branch_name = config.branch_name,
                                worktree_path = config.worktree_path,
                            }}
                        end,
                    }}
                end,
            }}
        end

        package.preload["lib.accessory"] = function()
            return {{ new = function() error("unexpected accessory spawn") end }}
        end

        package.preload["lib.session_close_policy"] = function()
            return {{ evaluate = function() return {{}} end }}
        end

        _G.agents_handler = require("handlers.agents")
        "#,
    ))
    .exec()
    .expect("load agents handler");

    lua
}

#[test]
fn create_for_root_notifies_worktree_created_before_agent_spawn() {
    let lua = new_agents_lua_vm();

    lua.load(
        r#"
        agents_handler.handle_create_agent(
            "botster-issue-77",
            "set up Jupiter",
            nil,
            nil,
            "codex",
            { source = "test" },
            {
                target_id = "target-jupiter",
                target_path = "/repos/jupiter",
                target_repo = "acme/jupiter",
            }
        )
        "#,
    )
    .exec()
    .expect("create agent through external target path");

    let (first, second, worktree_created_count, path, branch, repo): (
        String,
        String,
        usize,
        String,
        String,
        String,
    ) = lua
        .load(
            r#"
            local hook_payload
            local worktree_created_count = 0
            for _, note in ipairs(_G.__notifications) do
                if note.event == "worktree_created" then
                    worktree_created_count = worktree_created_count + 1
                    hook_payload = note.payload
                end
            end

            return
                _G.__order[1],
                _G.__order[2],
                worktree_created_count,
                hook_payload.path,
                hook_payload.branch,
                hook_payload.repo
            "#,
        )
        .eval()
        .expect("read hook assertions");

    assert_eq!(first, "worktree_created");
    assert_eq!(second, "agent_new");
    assert_eq!(worktree_created_count, 1);
    assert_eq!(path, "/repos/jupiter/.worktrees/botster-issue-77");
    assert_eq!(branch, "botster-issue-77");
    assert_eq!(repo, "acme/jupiter");
}
