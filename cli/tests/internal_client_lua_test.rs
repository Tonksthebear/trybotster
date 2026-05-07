//! Lua tests for internal command ingress through lib.client.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    reason = "test-code brevity"
)]

use std::path::PathBuf;

use botster::lua::primitives::log;
use mlua::Lua;

fn lua_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua")
}

fn new_lua() -> Lua {
    let lua = Lua::new();
    log::register(&lua).expect("register log");
    botster::lua::primitives::hook_timeout::register(&lua).expect("register hook timeout");

    let dir = lua_src_dir();
    lua.load(format!(
        r#"
        package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path
        hooks = require("hub.hooks")
        "#,
        dir = dir.display()
    ))
    .exec()
    .expect("configure lua");

    lua
}

#[test]
fn internal_dispatch_enters_client_command_hooks() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local commands = require("lib.commands")
            local internal = require("lib.internal_client")

            local seen = {}
            hooks.intercept("before_hub_command", "test.before_hub", function(command)
              seen.before_hub = command.type
              return command
            end)
            hooks.intercept("before_command", "test.before_command", function(ctx)
              seen.before_command = ctx.type
              seen.peer_id = ctx.client.peer_id
              return ctx
            end)
            hooks.on("after_hub_command", "test.after_command", function(ctx)
              seen.after_command = ctx.command
              seen.success = ctx.success
            end)

            commands.register("demo_internal", function(client, sub_id, command)
              seen.handler = command.payload
              client:send({
                subscriptionId = sub_id,
                type = "demo:response",
                payload = command.payload,
              })
            end)

            local result = internal.dispatch("audit", {
              type = "demo_internal",
              payload = "ok",
            })

            assert(seen.before_hub == "demo_internal")
            assert(seen.before_command == "demo_internal")
            assert(seen.after_command == "demo_internal")
            assert(seen.success == true)
            assert(seen.handler == "ok")
            assert(seen.peer_id == "internal:audit")
            assert(#result.frames == 1)
            assert(result.frames[1].type == "demo:response")
            return "ok"
            "#,
        )
        .eval()
        .expect("internal dispatch should flow through client.lua");

    assert_eq!(result, "ok");
}

#[test]
fn ui_action_dispatch_emits_correlated_success_result() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local action = require("lib.action")
            require("handlers.commands")

            action._reset_for_tests()
            action.on("demo.save", "test.demo", function(_, _)
              return action.HANDLED
            end)

            local result = require("lib.internal_client").dispatch("test", {
              type = "ui_action",
              target_surface = "demo_surface",
              action_request_id = "req-1",
              envelope = { id = "demo.save", payload = { value = "ok" } },
            })

            assert(#result.frames == 1)
            local frame = result.frames[1]
            assert(frame.type == "ui_action_result")
            assert(frame.v == 1)
            assert(frame.target_surface == "demo_surface")
            assert(frame.action_request_id == "req-1")
            assert(frame.action_id == "demo.save")
            assert(frame.ok == true)
            assert(frame.handled == true)
            assert(frame.via == "handler")
            return "ok"
            "#,
        )
        .eval()
        .expect("ui_action should emit result frame");

    assert_eq!(result, "ok");
}

#[test]
fn ui_action_invalid_envelope_emits_correlated_error_result() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            require("handlers.commands")

            local result = require("lib.internal_client").dispatch("test", {
              type = "ui_action",
              target_surface = "demo_surface",
              action_request_id = "req-bad",
              envelope = "bad",
            })

            assert(#result.frames == 1)
            local frame = result.frames[1]
            assert(frame.type == "ui_action_result")
            assert(frame.v == 1)
            assert(frame.action_request_id == "req-bad")
            assert(frame.ok == false)
            assert(frame.handled == false)
            assert(frame.error == "Invalid UI action envelope.")
            return "ok"
            "#,
        )
        .eval()
        .expect("invalid ui_action should emit error result frame");

    assert_eq!(result, "ok");
}

