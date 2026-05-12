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
            call = function(event, params)
                if event == "before_agent_create" then
                    _G.__before_agent_create = params
                end
                return params
            end,
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
            create_async = function(args)
                _G.__create_async_args = args
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
                    _G.__agent_new_config = config
                    return {{
                        info = function()
                            return {{
                                session_uuid = "agent-1",
                                branch_name = config.branch_name,
                                worktree_path = config.worktree_path,
                                metadata = config.metadata,
                            }}
                        end,
                    }}
                end,
            }}
        end

        package.preload["lib.accessory"] = function()
            return {{
                new = function(config)
                    push_order("accessory_new")
                    _G.__accessory_new_config = config
                    return {{
                        session_uuid = "accessory-1",
                        info = function()
                            return {{
                                session_uuid = "accessory-1",
                                branch_name = config.branch_name,
                                worktree_path = config.worktree_path,
                                metadata = config.metadata,
                                session_type = "accessory",
                            }}
                        end,
                    }}
                end,
            }}
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
fn accessory_creation_can_attach_to_selected_worktree() {
    let lua = new_agents_lua_vm();

    lua.load(
        r#"
        agents_handler.handle_create_accessory(
            nil,
            nil,
            "rails-server",
            nil,
            {
                workspace_id = "ws-1",
                workspace = "Feature A",
            },
            {
                target_id = "target-jupiter",
                target_path = "/repos/jupiter",
                target_repo = "acme/jupiter",
            },
            nil,
            "/repos/jupiter/.worktrees/feature-a",
            "feature-a"
        )
        "#,
    )
    .exec()
    .expect("create accessory in selected worktree");

    let (order, worktree_path, branch_name, workspace_id, workspace_name, target_id): (
        String,
        String,
        String,
        String,
        String,
        String,
    ) = lua
        .load(
            r#"
            local c = _G.__accessory_new_config
            return
                _G.__order[1],
                c.worktree_path,
                c.branch_name,
                c.metadata.workspace_id,
                c.metadata.workspace,
                c.target_id
            "#,
        )
        .eval()
        .expect("read accessory spawn config");

    assert_eq!(order, "accessory_new");
    assert_eq!(worktree_path, "/repos/jupiter/.worktrees/feature-a");
    assert_eq!(branch_name, "feature-a");
    assert_eq!(workspace_id, "ws-1");
    assert_eq!(workspace_name, "Feature A");
    assert_eq!(target_id, "target-jupiter");
}

#[test]
fn external_target_queues_async_worktree_and_spawns_after_created_event() {
    let lua = new_agents_lua_vm();

    lua.load(
        r#"
        agents_handler.handle_create_agent(
            "botster-issue-77",
            "set up Jupiter",
            nil,
            nil,
            "codex",
            {
                source = "test",
                request_id = "req-async-77",
                assignment_id = "assign-77",
                owner_plugin = "workflow",
                visibility = "plugin",
                surface = "workflow.queue",
                ticket_id = "TKT-77",
                run_id = "run-77",
                gate_id = "gate-77",
                role = "implementer",
                label = "Workflow worker",
            },
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

    let (
        queued_branch,
        queued_repo_root,
        queued_request_id,
        queued_assignment_id,
        spawned_before_event,
    ): (String, String, String, String, usize) = lua
        .load(
            r#"
            return
                _G.__create_async_args.branch,
                _G.__create_async_args.repo_root,
                _G.__create_async_args.metadata.request_id,
                _G.__create_async_args.metadata.assignment_id,
                #_G.__order
            "#,
        )
        .eval()
        .expect("read queued async request");

    assert_eq!(queued_branch, "botster-issue-77");
    assert_eq!(queued_repo_root, "/repos/jupiter");
    assert_eq!(queued_request_id, "req-async-77");
    assert_eq!(queued_assignment_id, "assign-77");
    assert_eq!(spawned_before_event, 0);

    lua.load(
        r#"
        _G.__events["worktree_created"]({
            branch = _G.__create_async_args.branch,
            path = "/repos/jupiter/.worktrees/" .. _G.__create_async_args.branch,
            metadata = _G.__create_async_args.metadata,
            prompt = _G.__create_async_args.prompt,
            agent_name = _G.__create_async_args.agent_name,
            client_rows = _G.__create_async_args.client_rows,
            client_cols = _G.__create_async_args.client_cols,
        })
        "#,
    )
    .exec()
    .expect("fire async completion event");

    let (
        first,
        second,
        worktree_created_count,
        path,
        branch,
        repo,
        before_request_id,
        before_assignment_id,
        worktree_request_id,
        worktree_assignment_id,
        agent_metadata_ok,
        agent_created_request_id,
        agent_created_assignment_id,
        agent_config_label,
        after_agent_create_count,
    ): (
        String,
        String,
        usize,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        bool,
        String,
        String,
        String,
        usize,
    ) = lua
        .load(
            r#"
            local hook_payload
            local agent_created_payload
            local after_agent_create_count = 0
            local worktree_created_count = 0
            for _, note in ipairs(_G.__notifications) do
                if note.event == "worktree_created" then
                    worktree_created_count = worktree_created_count + 1
                    hook_payload = note.payload
                elseif note.event == "agent_created" then
                    agent_created_payload = note.payload
                elseif note.event == "after_agent_create" then
                    after_agent_create_count = after_agent_create_count + 1
                end
            end

            local m = _G.__agent_new_config.metadata
            return
                _G.__order[1],
                _G.__order[2],
                worktree_created_count,
                hook_payload.path,
                hook_payload.branch,
                hook_payload.repo,
                _G.__before_agent_create.request_id,
                _G.__before_agent_create.assignment_id,
                hook_payload.request_id,
                hook_payload.assignment_id,
                m.request_id == "req-async-77"
                    and m.assignment_id == "assign-77"
                    and m.owner_plugin == "workflow"
                    and m.visibility == "plugin"
                    and m.surface == "workflow.queue"
                    and m.ticket_id == "TKT-77"
                    and m.run_id == "run-77"
                    and m.gate_id == "gate-77"
                    and m.role == "implementer"
                    and m.target_id == "target-jupiter"
                    and m.target_path == "/repos/jupiter"
                    and m.target_repo == "acme/jupiter",
                agent_created_payload.request_id,
                agent_created_payload.assignment_id,
                _G.__agent_new_config.label,
                after_agent_create_count
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
    assert_eq!(before_request_id, "req-async-77");
    assert_eq!(before_assignment_id, "assign-77");
    assert_eq!(worktree_request_id, "req-async-77");
    assert_eq!(worktree_assignment_id, "assign-77");
    assert!(
        agent_metadata_ok,
        "agent spawn should preserve plugin metadata"
    );
    assert_eq!(agent_created_request_id, "req-async-77");
    assert_eq!(agent_created_assignment_id, "assign-77");
    assert_eq!(agent_config_label, "Workflow worker");
    assert_eq!(after_agent_create_count, 0);
}
