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