#[test]
fn create_agent_creates_even_when_matching_agent_exists() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local spawned = 0

            config = { data_dir = function() return "/tmp/botster-test" end }
            spawn_targets = {
              get = function(id)
                if id == "target-1" then
                  return { id = "target-1", path = "/repo", enabled = true }
                end
              end,
              inspect = function(_)
                return { repo_name = "demo/repo", is_git_repo = true, repo_root = "/repo" }
              end,
            }

            local existing = {
              session_uuid = "sess-existing",
              metadata = { issue_number = 42 },
              session = {
                send_message = function(_, text)
                  error("create_agent should not notify existing sessions: " .. tostring(text))
                end,
              },
            }

            package.loaded["lib.agent"] = {
              list = function() return { existing } end,
              find_by_workspace = function(name)
                if name == "demo/repo#42" then return { existing } end
                return {}
              end,
              find_by_meta = function(_, _) return {} end,
            }
            package.loaded["handlers.agents"] = {
              handle_create_agent = function(_, _, _, _, _, metadata)
                spawned = spawned + 1
                assert(metadata.request_id == "req-create")
                assert(metadata.assignment_id == "assign-create")
                assert(metadata.label == "Workflow worker")
                return { session_uuid = "sess-new" }
              end,
            }

            require("handlers.commands")
            local result = require("lib.internal_client").dispatch("test", {
              type = "create_agent",
              request_id = "req-create",
              assignment_id = "assign-create",
              issue_or_branch = "42",
              prompt = "Please look at this",
              label = "Workflow worker",
              target_id = "target-1",
              metadata = {
                issue_number = 42,
                workspace = "demo/repo#42",
              },
            })

            assert(spawned == 1)
            assert(result.frames[1].type == "command_response")
            assert(result.frames[1].ok == true)
            assert(result.frames[1].session_uuid == "sess-new")
            assert(result.frames[1].request_id == "req-create")
            assert(result.frames[1].assignment_id == "assign-create")
            return "ok"
            "#,
        )
        .eval()
        .expect("create_agent should create a new session");

    assert_eq!(result, "ok");
}

#[test]
fn hub_create_agent_table_api_preserves_plugin_metadata() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path

            hub = {{ hub_id = function() return "hub-local" end }}
            local dispatched = nil

            package.loaded["hub.state"] = {{
              class = function(_)
                local cls = {{}}
                cls.__index = cls
                return cls
              end,
              get = function(_, default) return default end,
            }}
            package.loaded["lib.agent"] = {{
              list = function() return {{}} end,
              get = function() return nil end,
              all_info = function() return {{}} end,
            }}
            package.loaded["lib.internal_client"] = {{
              dispatch = function(_, command)
                dispatched = command
                return {{
                  frames = {{
                    {{
                      type = "command_response",
                      request_id = command.request_id,
                      ok = true,
                      status = "pending",
                      assignment_id = command.metadata.assignment_id,
                    }},
                  }},
                }}
              end,
            }}

            local Hub = require("lib.hub")
            local result = Hub.get():create_agent({{
              target_id = "target-1",
              target_path = "/repo",
              target_repo = "owner/repo",
              agent_name = "codex",
              issue_or_branch = "workflow-branch",
              label = "Workflow worker",
              prompt = "Implement the workflow",
              workspace_id = "ws-1",
              workspace_name = "Workflow",
              request_id = "req-api",
              assignment_id = "assign-api",
              metadata = {{
                owner_plugin = "workflow",
                visibility = "plugin",
                surface = "workflow.queue",
                ticket_id = "TKT-1",
                run_id = "run-1",
                gate_id = "gate-1",
                role = "implementer",
                custom_field = "kept",
              }},
            }})

            assert(result.status == "pending")
            assert(result.request_id == "req-api")
            assert(result.assignment_id == "assign-api")
            assert(dispatched.type == "create_agent")
            assert(dispatched.issue_or_branch == "workflow-branch")
            assert(dispatched.label == "Workflow worker")
            assert(dispatched.prompt == "Implement the workflow")
            assert(dispatched.agent_name == "codex")
            assert(dispatched.workspace_id == "ws-1")
            assert(dispatched.workspace_name == "Workflow")
            assert(dispatched.target_id == "target-1")
            assert(dispatched.target_path == "/repo")
            assert(dispatched.target_repo == "owner/repo")
            assert(dispatched.request_id == "req-api")
            assert(dispatched.metadata.request_id == "req-api")
            assert(dispatched.metadata.assignment_id == "assign-api")
            assert(dispatched.metadata.label == "Workflow worker")
            assert(dispatched.metadata.owner_plugin == "workflow")
            assert(dispatched.metadata.visibility == "plugin")
            assert(dispatched.metadata.surface == "workflow.queue")
            assert(dispatched.metadata.ticket_id == "TKT-1")
            assert(dispatched.metadata.run_id == "run-1")
            assert(dispatched.metadata.gate_id == "gate-1")
            assert(dispatched.metadata.role == "implementer")
            assert(dispatched.metadata.custom_field == "kept")
            assert(dispatched.metadata.workspace_id == "ws-1")
            assert(dispatched.metadata.workspace == "Workflow")
            return "ok"
            "#,
            dir = dir.display()
        ))
        .eval()
        .expect("table-style hub.create_agent should preserve plugin metadata");

    assert_eq!(result, "ok");
}

