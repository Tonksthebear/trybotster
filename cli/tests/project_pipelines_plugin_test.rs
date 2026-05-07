//! Project Pipelines plugin catalog regression tests.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    reason = "test-code brevity"
)]

use std::path::PathBuf;

use botster::lua::primitives::log;
use mlua::Lua;

fn project_root_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli has repo parent")
        .to_path_buf()
}

#[test]
fn catalog_plugin_project_pipelines_home_render_uses_bounded_notified_session_lookup() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let plugin_dir = project_root_dir().join("catalog/templates/plugins/project-pipelines");
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{plugin_dir}/?.lua;{plugin_dir}/?/init.lua;" .. package.path

            local calls = {{
              agent_list = 0,
              ticket_session_links_for_uuids = 0,
              ticket_session_uuids_by_ticket = 0,
              list_tickets = 0,
            }}

            ui = {{
              bind = function(path) return {{ bind = path }} end,
              action = function(name, payload) return {{ name = name, payload = payload }} end,
              list_item = function(props) return {{ type = "list_item", props = props }} end,
              text = function(props) return {{ type = "text", props = props }} end,
              badge = function(props) return {{ type = "badge", props = props }} end,
              status_dot = function(props) return {{ type = "status_dot", props = props }} end,
              button = function(props) return {{ type = "button", props = props }} end,
              list = function(props) return {{ type = "list", props = props }} end,
              bind_list = function(props) return {{ type = "bind_list", props = props }} end,
              stack = function(props) return {{ type = "stack", props = props }} end,
            }}

            package.loaded["project_pipelines.web.ui"] = {{
              page_header = function(props) return {{ type = "page_header", props = props }} end,
              panel = function(child) return {{ type = "panel", child = child }} end,
              section = function(title, children) return {{ type = "section", title = title, children = children }} end,
              empty = function(title) return {{ type = "empty", title = title }} end,
              badge = function(text, tone) return {{ type = "badge", text = text, tone = tone }} end,
            }}

            package.loaded["project_pipelines.repo"] = {{
              list_runs = function()
                return {{
                  {{ id = "run-1", ticket_id = "ticket-1", pipeline_id = "pipe-1", current_step_id = "step-1", status = "active" }},
                }}
              end,
              get_ticket = function(id) return {{ id = id, title = "Ticket " .. id }} end,
              get_pipeline = function(id) return {{ id = id, name = "Pipeline " .. id }} end,
              get_step = function(id) return {{ id = id, name = "Step " .. id }} end,
              has_open_questions = function() return false end,
              ticket_session_links_for_uuids = function(uuids)
                calls.ticket_session_links_for_uuids = calls.ticket_session_links_for_uuids + 1
                assert(#uuids == 1)
                assert(uuids[1] == "sess-notified")
                return {{
                  ["sess-notified"] = {{ {{ id = "ticket-1", title = "Important ticket" }} }},
                }}
              end,
              ticket_session_uuids_by_ticket = function()
                calls.ticket_session_uuids_by_ticket = calls.ticket_session_uuids_by_ticket + 1
                error("home render must not scan every ticket/session link")
              end,
              list_tickets = function()
                calls.list_tickets = calls.list_tickets + 1
                error("home render must not list every ticket for notification rows")
              end,
            }}

            package.loaded["lib.agent"] = {{
              list = function()
                calls.agent_list = calls.agent_list + 1
                return {{
                  {{ info = function() return {{ session_uuid = "sess-notified", notification = true, label = "Needs attention" }} end }},
                  {{ info = function() return {{ session_uuid = "sess-quiet", notification = false, label = "Quiet" }} end }},
                }}
              end,
            }}

            local home = require("project_pipelines.web.screens.home")
            home.render({{}}, {{ path = function(path) return "/pipelines" .. path end }})

            assert(calls.agent_list == 1)
            assert(calls.ticket_session_links_for_uuids == 1)
            assert(calls.ticket_session_uuids_by_ticket == 0)
            assert(calls.list_tickets == 0)
            return "ok"
            "#,
            plugin_dir = plugin_dir.display()
        ))
        .eval()
        .expect("project pipelines home render should use bounded session lookup");

    assert_eq!(result, "ok");
}

