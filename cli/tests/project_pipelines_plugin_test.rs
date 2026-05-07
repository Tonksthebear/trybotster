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
fn home_render_uses_bounded_notified_session_lookup() {
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
fn mcp_mutators_return_without_bulk_snapshot_publish() {
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
fn start_run_queues_agent_and_links_later_by_request_id() {
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