#[test]
fn hub_create_agent_mints_request_id_and_lists_owned_sessions() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path

            hub = {{ hub_id = function() return "hub-local" end }}
            local dispatched = nil
            local owned_session = {{
              session_uuid = "sess-owned",
              owner_plugin = "workflow",
              label = "Owned worker",
              metadata = {{ owner_plugin = "workflow", assignment_id = "assign-owned" }},
              status = "running",
            }}
            local other_session = {{
              session_uuid = "sess-other",
              owner_plugin = "other",
              metadata = {{ owner_plugin = "other" }},
              status = "running",
            }}

            package.loaded["hub.state"] = {{
              class = function(_)
                local cls = {{}}
                cls.__index = cls
                return cls
              end,
              get = function(_, default) return default end,
            }}
            package.loaded["lib.agent"] = {{
              list = function() return {{ owned_session, other_session }} end,
              get = function() return nil end,
              all_info = function() return {{}} end,
            }}
            package.loaded["lib.internal_client"] = {{
              dispatch = function(_, command)
                dispatched = command
                return {{
                  frames = {{
                    {{
                      type = "command_response",
                      request_id = command.request_id,
                      ok = true,
                      status = "pending",
                    }},
                  }},
                }}
              end,
            }}

            local Hub = require("lib.hub")
            local created = Hub.get():create_agent({{
              target_id = "target-1",
              target_path = "/repo",
              agent_name = "codex",
              metadata = {{ owner_plugin = "workflow" }},
            }})
            local owned = Hub.get():list_owned_sessions("workflow")

            assert(type(dispatched.request_id) == "string")
            assert(dispatched.request_id ~= "")
            assert(dispatched.request_id:match("^msg_hub%-loca_"))
            assert(dispatched.metadata.request_id == dispatched.request_id)
            assert(created.request_id == dispatched.request_id)
            assert(#owned == 1)
            assert(owned[1].session_uuid == "sess-owned")
            assert(owned[1].label == "Owned worker")
            assert(owned[1].metadata.assignment_id == "assign-owned")
            assert(owned[1].status == "running")
            return "ok"
            "#,
            dir = dir.display()
        ))
        .eval()
        .expect("hub create_agent should mint request_id and list owned sessions");

    assert_eq!(result, "ok");
}

#[test]
fn hub_create_agent_sync_success_includes_correlation() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path

            hub = {{ hub_id = function() return "hub-local" end }}
            local created_session = {{
              session_uuid = "sess-created",
              metadata = {{ request_id = "req-sync", assignment_id = "assign-sync" }},
            }}

            package.loaded["hub.state"] = {{
              class = function(_)
                local cls = {{}}
                cls.__index = cls
                return cls
              end,
              get = function(_, default) return default end,
            }}
            package.loaded["lib.agent"] = {{
              list = function() return {{}} end,
              get = function(id)
                if id == "sess-created" then return created_session end
              end,
              all_info = function() return {{}} end,
            }}
            package.loaded["lib.client_session_payload"] = {{
              build = function(session)
                return {{
                  session_uuid = session.session_uuid,
                  metadata = session.metadata,
                }}
              end,
            }}
            package.loaded["lib.internal_client"] = {{
              dispatch = function(_, command)
                return {{
                  frames = {{
                    {{
                      type = "command_response",
                      request_id = command.request_id,
                      ok = true,
                      session_uuid = "sess-created",
                      assignment_id = command.metadata.assignment_id,
                    }},
                  }},
                }}
              end,
            }}

            local Hub = require("lib.hub")
            local result = Hub.get():create_agent({{
              target_id = "target-1",
              target_path = "/repo",
              request_id = "req-sync",
              assignment_id = "assign-sync",
              metadata = {{ owner_plugin = "workflow" }},
            }})

            assert(result.session_uuid == "sess-created")
            assert(result.request_id == "req-sync")
            assert(result.assignment_id == "assign-sync")
            assert(result.metadata.request_id == "req-sync")
            return "ok"
            "#,
            dir = dir.display()
        ))
        .eval()
        .expect("sync create_agent result should include correlation");

    assert_eq!(result, "ok");
}