#[test]
fn catalog_plugin_project_pipelines_mcp_mutators_return_without_bulk_snapshot_publish() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let plugin_dir = project_root_dir().join("catalog/templates/plugins/project-pipelines");
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{plugin_dir}/?.lua;{plugin_dir}/?/init.lua;" .. package.path

            local handlers = {{}}
            local publish_snapshots = 0

            mcp = {{
              tool = function(name, _spec, handler)
                handlers[name] = handler
              end,
              prompt = function(_name, _spec, _handler) end,
            }}

            package.loaded["project_pipelines.repo"] = setmetatable({{
              prune_legacy_seed_data = function() end,
              update_ticket = function(ticket_id, fields)
                assert(ticket_id == "ticket-1")
                assert(fields.title == "Updated")
                return {{ id = ticket_id, title = fields.title }}
              end,
            }}, {{
              __index = function()
                return function() return {{}} end
              end,
            }})

            package.loaded["project_pipelines.engine"] = setmetatable({{
              publish_entity_snapshots = function()
                publish_snapshots = publish_snapshots + 1
                error("MCP mutators must not publish full entity snapshots synchronously")
              end,
            }}, {{
              __index = function()
                return function() return {{}} end
              end,
            }})

            package.loaded["lib.config_resolver"] = {{
              list_agents = function() return {{}} end,
            }}

            require("project_pipelines.mcp").register()

            local result = handlers.project_pipelines_update_ticket({{
              ticket_id = "ticket-1",
              title = "Updated",
            }}, {{}})
            assert(result.ok == true)
            assert(result.result.id == "ticket-1")
            assert(publish_snapshots == 0)
            return "ok"
            "#,
            plugin_dir = plugin_dir.display()
        ))
        .eval()
        .expect("project pipelines MCP mutators should not publish bulk snapshots synchronously");

    assert_eq!(result, "ok");
}

#[test]
fn catalog_plugin_project_pipelines_start_run_queues_agent_and_links_later_by_request_id() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let plugin_dir = project_root_dir().join("catalog/templates/plugins/project-pipelines");
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{plugin_dir}/?.lua;{plugin_dir}/?/init.lua;" .. package.path

            local create_agent_calls = 0
            local events = {{}}
            local run = nil
            local visit = nil

            local ticket = {{
              id = "ticket-1",
              title = "Async spawn",
              target_id = "target-1",
              target_path = "/repo",
            }}
            local pipeline = {{ id = "pipe-1", name = "Default" }}
            local step = {{
              id = "step-1",
              kind = "agent",
              name = "Implement",
              agent_name = "codex",
              prompt = "Do the work",
            }}

            package.loaded["project_pipelines.entities"] = {{
              register = function() end,
              publish_snapshots = function() end,
            }}

            package.loaded["project_pipelines.repo"] = {{
              prune_legacy_seed_data = function() end,
              get_ticket = function(id)
                assert(id == "ticket-1")
                return ticket
              end,
              open_ticket_run = function() return nil end,
              blocking_ticket_dependencies = function() return {{}} end,
              closed_ticket_dependencies = function() return {{}} end,
              get_pipeline = function(id)
                assert(id == "pipe-1")
                return pipeline
              end,
              pipeline_steps = function(id)
                assert(id == "pipe-1")
                return {{ step }}
              end,
              create_run = function(attrs)
                run = {{
                  id = "run-1",
                  ticket_id = attrs.ticket_id,
                  pipeline_id = attrs.pipeline_id,
                  target_id = attrs.target_id,
                  target_path = attrs.target_path,
                  workspace_id = attrs.workspace_id,
                  workspace_name = attrs.workspace_name,
                  base_ticket_id = attrs.base_ticket_id,
                  base_run_id = attrs.base_run_id,
                  base_ref = attrs.base_ref,
                  base_target_path = attrs.base_target_path,
                  status = "queued",
                }}
                return run
              end,
              next_step = function(seen_run)
                assert(seen_run.id == "run-1")
                return step
              end,
              create_run_step_visit = function(run_id, step_id, attrs)
                assert(run_id == "run-1")
                assert(step_id == "step-1")
                visit = {{
                  id = "visit-1",
                  run_id = run_id,
                  step_id = step_id,
                  status = attrs.status,
                  sequence = 1,
                }}
                return visit
              end,
              update_run = function(run_id, attrs)
                assert(run_id == "run-1")
                for key, value in pairs(attrs or {{}}) do
                  run[key] = value
                end
                return run
              end,
              get_run = function(run_id)
                assert(run_id == "run-1")
                return run
              end,
              get_run_step_visit = function(visit_id)
                assert(visit_id == "visit-1")
                return visit
              end,
              latest_step_session = function() return nil end,
              append_event = function(kind, event)
                events[#events + 1] = {{ kind = kind, event = event }}
              end,
              update_run_step = function(_run_id, _step_id, attrs)
                assert(attrs.agent_session_uuid == nil, "start_run must not link a session synchronously")
              end,
              update_run_step_visit = function(_visit_id, attrs)
                assert(attrs.agent_session_uuid == nil, "start_run must not link a session synchronously")
              end,
            }}

            package.loaded["lib.agent"] = {{
              get = function() return nil end,
            }}
            package.loaded["lib.hub"] = {{
              get = function()
                return {{
                  create_agent = function(_, opts)
                    create_agent_calls = create_agent_calls + 1
                    assert(opts.request_id == "project-pipelines:run-1:step-1:agent")
                    assert(opts.metadata.owner_plugin == "project-pipelines")
                    assert(opts.metadata.run_id == "run-1")
                    assert(opts.metadata.step_id == "step-1")
                    assert(opts.base_ref == nil)
                    assert(opts.base_target_path == nil)
                    return {{ ok = true, status = "queued", request_id = opts.request_id }}
                  end,
                }}
              end,
            }}

            local engine = require("project_pipelines.engine")
            local result = engine.start_run({{ ticket_id = "ticket-1", pipeline_id = "pipe-1" }})

            assert(create_agent_calls == 1)
            assert(result.activation.ok == true)
            assert(result.activation.agent.status == "queued")
            assert(result.activation.agent.request_id == "project-pipelines:run-1:step-1:agent")

            local requested = nil
            for _, entry in ipairs(events) do
              assert(entry.kind ~= "step.agent_spawned")
              if entry.kind == "step.agent_requested" then
                requested = entry.event
              end
            end
            assert(requested ~= nil)
            assert(requested.payload.request_id == "project-pipelines:run-1:step-1:agent")
            assert(requested.payload.status == "queued")
            assert(requested.payload.session_uuid == nil)
            assert(visit ~= nil and visit.agent_session_uuid == nil)
            return "ok"
            "#,
            plugin_dir = plugin_dir.display()
        ))
        .eval()
        .expect("project pipelines start_run should queue agent creation");

    assert_eq!(result, "ok");
}

