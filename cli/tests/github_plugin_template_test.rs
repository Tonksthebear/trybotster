//! Regression tests for the GitHub plugin catalog template shape.

use mlua::{Lua, LuaSerdeExt, Value};
use serde_json::{json, Value as JsonValue};

fn create_lua_vm() -> Lua {
    let lua = Lua::new();

    botster::lua::primitives::fs::register(&lua).expect("fs register");
    botster::lua::primitives::json::register(&lua).expect("json register");
    botster::lua::primitives::log::register(&lua).expect("log register");

    let lua_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
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

#[test]
fn github_template_catalog_entry_is_a_multi_file_plugin() {
    let catalog_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("catalog/templates");
    let catalog_root = catalog_root.to_str().unwrap();

    let lua = create_lua_vm();

    let files: Vec<String> = lua
        .load(format!(
            r#"
            local catalog = require("lib.template_catalog")
            local templates = catalog.list({{ source_root = "{catalog_root}" }})
            local out = {{}}
            for _, template in ipairs(templates) do
              if template.dest:match("^plugins/github/") then
                out[#out + 1] = template.dest
              end
            end
            table.sort(out)
            return out
            "#
        ))
        .eval()
        .expect("template catalog should load GitHub plugin template files");

    assert_eq!(
        files,
        vec![
            "plugins/github/event_routing.lua",
            "plugins/github/init.lua",
            "plugins/github/mcp_proxy.lua",
            "plugins/github/notifications.lua",
        ]
    );
}

#[test]
fn project_pipelines_template_catalog_entry_is_a_multi_file_plugin() {
    let catalog_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("catalog/templates");
    let catalog_root = catalog_root.to_str().unwrap();

    let lua = create_lua_vm();

    let files: Vec<String> = lua
        .load(format!(
            r#"
            local catalog = require("lib.template_catalog")
            local templates = catalog.list({{ source_root = "{catalog_root}" }})
            local out = {{}}
            for _, template in ipairs(templates) do
              if template.dest:match("^plugins/project%-pipelines/") then
                out[#out + 1] = template.dest
              end
            end
            table.sort(out)
            return out
            "#
        ))
        .eval()
        .expect("template catalog should load Project Pipelines plugin template files");

    assert_eq!(
        files,
        vec![
            "plugins/project-pipelines/README.md",
            "plugins/project-pipelines/init.lua",
            "plugins/project-pipelines/project_pipelines/db.lua",
            "plugins/project-pipelines/project_pipelines/engine.lua",
            "plugins/project-pipelines/project_pipelines/entities.lua",
            "plugins/project-pipelines/project_pipelines/mcp.lua",
            "plugins/project-pipelines/project_pipelines/repo.lua",
            "plugins/project-pipelines/project_pipelines/util.lua",
            "plugins/project-pipelines/project_pipelines/web/actions.lua",
            "plugins/project-pipelines/project_pipelines/web/screens/home.lua",
            "plugins/project-pipelines/project_pipelines/web/screens/new.lua",
            "plugins/project-pipelines/project_pipelines/web/screens/pipelines.lua",
            "plugins/project-pipelines/project_pipelines/web/screens/project.lua",
            "plugins/project-pipelines/project_pipelines/web/screens/run.lua",
            "plugins/project-pipelines/project_pipelines/web/screens/ticket.lua",
            "plugins/project-pipelines/project_pipelines/web/surface.lua",
            "plugins/project-pipelines/project_pipelines/web/ui.lua",
        ]
    );
}

#[test]
fn project_pipelines_dynamic_state_uses_plugin_entities_not_forced_tree_refreshes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("catalog/templates/plugins/project-pipelines");
    let engine = std::fs::read_to_string(root.join("project_pipelines/engine.lua"))
        .expect("read project pipelines engine");
    let entities = std::fs::read_to_string(root.join("project_pipelines/entities.lua"))
        .expect("read project pipelines entities");
    let readme = std::fs::read_to_string(root.join("README.md"))
        .expect("read project pipelines readme");

    assert!(
        !engine.contains("broadcast_ui_tree_snapshots")
            && !engine.contains("send_ui_tree_snapshots"),
        "Project Pipelines mutators must not force data-only ui_tree_snapshot refreshes"
    );
    for (entity_type, lua_key) in [
        ("project-pipelines.ticket", "ticket"),
        ("project-pipelines.run", "run"),
        ("project-pipelines.run_step", "run_step"),
        ("project-pipelines.gate_result", "gate_result"),
        ("project-pipelines.review", "review"),
        ("project-pipelines.finding", "finding"),
        ("project-pipelines.artifact", "artifact"),
        ("project-pipelines.question", "question"),
        ("project-pipelines.event", "event"),
        ("project-pipelines.pipeline_step", "pipeline_step"),
        ("project-pipelines.pipeline_gate", "pipeline_gate"),
    ] {
        assert!(
            entities.contains(&format!("{lua_key} = OWNER ..")),
            "entities.lua should publish {entity_type}"
        );
        assert!(
            readme.contains(entity_type),
            "README should document {entity_type}"
        );
    }
}

#[test]
fn project_pipelines_entities_register_snapshot_and_delta_publish() {
    let plugin_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("catalog/templates/plugins/project-pipelines");
    let plugin_root = plugin_root.to_str().unwrap();

    let lua = create_lua_vm();
    let script = r#"
            package.path = "__PLUGIN_ROOT__/?.lua;__PLUGIN_ROOT__/?/init.lua;" .. package.path

            local registrations = {}
            local frames = {}

            package.loaded["lib.entity_broadcast"] = {
              register = function(entity_type, opts)
                registrations[#registrations + 1] = {
                  entity_type = entity_type,
                  id_field = opts.id_field,
                  owner_plugin = opts.owner_plugin,
                }
              end,
            }

            package.loaded["lib.hub"] = {
              get = function()
                return {
                  entity_snapshot = function(_self, entity_type, items, opts)
                    frames[#frames + 1] = { type = "snapshot", entity_type = entity_type, items = items, owner_plugin = opts.owner_plugin }
                  end,
                  entity_upsert = function(_self, entity_type, entity, opts)
                    frames[#frames + 1] = { type = "upsert", entity_type = entity_type, entity = entity, owner_plugin = opts.owner_plugin }
                  end,
                  entity_remove = function(_self, entity_type, id, opts)
                    frames[#frames + 1] = { type = "remove", entity_type = entity_type, id = id, owner_plugin = opts.owner_plugin }
                  end,
                }
              end,
            }

            package.loaded["project_pipelines.web.ui"] = {
              target_label = function(target_id, target_path) return target_id or target_path or "No target" end,
              status_tone = function(status) return status == "done" and "success" or "muted" end,
              ticket_notification_count = function() return 0 end,
              visible_tickets = function(repo) return repo.standalone_tickets() end,
            }

            local rows_by_name = {
              tickets = {
                { id = "ticket-1", title = "Standalone Ticket", status = "open", target_id = "target-1", created_at = 1, updated_at = 2 },
                { id = "ticket-2", title = "Project Ticket", status = "open", project_id = "project-1", target_id = "target-1", created_at = 1, updated_at = 2 },
              },
              runs = { { id = "run-1", ticket_id = "ticket-1", pipeline_id = "pipe-1", status = "active", current_step_id = "step-1", created_at = 1, updated_at = 2 } },
              pipeline_steps = { { id = "step-1", pipeline_id = "pipe-1", position = 1, kind = "agent", name = "Implement", agent_name = "codex", prompt = "Do it", created_at = 1, updated_at = 2 } },
              pipeline_gates = { { id = "gate-1", step_id = "step-1", kind = "attestation", prompt = "Evidence", required_fields = "[\"summary\"]", created_at = 1, updated_at = 2 } },
              gate_results = { { id = "gate-result-1", run_id = "run-1", run_step_id = "run-step-1", step_id = "step-1", gate_id = "gate-1", status = "passed", evidence = "{\"summary\":\"ok\"}", created_at = 3 } },
              run_steps = { { id = "run-step-1", run_id = "run-1", step_id = "step-1", sequence = 1, status = "active", created_at = 1, updated_at = 2 } },
              artifacts = { { id = "artifact-1", run_id = "run-1", kind = "note", payload = "{\"a\":1}", created_at = 3 } },
              events = { { id = "event-1", run_id = "run-1", kind = "gate.submitted", payload = "{\"gate_id\":\"gate-1\"}", created_at = 4 } },
            }

            local db = {}
            function db:eval(sql, params)
              if sql:find("FROM tickets") then return rows_by_name.tickets end
              if sql:find("FROM runs") then return rows_by_name.runs end
              if sql:find("FROM pipeline_steps") then return rows_by_name.pipeline_steps end
              if sql:find("FROM pipeline_gates") then return rows_by_name.pipeline_gates end
              if sql:find("FROM gate_results") then return rows_by_name.gate_results end
              if sql:find("FROM run_steps") then return rows_by_name.run_steps end
              if sql:find("FROM artifacts") then return rows_by_name.artifacts end
              if sql:find("FROM events") then return rows_by_name.events end
              return {}
            end
            package.loaded["project_pipelines.db"] = db

            package.loaded["project_pipelines.repo"] = {
              standalone_tickets = function() return rows_by_name.tickets end,
              ticket_runs = function() return rows_by_name.runs end,
              get_step = function() return rows_by_name.pipeline_steps[1] end,
            }

            local entities = require("project_pipelines.entities")
            entities.register()
            entities.snapshot(entities.types.ticket)
            entities.snapshot(entities.types.run_step)
            entities.snapshot(entities.types.gate_result)
            entities.snapshot(entities.types.artifact)
            entities.snapshot(entities.types.event)
            entities.upsert(entities.types.run_step, rows_by_name.run_steps[1])
            entities.upsert(entities.types.run_step, { id = 123, run_id = "run-1" })
            entities.remove(entities.types.run_step, "run-step-1")

            return { registrations = registrations, frames = frames }
            "#
    .replace("__PLUGIN_ROOT__", plugin_root);
    let value: Value = lua
        .load(script)
        .eval()
        .expect("project pipelines entities behavior script");
    let result: JsonValue = lua
        .from_value(value)
        .expect("project pipelines entities result json");

    assert_eq!(result["registrations"].as_array().unwrap().len(), 15);
    assert!(
        result["registrations"].as_array().unwrap().iter().all(|registration| {
            registration["id_field"] == json!("id")
                && registration["owner_plugin"] == json!("project-pipelines")
        })
    );

    let frames = result["frames"].as_array().unwrap();
    assert_eq!(frames[0]["type"], json!("snapshot"));
    assert_eq!(frames[0]["entity_type"], json!("project-pipelines.ticket"));
    assert_eq!(frames[0]["items"].as_array().unwrap().len(), 2);
    assert_eq!(frames[0]["items"][1]["id"], json!("ticket-2"));
    assert_eq!(frames[0]["items"][1]["project_id"], json!("project-1"));
    assert_eq!(frames[1]["entity_type"], json!("project-pipelines.run_step"));
    assert_eq!(frames[1]["items"][0]["name"], json!("Implement"));
    assert_eq!(frames[1]["items"][0]["ticket_id"], json!("ticket-1"));
    assert_eq!(frames[2]["items"][0]["evidence"]["summary"], json!("ok"));
    assert_eq!(frames[3]["items"][0]["payload"]["a"], json!(1));
    assert_eq!(frames[4]["items"][0]["payload"]["gate_id"], json!("gate-1"));
    assert_eq!(frames[5]["type"], json!("upsert"));
    assert_eq!(frames[6]["type"], json!("remove"));
    assert_eq!(frames.len(), 7, "non-string id upsert should be dropped");
}

#[test]
fn github_event_routing_template_uses_internal_client_ingress() {
    let template = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("catalog/templates/plugins/github/event_routing.lua"),
    )
    .expect("read github event routing template");

    assert!(
        template.contains(r#"require("lib.internal_client")"#),
        "GitHub template should route application commands through client.lua ingress"
    );
    assert!(
        template.contains("InternalClient.dispatch"),
        "GitHub template should dispatch canonical commands through internal client"
    );
    assert!(
        !template.contains(r#"events.emit("command_message""#),
        "GitHub template must not use the legacy command_message bypass"
    );
}

#[test]
fn github_event_routing_template_notifies_matching_agent_before_create() {
    let template_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("catalog/templates/plugins/github/event_routing.lua");
    let template_path = template_path.to_str().unwrap();

    let lua = create_lua_vm();

    let result: String = lua
        .load(format!(
            r#"
            local notifications = {{}}
            local dispatches = 0
            local acked = false
            local callback = nil

            package.loaded["lib.agent"] = {{
              find_by_workspace = function(name)
                if name == "owner/repo#42" then
                  return {{
                    {{
                      session_uuid = "sess-existing",
                      session = {{
                        send_message = function(_, text)
                          notifications[#notifications + 1] = text
                        end,
                      }},
                    }},
                  }}
                end
                return {{}}
              end,
            }}
            package.loaded["lib.internal_client"] = {{
              dispatch = function()
                dispatches = dispatches + 1
              end,
            }}
            package.loaded["hub.state"] = {{
              get = function() return {{}} end,
            }}
            action_cable = {{
              connect = function() return "conn-1" end,
              subscribe = function(_, _, _, cb)
                callback = cb
                return "chan-1"
              end,
              perform = function(_, action, data)
                if action == "ack" and data.id == 7 then acked = true end
              end,
              close = function() end,
            }}

            local routing = dofile("{template_path}")
            routing.start("owner/repo")
            callback({{
              id = 7,
              event_type = "issue_comment",
              payload = {{
                issue_number = 42,
                prompt = "Please inspect this",
              }},
            }}, "chan-1")

            assert(#notifications == 1)
            assert(notifications[1]:match("Please inspect this"))
            assert(dispatches == 0)
            assert(acked == true)
            return "ok"
            "#
        ))
        .eval()
        .expect("GitHub template should notify matching agents instead of spawning");

    assert_eq!(result, "ok");
}