#[test]
fn hub_list_owned_sessions_remote_returns_sessions_array() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path

            hub = {{ hub_id = function() return "hub-local" end }}
            events = {{ emit = function() end }}
            hub_discovery = {{
              socket_path = function(hub_id)
                if hub_id == "hub-remote" then return "/tmp/hub-remote.sock" end
              end,
            }}
            hub_client = {{
              connect = function(_) return "conn-remote" end,
              request = function(conn_id, command)
                assert(conn_id == "conn-remote")
                assert(command.type == "hub_command")
                assert(command.command.type == "list_owned_sessions")
                assert(command.command.owner_plugin == "workflow")
                return {{
                  result = {{
                    ok = true,
                    sessions = {{
                      {{
                        session_uuid = "sess-remote",
                        label = "Remote worker",
                        metadata = {{ owner_plugin = "workflow" }},
                        status = "running",
                      }},
                    }},
                  }},
                }}
              end,
            }}

            package.loaded["hub.state"] = {{
              class = function(_)
                local cls = {{}}
                cls.__index = cls
                return cls
              end,
              get = function(_, default) return default end,
            }}
            package.loaded["lib.agent"] = {{
              list = function() return {{}} end,
              all_info = function() return {{}} end,
            }}
            package.loaded["lib.internal_client"] = {{
              dispatch = function() error("remote path should use hub_client") end,
            }}

            local Hub = require("lib.hub")
            local sessions = Hub.get("hub-remote"):list_owned_sessions("workflow")

            assert(#sessions == 1)
            assert(sessions[1].session_uuid == "sess-remote")
            assert(sessions[1].label == "Remote worker")
            assert(sessions[1].metadata.owner_plugin == "workflow")
            assert(sessions[1].status == "running")
            return "ok"
            "#,
            dir = dir.display()
        ))
        .eval()
        .expect("remote list_owned_sessions should return sessions array");

    assert_eq!(result, "ok");
}

#[test]
fn hub_get_defaults_to_parent_hub_inside_plugin_worker() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path

            _plugin_worker_parent_hub_id = "hub-parent"
            hub = {{ hub_id = function() return "plugin-worker:vault" end }}
            events = {{ emit = function() end }}

            local seen = {{}}
            plugin_worker_parent_hub = {{
              request = function(command)
                seen.command = command
                return {{ result = {{ ok = true, status = "queued", request_id = command.command.request_id }} }}
              end,
            }}

            package.loaded["hub.state"] = {{
              class = function(_)
                local cls = {{}}
                cls.__index = cls
                return cls
              end,
              get = function(_, default) return default end,
            }}
            package.loaded["lib.agent"] = {{
              list = function() error("worker Hub.get() must not use local Agent") end,
              all_info = function() return {{}} end,
            }}
            package.loaded["lib.internal_client"] = {{
              dispatch = function() error("worker Hub.get() must not dispatch locally") end,
            }}

            local Hub = require("lib.hub")
            local result = Hub.get():create_agent({{
              issue_or_branch = "main",
              prompt = "process inbox",
              agent_name = "codex",
              target_id = "target-1",
              target_path = "/repo",
              target_repo = "repo",
              metadata = {{ owner_plugin = "knowledge-inbox-pipeline" }},
            }})

            assert(seen.command.type == "hub_command")
            assert(seen.command.command.type == "create_agent")
            assert(seen.command.command.metadata.owner_plugin == "knowledge-inbox-pipeline")
            assert(seen.command.command.target_id == "target-1")
            assert(result.status == "queued")
            return "ok"
            "#,
            dir = dir.display()
        ))
        .eval()
        .expect("plugin worker Hub.get should proxy to parent hub");

    assert_eq!(result, "ok");
}

