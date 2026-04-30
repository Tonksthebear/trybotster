//! Regression tests for the GitHub plugin catalog template shape.

use mlua::Lua;

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