#[test]
fn catalog_plugin_project_pipelines_start_run_threads_stacked_base_metadata() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let plugin_dir = project_root_dir().join("catalog/templates/plugins/project-pipelines");
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{plugin_dir}/?.lua;{plugin_dir}/?/init.lua;" .. package.path

            local run = nil
            local ticket = {{
              id = "ticket-2",
              title = "Stacked change",
              target_id = "target-1",
              target_path = "/repo",
            }}
            local step = {{
              id = "step-1",
              kind = "agent",
              name = "Implement",
              agent_name = "codex",
            }}

            package.loaded["project_pipelines.entities"] = {{
              register = function() end,
              publish_snapshots = function() end,
            }}

            package.loaded["project_pipelines.repo"] = {{
              prune_legacy_seed_data = function() end,
              get_ticket = function(id) return ticket end,
              open_ticket_run = function() return nil end,
              blocking_ticket_dependencies = function() return {{}} end,
              closed_ticket_dependencies = function() return {{}} end,
              get_pipeline = function(id) return {{ id = id }} end,
              pipeline_steps = function() return {{ step }} end,
              create_run = function(attrs)
                assert(attrs.base_ticket_id == "ticket-1")
                assert(attrs.base_run_id == "run-11")
                assert(attrs.base_ref == "project-pipelines/ticket-1")
                assert(attrs.base_target_path == "/worktrees/pr-11")
                run = {{
                  id = "run-2",
                  ticket_id = attrs.ticket_id,
                  pipeline_id = attrs.pipeline_id,
                  target_id = attrs.target_id,
                  target_path = attrs.target_path,
                  base_ticket_id = attrs.base_ticket_id,
                  base_run_id = attrs.base_run_id,
                  base_ref = attrs.base_ref,
                  base_target_path = attrs.base_target_path,
                }}
                return run
              end,
              next_step = function() return step end,
              create_run_step_visit = function() return {{ id = "visit-2", sequence = 1 }} end,
              update_run = function(_run_id, attrs)
                for key, value in pairs(attrs or {{}}) do run[key] = value end
                return run
              end,
              get_run = function() return run end,
              get_run_step_visit = function() return {{ id = "visit-2" }} end,
              latest_step_session = function() return nil end,
              append_event = function() end,
            }}

            package.loaded["lib.agent"] = {{ get = function() return nil end }}
            package.loaded["lib.hub"] = {{
              get = function()
                return {{
                  create_agent = function(_, opts)
                    assert(opts.issue_or_branch == "project-pipelines/ticket-2")
                    assert(opts.base_ref == "project-pipelines/ticket-1")
                    assert(opts.base_target_path == "/worktrees/pr-11")
                    assert(opts.metadata.base_ticket_id == "ticket-1")
                    assert(opts.metadata.base_run_id == "run-11")
                    assert(opts.metadata.base_ref == "project-pipelines/ticket-1")
                    assert(opts.metadata.base_target_path == "/worktrees/pr-11")
                    return {{ ok = true, status = "queued", request_id = opts.request_id }}
                  end,
                }}
              end,
            }}

            local engine = require("project_pipelines.engine")
            local started = engine.start_run({{
              ticket_id = "ticket-2",
              pipeline_id = "pipe-1",
              base_ticket_id = "ticket-1",
              base_run_id = "run-11",
              base_ref = "project-pipelines/ticket-1",
              base_target_path = "/worktrees/pr-11",
            }})

            assert(started.run.base_ref == "project-pipelines/ticket-1")
            assert(started.activation.ok == true)
            return "ok"
            "#,
            plugin_dir = plugin_dir.display()
        ))
        .eval()
        .expect("project pipelines should preserve explicit stacked base metadata");

    assert_eq!(result, "ok");
}