#[test]
fn worker_boundary_hub_mutations_proxy_to_parent_command_queue() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path

            _plugin_worker_parent_hub_id = "hub-parent"
            hub = {{ hub_id = function() return "plugin-worker:workflow" end }}
            events = {{ emit = function() end }}

            local seen = {{}}
            plugin_worker_parent_hub = {{
              request = function(command)
                assert(command.type == "hub_command")
                seen[#seen + 1] = command.command
                return {{ result = {{ ok = true, status = "queued", request_id = command.command.request_id }} }}
              end,
            }}

            package.loaded["hub.state"] = {{
              class = function(_)
                local cls = {{}}
                cls.__index = cls
                return cls
              end,
              get = function(_, default) return default end,
            }}
            package.loaded["lib.agent"] = {{
              list = function() error("worker Hub.get() must not use local Agent") end,
              get = function() error("worker Hub.get() must not use local Agent") end,
              all_info = function() return {{}} end,
            }}
            package.loaded["lib.internal_client"] = {{
              dispatch = function() error("worker Hub.get() must not dispatch locally") end,
            }}

            local Hub = require("lib.hub")
            local h = Hub.get()
            h:update_session("sess-1", {{ label = "Updated" }})
            h:move_agent_workspace("sess-1", "workspace-2")
            h:rename_workspace("workspace-2", "Workspace 2")
            h:entity_upsert("workflow.item", {{ id = "item-1" }}, {{ owner_plugin = "workflow" }})
            h:delete_agent("sess-1", false)

            assert(#seen == 5)
            assert(seen[1].type == "update_session")
            assert(seen[1].agent_id == "sess-1")
            assert(seen[2].type == "move_agent_workspace")
            assert(seen[2].agent_id == "sess-1")
            assert(seen[2].workspace_id == "workspace-2")
            assert(seen[3].type == "rename_workspace")
            assert(seen[3].workspace_id == "workspace-2")
            assert(seen[4].type == "plugin_entity_publish")
            assert(seen[4].op == "upsert")
            assert(seen[4].owner_plugin == "workflow")
            assert(seen[5].type == "delete_agent")
            assert(seen[5].agent_id == "sess-1")
            return "ok"
            "#,
            dir = dir.display()
        ))
        .eval()
        .expect("plugin worker hub mutation calls should proxy through orchestration queue");

    assert_eq!(result, "ok");
}

