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
        .join("catalog")
        .join("templates")
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
fn catalog_plugin_cloudflare_registers_and_runs_generic_session_action() {
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
            package.loaded["lib.hub"] = {
              get = function()
                return {
                  prepare_plugin_command = function(_, opts)
                    _G.prepare_request = opts
                  end,
                  create_accessory = function(_, opts)
                    _G.created_connector_request = opts
                    return {
                      ok = true,
                      status = "queued",
                      request_id = opts.request_id,
                    }
                  end,
                }
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
            json = {
              decode = function(raw)
                if raw:match('"Status"%s*:%s*0') then
                  local decoded = { Status = 0 }
                  if raw:match('"Answer"') then
                    decoded.Answer = { { type = 1, data = "104.21.1.1" } }
                  end
                  return decoded
                end
                if raw:match('"Status"%s*:%s*3') then
                  return { Status = 3 }
                end
                return {}
              end,
            }
            local http_calls = 0
            http = {
              request = function(opts, cb)
                http_calls = http_calls + 1
                _G.last_probe = opts
                if http_calls == 1 then
                  cb({ status = 200, body = '{"Status":3}', headers = {} }, nil)
                else
                  cb({ status = 200, body = '{"Status":0,"Answer":[{"type":1,"data":"104.21.1.1"}]}', headers = {} }, nil)
                end
                return "request-" .. tostring(http_calls), nil
              end,
            }
            _G.timer_callbacks = {}
            timer = {
              after = function(seconds, cb)
                timer_callbacks[#timer_callbacks + 1] = { seconds = seconds, cb = cb }
                return "timer-" .. tostring(#timer_callbacks)
              end,
            }
            hub = {}

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
                _G.last_parent_update = fields
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
                assert(created_connector_request == nil)

                event_handlers.plugin_command_prepared({
                  request_id = "stale-request",
                  command = "/usr/local/bin/cloudflared",
                  config_path = "/tmp/botster-cloudflared-quick.yml",
                  context = { parent_session_uuid = "parent-1", port = 4567 },
                })
                assert(created_connector_request == nil)

                event_handlers.plugin_command_prepared({
                  request_id = preview.prepare_request_id,
                  command = "/usr/local/bin/cloudflared",
                  config_path = "/tmp/botster-cloudflared-quick.yml",
                  context = { parent_session_uuid = "parent-1", port = 4567 },
                })
                assert(created_connector_request.session.command == "/usr/local/bin/cloudflared")
                assert(created_connector_request.session.args[5] == "http://127.0.0.1:4567")
                assert(created_connector_request.metadata.system_kind == "cloudflare_hosted_preview_connector")
                assert(created_connector_request.metadata.owner_plugin == "cloudflare-hosted-preview")
                assert(created_connector_request.metadata.visibility == "plugin")
                assert(created_connector_request.metadata.surface == "cloudflare-hosted-preview")
                assert(created_connector_request.metadata.system_session == nil)
                assert(created_connector_request.metadata.observe_output == true)
                preview = test_sessions["parent-1"].plugin_state.cloudflare_hosted_preview
                assert(preview.status == "starting")
                assert(preview.provider == "cloudflare")
                assert(preview.prepare_request_id ~= false)

                local connector = {
                  session_uuid = "conn-1",
                  status = "running",
                  metadata = created_connector_request.metadata,
                  session_opts = created_connector_request.session,
                  get_meta = function(self, key) return self.metadata[key] end,
                  set_meta = function(self, key, value) self.metadata[key] = value end,
                  close = function(self) self.status = "closed" end,
                }
                test_sessions[connector.session_uuid] = connector
                _G.created_connector = connector
                hook_handlers["agent_created:cloudflare-hosted-preview.connector_created"]({
                  session_uuid = connector.session_uuid,
                  metadata = connector.metadata,
                })
                preview = test_sessions["parent-1"].plugin_state.cloudflare_hosted_preview
                assert(preview.connector_session_uuid == "conn-1")
                assert(preview.prepare_request_id == false)
                assert(timer_callbacks[1].seconds == 20.0)

                hook_handlers["pty_output:cloudflare-hosted-preview.cloudflared_output"](
                  { session_uuid = "conn-1" },
                  "\27[32mready https://preview"
                )
                hook_handlers["pty_output:cloudflare-hosted-preview.cloudflared_output"](
                  { session_uuid = "conn-1" },
                  ".trycloudflare.com\27[0m"
                )
                preview = test_sessions["parent-1"].plugin_state.cloudflare_hosted_preview
                assert(preview.status == "starting")
                assert(preview.url == false)
                assert(last_probe.url == "https://cloudflare-dns.com/dns-query?name=preview.trycloudflare.com&type=A")
                assert(last_probe.method == "GET")
                assert(last_probe.headers["Accept"] == "application/dns-json")
                assert(last_probe.timeout_ms == 3000)
                assert(timer_callbacks[2].seconds == 1.0)
                timer_callbacks[2].cb()

                preview = test_sessions["parent-1"].plugin_state.cloudflare_hosted_preview
                assert(preview.status == "running")
                assert(preview.url == "https://preview.trycloudflare.com")
                assert(test_upserts[#test_upserts].status == "running")
                assert(test_upserts[#test_upserts].url == "https://preview.trycloudflare.com")
                timer_callbacks[1].cb()
                assert(test_sessions["parent-1"].plugin_state.cloudflare_hosted_preview.status == "running")

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

#[test]
fn catalog_plugin_cloudflare_url_discovery_timeout_marks_preview_error() {
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
            local forbid_session_get = false
            package.loaded["lib.entity_model"] = {
              upsert_session_action = function(_action) end,
              remove_session_action = function(_session_uuid, _action_id) end,
            }
            package.loaded["lib.target_context"] = {
              from_session = function(_session) return {} end,
              with_metadata = function(metadata, _context) return metadata or {} end,
            }
            package.loaded["lib.session"] = {
              list = function() return {} end,
              get = function(uuid)
                assert(not forbid_session_get, "Session.get must not run from Cloudflare timer callbacks")
                return sessions[uuid]
              end,
              all_info = function()
                return {
                  { session_uuid = "parent-timeout", id = "parent-timeout", session_type = "agent", port = 4567 },
                }
              end,
            }
            package.loaded["lib.hub"] = {
              get = function()
                return {
                  prepare_plugin_command = function(_, opts) _G.prepare_request = opts end,
                  create_accessory = function(_, opts)
                    _G.created_connector_request = opts
                    return { ok = true, status = "queued", request_id = opts.request_id }
                  end,
                }
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
            http = {
              request = function(_opts, _cb)
                error("URL probe should not run before URL discovery")
              end,
            }
            _G.timer_callbacks = {}
            timer = {
              after = function(seconds, cb)
                timer_callbacks[#timer_callbacks + 1] = { seconds = seconds, cb = cb }
                return "timer-" .. tostring(#timer_callbacks)
              end,
            }
            hub = {}

            local parent = {
              session_uuid = "parent-timeout",
              _port = 4567,
              metadata = {},
              update = function(self, fields)
                self.plugin_state = fields.plugin_state
                _G.last_parent_update = fields
              end,
            }
            sessions[parent.session_uuid] = parent
            _G.test_sessions = sessions
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
                local ok, err = actions.run("parent-timeout", "cloudflare.preview.toggle", { params = {} })
                assert(ok, err)
                local preview = test_sessions["parent-timeout"].plugin_state.cloudflare_hosted_preview
                event_handlers.plugin_command_prepared({
                  request_id = preview.prepare_request_id,
                  command = "/usr/local/bin/cloudflared",
                  config_path = "/tmp/botster-cloudflared-quick.yml",
                  context = { parent_session_uuid = "parent-timeout", port = 4567 },
                })

                local connector = {
                  session_uuid = "conn-timeout",
                  status = "running",
                  metadata = created_connector_request.metadata,
                  get_meta = function(self, key) return self.metadata[key] end,
                  set_meta = function(self, key, value) self.metadata[key] = value end,
                  close = function(self) self.status = "closed" end,
                }
                test_sessions[connector.session_uuid] = connector
                hook_handlers["agent_created:cloudflare-hosted-preview.connector_created"]({
                  session_uuid = connector.session_uuid,
                  metadata = connector.metadata,
                })
                assert(timer_callbacks[1].seconds == 20.0)
                forbid_session_get = true
                timer_callbacks[1].cb()
                forbid_session_get = false

                preview = test_sessions["parent-timeout"].plugin_state.cloudflare_hosted_preview
                assert(preview.status == "error", preview.status)
                assert(preview.error == "Cloudflare quick tunnel did not emit a preview URL", tostring(preview.error))
                assert(preview.connector_session_uuid == false, tostring(preview.connector_session_uuid))
                assert(test_sessions["conn-timeout"].status == "closed", test_sessions["conn-timeout"].status)
                return true
            "#,
            )
            .eval()
            .expect("URL timeout scenario should run")
        })
        .expect("install stubs");

    assert!(ok);
}

#[test]
fn catalog_plugin_cloudflare_readiness_failures_keep_preview_starting() {
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
            package.loaded["lib.entity_model"] = {
              upsert_session_action = function(_action) end,
              remove_session_action = function(_session_uuid, _action_id) end,
            }
            package.loaded["lib.target_context"] = {
              from_session = function(_session) return {} end,
              with_metadata = function(metadata, _context) return metadata or {} end,
            }
            package.loaded["lib.session"] = {
              list = function() return {} end,
              get = function(uuid) return sessions[uuid] end,
              all_info = function()
                return {
                  { session_uuid = "parent-readiness", id = "parent-readiness", session_type = "agent", port = 4567 },
                }
              end,
            }
            package.loaded["lib.hub"] = {
              get = function()
                return {
                  prepare_plugin_command = function(_, opts) _G.prepare_request = opts end,
                  create_accessory = function(_, opts)
                    _G.created_connector_request = opts
                    return { ok = true, status = "queued", request_id = opts.request_id }
                  end,
                }
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
            local probe_count = 0
            http = {
              request = function(opts, _cb)
                probe_count = probe_count + 1
                _G.last_probe = opts
                return nil, "error sending request for url (" .. opts.url .. ")"
              end,
            }
            _G.timer_callbacks = {}
            timer = {
              after = function(seconds, cb)
                timer_callbacks[#timer_callbacks + 1] = { seconds = seconds, cb = cb }
                return "timer-" .. tostring(#timer_callbacks)
              end,
            }
            hub = {}

            local parent = {
              session_uuid = "parent-readiness",
              _port = 4567,
              metadata = {},
              update = function(self, fields)
                self.plugin_state = fields.plugin_state
                _G.last_parent_update = fields
              end,
            }
            sessions[parent.session_uuid] = parent
            _G.test_sessions = sessions
            _G.probe_count = function() return probe_count end
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
                local ok, err = actions.run("parent-readiness", "cloudflare.preview.toggle", { params = {} })
                assert(ok, err)
                local preview = test_sessions["parent-readiness"].plugin_state.cloudflare_hosted_preview
                event_handlers.plugin_command_prepared({
                  request_id = preview.prepare_request_id,
                  command = "/usr/local/bin/cloudflared",
                  config_path = "/tmp/botster-cloudflared-quick.yml",
                  context = { parent_session_uuid = "parent-readiness", port = 4567 },
                })

                local connector = {
                  session_uuid = "conn-readiness",
                  status = "running",
                  metadata = created_connector_request.metadata,
                  get_meta = function(self, key) return self.metadata[key] end,
                  set_meta = function(self, key, value) self.metadata[key] = value end,
                  close = function(self) self.status = "closed" end,
                }
                test_sessions[connector.session_uuid] = connector
                hook_handlers["agent_created:cloudflare-hosted-preview.connector_created"]({
                  session_uuid = connector.session_uuid,
                  metadata = connector.metadata,
                })
                hook_handlers["pty_output:cloudflare-hosted-preview.cloudflared_output"](
                  { session_uuid = "conn-readiness" },
                  "trycloudflare tunnel https://still-propagating.trycloudflare.com"
                )

                for i = 2, 6 do
                  assert(timer_callbacks[i].seconds == 1.0)
                  timer_callbacks[i].cb()
                end

                preview = test_sessions["parent-readiness"].plugin_state.cloudflare_hosted_preview
                assert(probe_count() == 6, tostring(probe_count()))
                assert(preview.status == "starting", preview.status)
                assert(preview.error == false, tostring(preview.error))
                assert(preview.url == false, tostring(preview.url))
                assert(preview.connector_session_uuid == "conn-readiness", tostring(preview.connector_session_uuid))
                assert(test_sessions["conn-readiness"].status == "running", test_sessions["conn-readiness"].status)
                assert(last_probe.url == "https://cloudflare-dns.com/dns-query?name=still-propagating.trycloudflare.com&type=A")
                assert(last_probe.headers["Accept"] == "application/dns-json")
                assert(last_probe.timeout_ms == 3000, tostring(last_probe.timeout_ms))
                return true
            "#,
            )
            .eval()
            .expect("readiness retry scenario should run")
        })
        .expect("install stubs");

    assert!(ok);
}

#[test]
fn catalog_plugin_cloudflare_reconcile_promotes_pending_connector_without_parent_state() {
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
            package.loaded["lib.entity_model"] = {
              upsert_session_action = function(_action) end,
              remove_session_action = function(_session_uuid, _action_id) end,
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
                  { session_uuid = "parent-reconcile", id = "parent-reconcile", session_type = "agent", port = 4567 },
                }
              end,
            }
            package.loaded["lib.hub"] = {
              get = function()
                return {
                  update_session = function(_, uuid, fields)
                    local session = sessions[uuid]
                    if session and fields.metadata then
                      session.metadata = fields.metadata
                    end
                    if session and fields.plugin_state then
                      session.plugin_state = fields.plugin_state
                    end
                    _G.last_update_session = { uuid = uuid, fields = fields }
                  end,
                  prepare_plugin_command = function(_, _opts) error("no prepare expected") end,
                }
              end,
            }

            hooks = { on = function(_name, _key, _fn) end }
            events = {
              on = function(name, fn)
                _G.event_handlers = _G.event_handlers or {}
                _G.event_handlers[name] = fn
                return name
              end,
              off = function(_sub) end,
            }
            json = {
              decode = function(raw)
                if raw:match('"Status"%s*:%s*0') then
                  return { Status = 0, Answer = { { type = 1, data = "104.21.1.1" } } }
                end
                return {}
              end,
            }
            http = {
              request = function(opts, cb)
                _G.last_probe = opts
                _G.probe_callback = cb
                return "request-1", nil
              end,
            }
            timer = { after = function(seconds, cb) _G.last_timer = { seconds = seconds, cb = cb }; return "timer" end }
            hub = {}

            local parent = {
              session_uuid = "parent-reconcile",
              _port = 4567,
              metadata = {},
              update = function(self, fields)
                self.plugin_state = fields.plugin_state
                _G.last_parent_update = fields
              end,
            }
            sessions[parent.session_uuid] = parent
            sessions["conn-reconcile"] = {
              session_uuid = "conn-reconcile",
              status = "active",
              metadata = {
                system_session = true,
                system_kind = "cloudflare_hosted_preview_connector",
                owner_plugin = "cloudflare-hosted-preview",
                target_session_uuid = "parent-reconcile",
                preview_pending_url = "https://ready.trycloudflare.com",
                preview_hostname = "ready.trycloudflare.com",
              },
              get_meta = function(self, key) return self.metadata[key] end,
              set_meta = function(self, key, value) self.metadata[key] = value end,
              close = function(self) self.status = "closed" end,
            }
            _G.test_sessions = sessions
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
                local preview = test_sessions["parent-reconcile"].plugin_state.cloudflare_hosted_preview
                assert(preview.status == "starting", preview.status)
                assert(preview.connector_session_uuid == "conn-reconcile", tostring(preview.connector_session_uuid))
                assert(last_probe.url == "https://cloudflare-dns.com/dns-query?name=ready.trycloudflare.com&type=A")

                test_sessions["parent-reconcile"].plugin_state = nil
                probe_callback({ status = 200, body = '{"Status":0,"Answer":[{"type":1,"data":"104.21.1.1"}]}', headers = {} }, nil)

                preview = test_sessions["parent-reconcile"].plugin_state.cloudflare_hosted_preview
                assert(preview.status == "running", preview.status)
                assert(preview.url == "https://ready.trycloudflare.com", tostring(preview.url))
                assert(preview.connector_session_uuid == "conn-reconcile", tostring(preview.connector_session_uuid))
                assert(test_sessions["conn-reconcile"].metadata.preview_url == "https://ready.trycloudflare.com")
                assert(test_sessions["conn-reconcile"].metadata.preview_pending_url == false)
                return true
            "#,
            )
            .eval()
            .expect("pending connector reconcile scenario should run")
        })
        .expect("install stubs");

    assert!(ok);
}

#[test]
fn catalog_plugin_cloudflare_recovered_connector_restores_parent_action_state() {
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
              upsert_session_action = function(action) upserts[#upserts + 1] = action end,
              remove_session_action = function(_session_uuid, _action_id) end,
            }
            package.loaded["lib.target_context"] = {
              from_session = function(_session) return {} end,
              with_metadata = function(metadata, _context) return metadata or {} end,
            }
            package.loaded["lib.session"] = {
              list = function() return {} end,
              get = function(uuid) return sessions[uuid] end,
              all_info = function()
                return {
                  { session_uuid = "parent-recovered", id = "parent-recovered", session_type = "agent", port = 4567 },
                }
              end,
            }
            package.loaded["lib.hub"] = {
              get = function()
                return {
                  update_session = function(_, uuid, fields)
                    local session = sessions[uuid]
                    if session and fields.metadata then session.metadata = fields.metadata end
                  end,
                  prepare_plugin_command = function(_, _opts) error("no prepare expected") end,
                }
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
            http = { request = function() error("no readiness probe expected") end }
            timer = { after = function() error("no timer expected") end }
            hub = {}

            local parent = {
              session_uuid = "parent-recovered",
              _port = 4567,
              metadata = {},
              update = function(self, fields)
                self.plugin_state = fields.plugin_state
                _G.last_parent_update = fields
                require("lib.session_actions").publish_for_session(self)
              end,
            }
            sessions[parent.session_uuid] = parent
            local connector_metadata = {
              system_kind = "cloudflare_hosted_preview_connector",
              owner_plugin = "cloudflare-hosted-preview",
              visibility = "plugin",
              surface = "cloudflare-hosted-preview",
              request_id = "parent-recovered:1",
              target_session_uuid = "parent-recovered",
              preview_url = "https://recovered.trycloudflare.com",
              preview_hostname = "recovered.trycloudflare.com",
            }
            sessions["conn-recovered"] = {
              session_uuid = "conn-recovered",
              metadata = connector_metadata,
              get_meta = function(self, key) return self.metadata[key] end,
              set_meta = function(self, key, value) self.metadata[key] = value end,
              close = function(self) self.status = "closed" end,
            }
            _G.recovered_info = {
              session_uuid = "conn-recovered",
              metadata = connector_metadata,
            }
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
                hook_handlers["agent_created:cloudflare-hosted-preview.connector_created"](recovered_info)

                local preview = test_sessions["parent-recovered"].plugin_state.cloudflare_hosted_preview
                assert(preview.status == "running", preview.status)
                assert(preview.url == "https://recovered.trycloudflare.com", tostring(preview.url))
                assert(preview.connector_session_uuid == "conn-recovered", tostring(preview.connector_session_uuid))
                assert(test_sessions["conn-recovered"].metadata.preview_url == "https://recovered.trycloudflare.com")
                assert(test_sessions["conn-recovered"].metadata.preview_pending_url == false)
                assert(test_upserts[#test_upserts].status == "running", tostring(test_upserts[#test_upserts].status))
                assert(test_upserts[#test_upserts].url == "https://recovered.trycloudflare.com", tostring(test_upserts[#test_upserts].url))
                return true
            "#,
            )
            .eval()
            .expect("recovered connector scenario should run")
        })
        .expect("install stubs");

    assert!(ok);
}

#[test]
fn catalog_plugin_cloudflare_reconcile_uses_owned_plugin_connectors() {
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
              upsert_session_action = function(action) upserts[#upserts + 1] = action end,
              remove_session_action = function(_session_uuid, _action_id) end,
            }
            package.loaded["lib.target_context"] = {
              from_session = function(_session) return {} end,
              with_metadata = function(metadata, _context) return metadata or {} end,
            }
            package.loaded["lib.session"] = {
              list = function()
                return { sessions["parent-owned-plugin"] }
              end,
              get = function(uuid) return sessions[uuid] end,
              all_info = function()
                return {
                  { session_uuid = "parent-owned-plugin", id = "parent-owned-plugin", session_type = "agent", port = 4567 },
                }
              end,
            }

            local connector_metadata = {
              system_kind = "cloudflare_hosted_preview_connector",
              owner_plugin = "cloudflare-hosted-preview",
              visibility = "plugin",
              surface = "cloudflare-hosted-preview",
              target_session_uuid = "parent-owned-plugin",
              preview_url = "https://owned-plugin.trycloudflare.com",
              preview_hostname = "owned-plugin.trycloudflare.com",
            }
            package.loaded["lib.hub"] = {
              get = function()
                return {
                  list_owned_sessions = function(_, owner_plugin)
                    assert(owner_plugin == "cloudflare-hosted-preview", tostring(owner_plugin))
                    return {
                      {
                        session_uuid = "conn-owned-plugin",
                        status = "active",
                        metadata = connector_metadata,
                      },
                    }
                  end,
                  update_session = function(_, uuid, fields)
                    if uuid == "conn-owned-plugin" and fields.metadata then
                      connector_metadata = fields.metadata
                    end
                  end,
                  prepare_plugin_command = function(_, _opts) error("no prepare expected") end,
                }
              end,
            }

            hooks = { on = function(_name, _key, _fn) end }
            events = {
              on = function(name, fn)
                _G.event_handlers = _G.event_handlers or {}
                _G.event_handlers[name] = fn
                return name
              end,
              off = function(_sub) end,
            }
            http = { request = function() error("no readiness probe expected") end }
            timer = { after = function() error("no timer expected") end }
            hub = {}

            local parent = {
              session_uuid = "parent-owned-plugin",
              _port = 4567,
              metadata = {},
              update = function(self, fields)
                self.plugin_state = fields.plugin_state
                _G.last_parent_update = fields
                require("lib.session_actions").publish_for_session(self)
              end,
            }
            sessions[parent.session_uuid] = parent
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
                local preview = test_sessions["parent-owned-plugin"].plugin_state.cloudflare_hosted_preview
                assert(preview.status == "running", preview.status)
                assert(preview.url == "https://owned-plugin.trycloudflare.com", tostring(preview.url))
                assert(preview.connector_session_uuid == "conn-owned-plugin", tostring(preview.connector_session_uuid))
                assert(test_upserts[#test_upserts].status == "running", tostring(test_upserts[#test_upserts].status))
                assert(test_upserts[#test_upserts].url == "https://owned-plugin.trycloudflare.com", tostring(test_upserts[#test_upserts].url))
                return true
            "#,
            )
            .eval()
            .expect("owned plugin connector reconcile scenario should run")
        })
        .expect("install stubs");

    assert!(ok);
}

#[test]
fn catalog_plugin_cloudflare_closes_all_existing_connectors_before_retry() {
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
            package.loaded["lib.entity_model"] = {
              upsert_session_action = function(_action) end,
              remove_session_action = function(_session_uuid, _action_id) end,
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
                  { session_uuid = "parent-retry", id = "parent-retry", session_type = "agent", port = 4567 },
                }
              end,
            }
            package.loaded["lib.hub"] = {
              get = function()
                return {
                  prepare_plugin_command = function(_, opts) _G.prepare_request = opts end,
                }
              end,
            }

            hooks = { on = function(_name, _key, _fn) end }
            events = {
              on = function(name, fn)
                _G.event_handlers = _G.event_handlers or {}
                _G.event_handlers[name] = fn
                return name
              end,
              off = function(_sub) end,
            }
            http = { request = function() error("no probe expected") end }
            timer = { after = function() return "timer" end }
            hub = {}

            local parent = {
              session_uuid = "parent-retry",
              _port = 4567,
              metadata = {},
              plugin_state = {
                cloudflare_hosted_preview = {
                  status = "error",
                  connector_session_uuid = "old-2",
                },
              },
              update = function(self, fields)
                self.plugin_state = fields.plugin_state
              end,
            }
            sessions[parent.session_uuid] = parent

            local function connector(uuid)
              return {
                session_uuid = uuid,
                status = "active",
                metadata = {
                  system_kind = "cloudflare_hosted_preview_connector",
                  owner_plugin = "cloudflare-hosted-preview",
                  visibility = "plugin",
                  surface = "cloudflare-hosted-preview",
                  target_session_uuid = "parent-retry",
                },
                get_meta = function(self, key) return self.metadata[key] end,
                close = function(self) self.status = "closed" end,
              }
            end
            sessions["old-1"] = connector("old-1")
            sessions["old-2"] = connector("old-2")
            _G.test_sessions = sessions
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
                local ok, err = actions.run("parent-retry", "cloudflare.preview.toggle", {
                  params = { enabled = true },
                })
                assert(ok, err)
                local preview = test_sessions["parent-retry"].plugin_state.cloudflare_hosted_preview
                assert(test_sessions["old-1"].status == "closed", test_sessions["old-1"].status)
                assert(test_sessions["old-2"].status == "closed", test_sessions["old-2"].status)
                assert(preview.status == "starting", preview.status)
                assert(preview.connector_session_uuid == false, tostring(preview.connector_session_uuid))
                assert(preview.prepare_request_id ~= nil, "missing prepare_request_id")
                assert(prepare_request.command == "cloudflared", tostring(prepare_request and prepare_request.command))
                return true
            "#,
            )
            .eval()
            .expect("retry connector cleanup scenario should run")
        })
        .expect("install stubs");

    assert!(ok);
}