#[test]
fn catalog_plugin_project_pipelines_start_run_infers_base_ref_from_closed_pr_dependency() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let plugin_dir = project_root_dir().join("catalog/templates/plugins/project-pipelines");
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{plugin_dir}/?.lua;{plugin_dir}/?/init.lua;" .. package.path

            local run = nil
            local tickets = {{
              ["ticket-child"] = {{ id = "ticket-child", title = "Child", target_id = "target-1", target_path = "/repo" }},
              ["ticket-parent"] = {{ id = "ticket-parent", title = "Parent", target_id = "target-1", target_path = "/repo" }},
            }}
            local parent_run = {{
              id = "run-parent",
              ticket_id = "ticket-parent",
              base_ref = "project-pipelines/grandparent",
              base_target_path = "/worktrees/parent-pr",
            }}
            local step = {{ id = "step-1", kind = "agent", name = "Implement", agent_name = "codex" }}

            package.loaded["project_pipelines.entities"] = {{ register = function() end, publish_snapshots = function() end }}
            package.loaded["project_pipelines.repo"] = {{
              prune_legacy_seed_data = function() end,
              get_ticket = function(id) return tickets[id] end,
              open_ticket_run = function() return nil end,
              blocking_ticket_dependencies = function() return {{}} end,
              closed_ticket_dependencies = function(ticket_id)
                assert(ticket_id == "ticket-child")
                return {{ {{ ticket_id = "ticket-child", depends_on_ticket_id = "ticket-parent", depends_on_status = "closed" }} }}
              end,
              latest_ticket_run = function(ticket_id)
                assert(ticket_id == "ticket-parent")
                return parent_run
              end,
              latest_merge_pr_artifact = function(run_id)
                assert(run_id == "run-parent")
                return {{ kind = "merge", uri = "https://github.test/pulls/11", payload = "{{}}" }}
              end,
              get_pipeline = function(id) return {{ id = id }} end,
              pipeline_steps = function() return {{ step }} end,
              create_run = function(attrs)
                assert(attrs.base_ticket_id == "ticket-parent")
                assert(attrs.base_run_id == "run-parent")
                assert(attrs.base_ref == "project-pipelines/ticket-parent")
                assert(attrs.base_target_path == "/worktrees/parent-pr")
                run = attrs
                run.id = "run-child"
                return run
              end,
              next_step = function() return step end,
              create_run_step_visit = function() return {{ id = "visit-child", sequence = 1 }} end,
              update_run = function(_run_id, attrs)
                for key, value in pairs(attrs or {{}}) do run[key] = value end
                return run
              end,
              get_run = function() return run end,
              get_run_step_visit = function() return {{ id = "visit-child" }} end,
              latest_step_session = function() return nil end,
              append_event = function() end,
            }}
            package.loaded["lib.agent"] = {{ get = function() return nil end }}
            package.loaded["lib.hub"] = {{
              get = function()
                return {{
                  create_agent = function(_, opts)
                    assert(opts.base_ref == "project-pipelines/ticket-parent")
                    assert(opts.base_target_path == "/worktrees/parent-pr")
                    return {{ ok = true, status = "queued", request_id = opts.request_id }}
                  end,
                }}
              end,
            }}

            local started = require("project_pipelines.engine").start_run({{
              ticket_id = "ticket-child",
              pipeline_id = "pipe-1",
            }})
            assert(started.run.base_ref == "project-pipelines/ticket-parent")
            return "ok"
            "#,
            plugin_dir = plugin_dir.display()
        ))
        .eval()
        .expect("project pipelines should infer stacked base refs from closed PR dependencies");

    assert_eq!(result, "ok");
}