#[test]
fn worker_parent_orchestration_returns_before_dispatch() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path

            hub = {{ hub_id = function() return "hub-parent" end }}
            events = {{ emit = function() end }}

            local scheduled = nil
            timer = {{
              after = function(delay, callback)
                assert(delay == 0)
                scheduled = callback
                return "timer-1"
              end,
            }}

            local expected = {{
              create_agent = "req-async",
              delete_agent = "req-delete",
              update_session = "req-update",
              move_agent_workspace = "req-move",
              rename_workspace = "req-rename",
              plugin_entity_publish = "req-entity",
              create_accessory = "req-hub-command",
            }}
            local expected_order = {{
              "create_agent",
              "delete_agent",
              "update_session",
              "move_agent_workspace",
              "rename_workspace",
              "plugin_entity_publish",
              "create_accessory",
            }}
            local dispatched = 0
            package.loaded["hub.state"] = {{
              class = function(_)
                local cls = {{}}
                cls.__index = cls
                return cls
              end,
              get = function(_, default) return default end,
            }}
            package.loaded["lib.agent"] = {{
              list = function() return {{}} end,
              all_info = function() return {{}} end,
            }}
            package.loaded["lib.internal_client"] = {{
              dispatch = function(_, command)
                dispatched = dispatched + 1
                local expected_type = expected_order[dispatched]
                assert(command.type == expected_type, "expected " .. tostring(expected_type) .. ", got " .. tostring(command.type))
                assert(command.request_id == expected[expected_type])
                return {{
                  frames = {{
                    {{ type = "command_response", request_id = command.request_id, ok = true, status = "pending" }},
                  }},
                }}
              end,
            }}

            local commands = require("lib.commands")
            commands.register("create_agent", function() end)
            commands.register("delete_agent", function() end)
            commands.register("update_session", function() end)
            commands.register("move_agent_workspace", function() end)
            commands.register("rename_workspace", function() end)
            commands.register("plugin_entity_publish", function() end)
            commands.register("create_accessory", function() end)

            local Hub = require("lib.hub")
            local WorkerParentCommandQueue = require("lib.worker_parent_command_queue")
            assert(WorkerParentCommandQueue.is_queued_command({{ type = "create_agent" }}))
            assert(WorkerParentCommandQueue.is_queued_command({{ type = "delete_agent" }}))
            assert(WorkerParentCommandQueue.is_queued_command({{ type = "update_session" }}))
            assert(WorkerParentCommandQueue.is_queued_command({{ type = "move_agent_workspace" }}))
            assert(WorkerParentCommandQueue.is_queued_command({{ type = "rename_workspace" }}))
            assert(WorkerParentCommandQueue.is_queued_command({{ type = "plugin_entity_publish" }}))
            assert(not WorkerParentCommandQueue.is_queued_command({{ type = "not_registered" }}))

            local response = Hub._handle_worker_parent_request({{
              type = "hub_command",
              command = {{
                type = "create_agent",
                request_id = "req-async",
                metadata = {{ assignment_id = "assign-async" }},
              }},
            }})

            assert(dispatched == 0)
            assert(response.result.status == "queued")
            assert(response.result.request_id == "req-async")
            assert(response.result.assignment_id == "assign-async")
            assert(type(scheduled) == "function")
            scheduled()
            assert(dispatched == 1)
            scheduled = nil

            local delete_response = Hub._handle_worker_parent_request({{
              type = "hub_command",
              command = {{
                type = "delete_agent",
                request_id = "req-delete",
                session_uuid = "sess-delete",
              }},
            }})

            assert(dispatched == 1)
            assert(delete_response.result.status == "queued")
            assert(delete_response.result.request_id == "req-delete")
            assert(type(scheduled) == "function")
            scheduled()
            assert(dispatched == 2)

            local update_response = Hub._handle_worker_parent_request({{
              type = "hub_command",
              command = {{
                type = "update_session",
                request_id = "req-update",
                session_uuid = "sess-update",
                label = "updated",
              }},
            }})
            assert(dispatched == 2)
            assert(update_response.result.status == "queued")
            scheduled()
            assert(dispatched == 3)
            scheduled = nil

            local move_response = Hub._handle_worker_parent_request({{
              type = "hub_command",
              command = {{
                type = "move_agent_workspace",
                request_id = "req-move",
                session_uuid = "sess-move",
                workspace_id = "workspace-1",
              }},
            }})
            assert(dispatched == 3)
            assert(move_response.result.status == "queued")
            scheduled()
            assert(dispatched == 4)
            scheduled = nil

            local rename_response = Hub._handle_worker_parent_request({{
              type = "hub_command",
              command = {{
                type = "rename_workspace",
                request_id = "req-rename",
                workspace_id = "workspace-1",
                new_name = "Workspace 1",
              }},
            }})
            assert(dispatched == 4)
            assert(rename_response.result.status == "queued")
            scheduled()
            assert(dispatched == 5)
            scheduled = nil

            local entity_response = Hub._handle_worker_parent_request({{
              type = "hub_command",
              command = {{
                type = "plugin_entity_publish",
                request_id = "req-entity",
                op = "upsert",
                entity_type = "project-pipelines.ticket",
                entity = {{ id = "ticket-1" }},
                owner_plugin = "project-pipelines",
              }},
            }})
            assert(dispatched == 5)
            assert(entity_response.result.status == "queued")
            scheduled()
            assert(dispatched == 6)
            scheduled = nil

            local hub_command_response = Hub._handle_worker_parent_request({{
              type = "hub_command",
              command = {{
                type = "create_accessory",
                request_id = "req-hub-command",
                accessory_name = "server",
              }},
            }})
            assert(dispatched == 6)
            assert(hub_command_response.result.status == "queued")
            scheduled()
            assert(dispatched == 7)
            scheduled = nil

            local rejected = Hub._handle_worker_parent_request({{
              type = "hub_command",
              command = {{ type = "not_registered", request_id = "req-read" }},
            }})
            assert(rejected.error:match("unsupported worker parent hub command"))

            local stale_direct_create = Hub._handle_worker_parent_request({{
              type = "create_agent",
              request_id = "req-stale-direct",
            }})
            assert(stale_direct_create.error:match("unsupported worker parent hub request"))
            return "ok"
            "#,
            dir = dir.display()
        ))
        .eval()
        .expect("worker parent orchestration command should be queued before dispatch");

    assert_eq!(result, "ok");
}

