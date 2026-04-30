//! Rust-hosted Lua tests for the Cloudflare hosted-preview session action.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    reason = "test-code brevity"
)]

use mlua::Lua;
use std::path::PathBuf;

fn repo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli has repo parent")
        .to_path_buf()
}

fn lua_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua")
}

fn plugin_path() -> PathBuf {
    repo_dir()
        .join(".botster")
        .join("plugins")
        .join("cloudflare-hosted-preview")
        .join("init.lua")
}

fn new_lua() -> Lua {
    let lua = Lua::new();
    let dir = lua_src_dir();
    let setup = format!(
        r#"package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path"#,
        dir = dir.display()
    );
    lua.load(&setup).exec().expect("set package.path");
    lua
}

#[test]
fn cloudflare_plugin_registers_and_runs_generic_session_action() {
    let lua = new_lua();
    let plugin_source = std::fs::read_to_string(plugin_path()).expect("read plugin");

    let ok: bool = lua
        .load(
            r#"
            local store = {}
            package.loaded["hub.state"] = {
              get = function(key, default)
                if store[key] == nil then store[key] = default end
                return store[key]
              end,
              set = function(key, value) store[key] = value end,
            }

            local sessions = {}
            local upserts = {}
            package.loaded["lib.entity_model"] = {
              upsert_session_action = function(action)
                upserts[#upserts + 1] = action
              end,
              remove_session_action = function(session_uuid, action_id)
                _G.removed_action = { session_uuid = session_uuid, action_id = action_id }
              end,
            }
            package.loaded["lib.target_context"] = {
              from_session = function(_session) return {} end,
              with_metadata = function(metadata, _context) return metadata or {} end,
            }
            package.loaded["lib.session"] = {
              list = function()
                local out = {}
                for _, session in pairs(sessions) do out[#out + 1] = session end
                return out
              end,
              get = function(uuid) return sessions[uuid] end,
              all_info = function()
                return {
                  {
                    session_uuid = "parent-1",
                    id = "parent-1",
                    session_type = "agent",
                    title = "Rails",
                    port = 4567,
                    plugin_state = sessions["parent-1"].plugin_state,
                  },
                  {
                    session_uuid = "no-port",
                    id = "no-port",
                    session_type = "agent",
                    title = "No Port",
                  },
                }
              end,
            }
            package.loaded["lib.accessory"] = {
              new = function(opts)
                local connector = {
                  session_uuid = "conn-1",
                  status = "running",
                  metadata = opts.metadata,
                  session_opts = opts.session,
                  get_meta = function(self, key) return self.metadata[key] end,
                  set_meta = function(self, key, value) self.metadata[key] = value end,
                  close = function(self) self.status = "closed" end,
                }
                sessions[connector.session_uuid] = connector
                _G.created_connector = connector
                return connector
              end,
            }

            hooks = {
              on = function(name, key, fn)
                _G.hook_handlers = _G.hook_handlers or {}
                _G.hook_handlers[name .. ":" .. key] = fn
              end,
            }
            events = {
              on = function(name, fn)
                _G.event_handlers = _G.event_handlers or {}
                _G.event_handlers[name] = fn
                return name
              end,
              off = function(_sub) end,
            }
            hub = {
              prepare_plugin_command = function(opts)
                _G.prepare_request = opts
              end,
              probe_url_ready = function(connector_uuid, parent_uuid, url, hostname, timeout_secs)
                _G.probe = {
                  connector_uuid = connector_uuid,
                  parent_uuid = parent_uuid,
                  url = url,
                  hostname = hostname,
                  timeout_secs = timeout_secs,
                }
              end,
            }

            local parent = {
              session_uuid = "parent-1",
              _port = 4567,
              _workspace_name = "Realignment",
              _workspace_id = "ws-1",
              repo = "repo",
              branch_name = "main",
              worktree_path = "/tmp/repo",
              metadata = {},
              update = function(self, fields)
                for key, value in pairs(fields) do self[key] = value end
                require("lib.session_actions").publish_for_session(self)
              end,
            }
            sessions[parent.session_uuid] = parent
            sessions["no-port"] = { session_uuid = "no-port", metadata = {} }
            _G.test_sessions = sessions
            _G.test_upserts = upserts
        "#,
        )
        .exec()
        .map(|()| {
            lua.load(&plugin_source)
                .set_name("@cloudflare-hosted-preview/init.lua")
                .exec()
                .expect("load plugin");
            lua.load(
                r#"
                local actions = require("lib.session_actions")
                assert(actions.get("cloudflare.preview.toggle"), "action registered")
                assert(test_upserts[1].action_id == "cloudflare.preview.toggle")
                assert(test_upserts[1].session_uuid == "parent-1")
                assert(test_upserts[1].visibility == "visible")
                assert(test_upserts[2].session_uuid == "no-port")
                assert(test_upserts[2].visibility == "hidden")

                local ok, err = actions.run("parent-1", "cloudflare.preview.toggle", { params = {} })
                assert(ok, err)
                local preview = test_sessions["parent-1"].plugin_state.cloudflare_hosted_preview
                assert(preview.status == "starting")
                assert(preview.prepare_request_id ~= nil)
                assert(prepare_request.command == "cloudflared")
                assert(prepare_request.config_path:match("botster%-cloudflared%-quick%.yml$"))
                assert(prepare_request.config_contents == "{}\n")
                assert(prepare_request.context.parent_session_uuid == "parent-1")
                assert(created_connector == nil)

                event_handlers.plugin_command_prepared({
                  request_id = "stale-request",
                  command = "/usr/local/bin/cloudflared",
                  config_path = "/tmp/botster-cloudflared-quick.yml",
                  context = { parent_session_uuid = "parent-1", port = 4567 },
                })
                assert(created_connector == nil)

                event_handlers.plugin_command_prepared({
                  request_id = preview.prepare_request_id,
                  command = "/usr/local/bin/cloudflared",
                  config_path = "/tmp/botster-cloudflared-quick.yml",
                  context = { parent_session_uuid = "parent-1", port = 4567 },
                })
                assert(created_connector.session_opts.command == "/usr/local/bin/cloudflared")
                assert(created_connector.session_opts.args[5] == "http://127.0.0.1:4567")
                assert(created_connector.metadata.system_kind == "cloudflare_hosted_preview_connector")
                assert(created_connector.metadata.owner_plugin == "cloudflare-hosted-preview")
                preview = test_sessions["parent-1"].plugin_state.cloudflare_hosted_preview
                assert(preview.status == "starting")
                assert(preview.provider == "cloudflare")
                assert(preview.prepare_request_id == false)

                hook_handlers["pty_output:cloudflare-hosted-preview.cloudflared_output"](
                  { session_uuid = "conn-1" },
                  "ready https://preview.trycloudflare.com"
                )
                assert(probe.connector_uuid == "conn-1")
                assert(probe.parent_uuid == "parent-1")
                assert(probe.url == "https://preview.trycloudflare.com")
                assert(probe.hostname == "preview.trycloudflare.com")
                assert(probe.timeout_secs == 15.0)

                event_handlers.url_probe_ready({
                  connector_session_uuid = "conn-1",
                  parent_session_uuid = "parent-1",
                  url = "https://preview.trycloudflare.com",
                  ready = true,
                })
                preview = test_sessions["parent-1"].plugin_state.cloudflare_hosted_preview
                assert(preview.status == "running")
                assert(preview.url == "https://preview.trycloudflare.com")
                assert(test_upserts[#test_upserts].status == "running")
                assert(test_upserts[#test_upserts].url == "https://preview.trycloudflare.com")

                local disabled, disable_err = actions.run(
                  "parent-1",
                  "cloudflare.preview.toggle",
                  { params = { enabled = false } }
                )
                assert(disabled, disable_err)
                assert(test_sessions["parent-1"].plugin_state.cloudflare_hosted_preview.status == "inactive")
                assert(test_sessions["conn-1"].status == "closed")

                local retry, retry_err = actions.run(
                  "parent-1",
                  "cloudflare.preview.toggle",
                  { params = { enabled = true } }
                )
                assert(retry, retry_err)
                preview = test_sessions["parent-1"].plugin_state.cloudflare_hosted_preview
                event_handlers.plugin_command_prepared({
                  request_id = preview.prepare_request_id,
                  error_kind = "command_missing",
                  error = "Command not found: cloudflared",
                  context = { parent_session_uuid = "parent-1", port = 4567 },
                })
                preview = test_sessions["parent-1"].plugin_state.cloudflare_hosted_preview
                return preview.status == "error"
                  and preview.error == "Hosted preview requires cloudflared to be installed on this machine."
                  and preview.install_url:match("cloudflare%-one") ~= nil
            "#,
            )
            .eval()
            .expect("plugin action scenario should run")
        })
        .expect("install stubs");

    assert!(ok);
}