#[test]
fn catalog_plugin_project_pipelines_retry_step_agent_requeues_current_visit() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");

    let plugin_dir = project_root_dir().join("catalog/templates/plugins/project-pipelines");
    let result: String = lua
        .load(format!(
            r#"
            package.path = "{plugin_dir}/?.lua;{plugin_dir}/?/init.lua;" .. package.path

            local run = {{
              id = "run-verify",
              ticket_id = "ticket-verify",
              pipeline_id = "pipe-1",
              status = "blocked",
              current_step_id = "verify",
              current_run_step_id = "visit-verify",
              target_id = "target-1",
              target_path = "/repo",
              workspace_name = "Pipeline - Verify",
            }}
            local visit = {{
              id = "visit-verify",
              run_id = "run-verify",
              step_id = "verify",
              status = "blocked",
              agent_session_uuid = "sess-dead",
            }}
            local step = {{
              id = "verify",
              kind = "agent",
              name = "Verify",
              agent_name = "codex",
              prompt = "Verify the work",
            }}
            local ticket = {{ id = "ticket-verify", title = "Retry verify", target_id = "target-1", target_path = "/repo" }}
            local events = {{}}
            local create_agent_calls = 0

            package.loaded["project_pipelines.entities"] = {{ register = function() end, publish_snapshots = function() end }}
            package.loaded["project_pipelines.repo"] = {{
              get_run = function(id) assert(id == "run-verify"); return run end,
              get_step = function(id) assert(id == "verify"); return step end,
              get_ticket = function(id) assert(id == "ticket-verify"); return ticket end,
              get_run_step_visit = function(id) assert(id == "visit-verify"); return visit end,
              get_run_step = function(run_id, step_id) assert(run_id == "run-verify"); assert(step_id == "verify"); return visit end,
              latest_step_session = function() return nil end,
              update_run = function(id, attrs)
                assert(id == "run-verify")
                for key, value in pairs(attrs or {{}}) do run[key] = value end
                return run
              end,
              update_run_step_visit = function(id, attrs)
                assert(id == "visit-verify")
                for key, value in pairs(attrs or {{}}) do visit[key] = value end
                return visit
              end,
              append_event = function(kind, event)
                events[#events + 1] = {{ kind = kind, event = event }}
              end,
            }}
            package.loaded["lib.agent"] = {{ get = function() return nil end }}
            package.loaded["lib.hub"] = {{
              get = function()
                return {{
                  create_agent = function(_, opts)
                    create_agent_calls = create_agent_calls + 1
                    assert(opts.request_id == "project-pipelines:run-verify:verify:agent")
                    assert(opts.metadata.run_id == "run-verify")
                    assert(opts.metadata.step_id == "verify")
                    return {{ ok = true, status = "queued", request_id = opts.request_id }}
                  end,
                }}
              end,
            }}

            local result = require("project_pipelines.engine").retry_step_agent({{
              run_id = "run-verify",
              reason = "spawn failed before linking",
            }}, {{ session_uuid = "sess-human" }})

            assert(result.ok == true)
            assert(create_agent_calls == 1)
            assert(run.status == "active")
            assert(run.current_run_step_id == "visit-verify")
            assert(visit.status == "active")
            assert(visit.agent_session_uuid == "")

            local saw_retry = false
            local saw_requested = false
            for _, event in ipairs(events) do
              if event.kind == "step.agent_retry_requested" then
                saw_retry = true
                assert(event.event.payload.run_step_id == "visit-verify")
                assert(event.event.payload.requested_by_session_uuid == "sess-human")
              elseif event.kind == "step.agent_requested" then
                saw_requested = true
                assert(event.event.payload.request_id == "project-pipelines:run-verify:verify:agent")
              end
            end
            assert(saw_retry)
            assert(saw_requested)
            return "ok"
            "#,
            plugin_dir = plugin_dir.display()
        ))
        .eval()
        .expect("project pipelines should retry blocked agent steps by reusing the current visit");

    assert_eq!(result, "ok");
}