#[test]
fn agent_session_lookups_proxy_to_parent_hub_inside_plugin_worker() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path

            _plugin_worker_parent_hub_id = "hub-parent"
            hub = {{ hub_id = function() return "plugin-worker:vault" end }}
            events = {{ emit = function() end }}
            local request_count = 0
            plugin_worker_parent_hub = {{
              request = function(command)
                request_count = request_count + 1
                if command.type == "get_agent_list" then
                  return {{
                    result = {{
                      {{
                        id = "sess-worker",
                        session_uuid = "sess-worker",
                        session_type = "agent",
                        label = "Knowledge Worker",
                        workspace_name = "Vault",
                        target_path = "/Users/jasonconigliari/knowledge",
                        metadata = {{ owner_plugin = "knowledge-inbox-pipeline" }},
                        status = "running",
                      }},
                    }},
                  }}
                end
                error("unexpected command " .. tostring(command.type))
              end,
            }}
            package.loaded["hub.state"] = {{
              class = function(name)
                local cls = {{}}
                cls.__index = cls
                return cls
              end,
              get = function(_, default) return default end,
            }}

            local Agent = require("lib.agent")
            local Session = require("lib.session")
            local list = Agent.list()
            local found = Agent.get("sess-worker")
            local by_meta = Session.find_by_meta("owner_plugin", "knowledge-inbox-pipeline")

            assert(#list == 1)
            assert(found and found:info().session_uuid == "sess-worker")
            assert(#by_meta == 1)
            assert(request_count == 1, "expected cached parent list lookup, got " .. tostring(request_count))
            return "ok"
            "#,
            dir = dir.display()
        ))
        .eval()
        .expect("worker Agent/Session lookups should proxy to parent hub");

    assert_eq!(result, "ok");
}

#[test]
fn hub_proxy_uses_parent_bridge_inside_plugin_worker() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let dir = lua_src_dir();
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path

            _plugin_worker_parent_hub_id = "hub-parent"
            hub = {{ hub_id = function() return "plugin-worker:vault" end }}
            events = {{ emit = function() end }}

            local request_count = 0
            plugin_worker_parent_hub = {{
              request = function(command)
                request_count = request_count + 1
                assert(command.type == "get_agent_list")
                return {{ result = {{ {{ session_uuid = "sess-parent-bridge" }} }} }}
              end,
            }}
            package.loaded["hub.state"] = {{
              class = function(_)
                local cls = {{}}
                cls.__index = cls
                return cls
              end,
              get = function(_, default) return default end,
            }}
            package.loaded["lib.agent"] = {{
              all_info = function() return {{}} end,
            }}
            package.loaded["lib.internal_client"] = {{
              dispatch = function() error("worker Hub.get() must not dispatch locally") end,
            }}

            local Hub = require("lib.hub")
            local sessions = Hub.get():list_agents()
            assert(request_count == 1)
            assert(sessions[1].session_uuid == "sess-parent-bridge")
            return "ok"
            "#,
            dir = dir.display()
        ))
        .eval()
        .expect("worker Hub proxy should use parent bridge");

    assert_eq!(result, "ok");
}

#[test]
fn hub_command_channel_create_agent_webhook_dispatches_spawn_command() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local callback = nil
            local dispatched = {}

            config = { env = function() return "development" end }
            hub = {
              is_offline = function() return false end,
              server_id = function() return "hub-1" end,
              handle_signaling_message = function() error("unexpected signal") end,
            }
            timer = {
              every = function() return "timer-1" end,
              cancel = function() end,
            }
            events = {
              on = function() return "event-1" end,
              off = function() end,
            }
            action_cable = {
              connect = function() return "conn-1" end,
              subscribe = function(_, _, _, cb)
                callback = cb
                return "channel-1"
              end,
              unsubscribe = function() end,
              perform = function() end,
            }
            spawn_targets = {
              get = function(id)
                if id == "target-1" then
                  return { id = "target-1", path = "/repo", enabled = true }
                end
              end,
              inspect = function(_)
                return { repo_name = "owner/repo", is_git_repo = true, repo_root = "/repo" }
              end,
            }

            package.loaded["hub.state"] = {
              get = function() return {} end,
            }
            package.loaded["lib.agent"] = {
              find_by_workspace = function()
                error("HubCommandChannel create_agent ingress should not dedupe by existing session")
              end,
            }
            package.loaded["lib.internal_client"] = {
              dispatch = function(source, command)
                dispatched[#dispatched + 1] = { source = source, command = command }
              end,
            }

            require("handlers.hub_commands")
            assert(type(callback) == "function")
            callback({
              type = "message",
              event_type = "create_agent",
              payload = {
                target_id = "target-1",
                target_path = "/repo",
                target_repo = "owner/repo",
                issue_number = 42,
                prompt = "Please inspect this",
                issue_url = "https://github.com/owner/repo/issues/42",
              },
            }, "channel-1")

            assert(#dispatched == 1)
            assert(dispatched[1].source == "hub_commands")
            assert(dispatched[1].command.type == "create_agent")
            assert(dispatched[1].command.issue_or_branch == "42")
            assert(dispatched[1].command.prompt == "Please inspect this")
            assert(dispatched[1].command.target_id == "target-1")
            assert(dispatched[1].command.metadata.workspace == "owner/repo#42")
            return "ok"
            "#,
        )
        .eval()
        .expect("HubCommandChannel create_agent webhook should dispatch explicit spawn command");

    assert_eq!(result, "ok");
}

#[test]
fn command_failures_are_observable_as_response_frames() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            config = { data_dir = function() return "/tmp/botster-test" end }
            package.loaded["lib.workspace_store"] = {
              rename_workspace = function() return false end,
            }

            require("handlers.commands")
            local result = require("lib.internal_client").dispatch("test", {
              type = "rename_workspace",
              request_id = "req-rename",
              workspace_id = "ws-1",
              new_name = "New",
            })

            assert(result.frames[1].type == "command_response")
            assert(result.frames[1].request_id == "req-rename")
            assert(result.frames[1].ok == false)
            assert(result.frames[1].error == "failed to rename workspace")
            return "ok"
            "#,
        )
        .eval()
        .expect("failure should be returned as command_response");

    assert_eq!(result, "ok");
}

#[test]
fn update_session_requires_an_actual_field() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local session = {
              session_uuid = "sess-existing",
              update = function()
                error("update should not be called")
              end,
            }
            package.loaded["lib.agent"] = {
              get = function(id)
                if id == "sess-existing" then return session end
              end,
            }

            require("handlers.commands")
            local result = require("lib.internal_client").dispatch("test", {
              type = "update_session",
              request_id = "req-update",
              session_uuid = "sess-existing",
            })

            assert(result.frames[1].type == "command_response")
            assert(result.frames[1].request_id == "req-update")
            assert(result.frames[1].ok == false)
            assert(result.frames[1].error == "updatable field is required")
            return "ok"
            "#,
        )
        .eval()
        .expect("update_session should reject no-op updates");

    assert_eq!(result, "ok");
}

#[test]
fn thrown_command_errors_are_observable_as_response_frames() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local commands = require("lib.commands")
            local internal = require("lib.internal_client")

            local after_success = nil
            hooks.on("after_hub_command", "test.after_throwing_command", function(ctx)
              after_success = ctx.success
            end)

            commands.register("throwing_command", function()
              error("boom from handler")
            end)

            local result = internal.dispatch("test", {
              type = "throwing_command",
              request_id = "req-throw",
            })

            assert(result.frames[1].type == "command_response")
            assert(result.frames[1].request_id == "req-throw")
            assert(result.frames[1].ok == false)
            assert(result.frames[1].error:match("boom from handler"))
            assert(after_success == false)
            return "ok"
            "#,
        )
        .eval()
        .expect("throwing handler should return command_response");

    assert_eq!(result, "ok");
}

#[test]
fn internal_dispatch_restores_synthetic_subscription() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local commands = require("lib.commands")
            local internal = require("lib.internal_client")

            commands.register("subscription_probe", function(client, sub_id, _command)
              assert(client.subscriptions[sub_id] ~= nil)
              client:send({
                subscriptionId = sub_id,
                type = "probe_response",
              })
            end)

            local result = internal.dispatch("test-subscriptions", {
              type = "subscription_probe",
            }, {
              subscription_id = "dynamic-subscription",
            })

            assert(result.frames[1].type == "probe_response")
            assert(result.client.subscriptions["dynamic-subscription"] == nil)
            return "ok"
            "#,
        )
        .eval()
        .expect("internal dispatch should clean synthetic subscriptions");

    assert_eq!(result, "ok");
}
