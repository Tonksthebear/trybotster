//! Rust-hosted Lua tests for the Cloudflare stable URL connector plugin.

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

fn plugin_dir() -> PathBuf {
    repo_dir()
        .join("catalog")
        .join("templates")
        .join("plugins")
        .join("cloudflare-stable-urls")
}

fn plugin_path(relative: &str) -> PathBuf {
    plugin_dir().join(relative)
}

fn new_lua() -> Lua {
    let lua = Lua::new();
    let lua_dir = lua_src_dir();
    let plugin_dir = plugin_dir();
    let setup = format!(
        r#"package.path = "{lua_dir}/?.lua;{lua_dir}/?/init.lua;{plugin_dir}/?.lua;{plugin_dir}/?/init.lua;" .. package.path"#,
        lua_dir = lua_dir.display(),
        plugin_dir = plugin_dir.display()
    );
    lua.load(&setup).exec().expect("set package.path");
    lua
}

#[test]
fn cloudflare_stable_urls_reconcile_fetches_broker_material_and_spawns_named_connector() {
    let lua = new_lua();
    let connector_source =
        std::fs::read_to_string(plugin_path("cloudflare_stable_urls/connector.lua"))
            .expect("read connector");

    lua.load(
        r#"
        _G.saved_connector = { connector_generation = 0, retry_count = 0 }
        _G.claim_status = {}
        package.loaded["cloudflare_stable_urls.repo"] = {
          connector = function() return _G.saved_connector end,
          save_connector = function(attrs)
            for k, v in pairs(attrs or {}) do _G.saved_connector[k] = v end
            return _G.saved_connector
          end,
          active_claims = function()
            return {
              {
                id = "claim-1",
                hostname = "hook.example.test",
                public_url = "https://hook.example.test",
                owner_plugin = "github",
                owner_key = "repo:owner/name",
                purpose = "webhook",
                local_service_url = "http://127.0.0.1:47123",
                status = "claimed",
              },
            }
          end,
          list_claims = function() return {} end,
          mark_claims_status = function(status, message)
            _G.claim_status = { status = status, message = message }
          end,
        }
        package.loaded["cloudflare_stable_urls.entities"] = {
          snapshot = function() _G.entity_snapshots = (_G.entity_snapshots or 0) + 1 end,
          upsert = function(row) _G.last_entity_upsert = row end,
        }
        package.loaded["lib.session"] = {
          list = function()
            return _G.live_sessions or {}
          end,
          get = function(uuid)
            return (_G.sessions or {})[uuid]
          end,
        }
        package.loaded["lib.hub"] = {
          get = function()
            return {
              list_owned_sessions = function()
                return _G.owned_sessions or {}
              end,
              prepare_plugin_command = function(_, opts)
                _G.prepared_command = opts
                return "prep-1"
              end,
              create_accessory = function(_, opts)
                _G.created_accessory = opts
                return { session_uuid = "stable-conn-1" }
              end,
            }
          end,
        }
        hub = {
          hub_id = function() return "hub-123" end,
          api_token = function() return "hub-api-token" end,
          is_offline = function() return false end,
        }
        config = {
          server_url = function() return "https://trybotster.test/" end,
          data_dir = function() return "/tmp/botster-test" end,
        }
        _G.broker_response = {
          cloudflare_tunnel = {
            id = 1,
            cloudflare_tunnel_id = "cf-tunnel-1",
            cloudflare_tunnel_name = "botster-hub-123",
            token_version = 7,
            connector_token = "sentinel-cfargotunnel-token",
            status = "ready",
          },
        }
        json = { decode = function(_) return _G.broker_response end }
        http = {
          request = function(opts, cb)
            _G.http_request = opts
            cb({ status = 201, body = "broker-json", headers = {} }, nil)
            return "http-1"
          end,
        }
        secrets = {
          set = function(namespace, key, value)
            _G.secret_set = { namespace = namespace, key = key, value = value }
            _G.secret_value = value
            return true, nil
          end,
          get = function(namespace, key)
            _G.secret_get = { namespace = namespace, key = key }
            return _G.secret_value, nil
          end,
        }
        fs = {
          write_private = function(path, content)
            _G.private_write = { path = path, content = content, mode = 384 }
            return true, nil
          end,
        }
        timer = {
          after = function(seconds, cb)
            _G.retry_timer = { seconds = seconds, cb = cb }
            return "timer-1"
          end,
        }
        log = { warn = function(_) end, info = function(_) end }
    "#,
    )
    .exec()
    .expect("install stubs");

    lua.load(&connector_source)
        .set_name("@cloudflare-stable-urls/cloudflare_stable_urls/connector.lua")
        .exec()
        .expect("load connector");

    lua.load(
        r#"
        local connector = require("cloudflare_stable_urls.connector")
        connector.reconcile("plugin_load")

        assert(http_request.method == "POST")
        assert(http_request.url == "https://trybotster.test/hubs/hub-123/cloudflare_tunnel")
        assert(http_request.headers.Authorization == "Bearer hub-api-token")

        assert(secret_set.namespace == "cloudflare-stable-urls")
        assert(secret_set.key == "connector_token_v7")
        assert(secret_set.value == "sentinel-cfargotunnel-token")
        assert(secret_get.key == "connector_token_v7")
        assert(private_write.content == "sentinel-cfargotunnel-token")
        assert(private_write.path:match("/plugin%-data/cloudflare%-stable%-urls/runtime/token%-7$"))
        assert(config_write == nil)

        assert(prepared_command.command == "cloudflared")
        assert(prepared_command.config_contents == nil)
        assert(prepared_command.config_path == nil)
        assert(prepared_command.context.owner_plugin == "cloudflare-stable-urls")
        assert(prepared_command.context.connector_generation == 7)

        local ok = connector.handle_plugin_command_prepared({
          request_id = prepared_command.request_id,
          command = "/usr/local/bin/cloudflared",
        })
        assert(ok == true)
        assert(created_accessory.session.command == "/usr/local/bin/cloudflared")
        local args = table.concat(created_accessory.session.args, " ")
        assert(args:match("tunnel"))
        assert(not args:find("--config", 1, true))
        assert(args:match("%-%-token%-file"))
        assert(args:match("/token%-7"))
        assert(args:match("botster%-hub%-123"))
        assert(not args:find("--url", 1, true))
        assert(created_accessory.metadata.owner_plugin == "cloudflare-stable-urls")
        assert(created_accessory.metadata.system_kind == "cloudflare_stable_urls_connector")
        assert(created_accessory.metadata.connector_generation == 7)
        assert(not created_accessory.metadata.connector_token)

        connector.handle_agent_created({
          session_uuid = "stable-conn-1",
          metadata = created_accessory.metadata,
        })
        assert(saved_connector.status == "running")
        assert(saved_connector.connector_session_uuid == "stable-conn-1")

        connector.handle_process_exited({ session_uuid = "stable-conn-1", exit_code = 42 })
        assert(saved_connector.status == "reconciling" or saved_connector.status == "unhealthy")
        assert(claim_status.status == "unhealthy")
        assert(retry_timer.seconds == 5)
    "#,
    )
    .exec()
    .expect("exercise connector");
}

#[test]
fn cloudflare_stable_urls_missing_cloudflared_publishes_install_state_without_spawn() {
    let lua = new_lua();
    let connector_source =
        std::fs::read_to_string(plugin_path("cloudflare_stable_urls/connector.lua"))
            .expect("read connector");

    lua.load(
        r#"
        _G.saved_connector = { connector_generation = 0, retry_count = 0 }
        package.loaded["cloudflare_stable_urls.repo"] = {
          connector = function() return _G.saved_connector end,
          save_connector = function(attrs)
            for k, v in pairs(attrs or {}) do _G.saved_connector[k] = v end
            return _G.saved_connector
          end,
          active_claims = function() return {} end,
          list_claims = function() return {} end,
          mark_claims_status = function(status, message)
            _G.claim_status = { status = status, message = message }
          end,
        }
        package.loaded["cloudflare_stable_urls.entities"] = {
          snapshot = function() _G.entity_snapshots = (_G.entity_snapshots or 0) + 1 end,
        }
        package.loaded["lib.session"] = { list = function() return {} end }
        package.loaded["lib.hub"] = {
          get = function()
            return {
              list_owned_sessions = function() return {} end,
              prepare_plugin_command = function(_, opts)
                _G.prepared_command = opts
              end,
              create_accessory = function(_, opts)
                _G.created_accessory = opts
                return { session_uuid = "should-not-spawn" }
              end,
            }
          end,
        }
        hub = {
          hub_id = function() return "hub-123" end,
          api_token = function() return "hub-api-token" end,
          is_offline = function() return false end,
        }
        config = {
          server_url = function() return "https://trybotster.test" end,
          data_dir = function() return "/tmp/botster-test" end,
        }
        json = {
          decode = function(_)
            return {
              cloudflare_tunnel = {
                cloudflare_tunnel_id = "cf-tunnel-1",
                cloudflare_tunnel_name = "botster-hub-123",
                token_version = 8,
                connector_token = "sentinel-cfargotunnel-token",
              },
            }
          end,
        }
        http = {
          request = function(_, cb)
            cb({ status = 201, body = "broker-json" }, nil)
            return "http-1"
          end,
        }
        secrets = {
          set = function(_, _, value) _G.secret_value = value return true, nil end,
          get = function() return _G.secret_value, nil end,
        }
        fs = { write_private = function(_, _) return true, nil end }
        timer = {
          after = function(seconds, cb)
            _G.retry_timer = { seconds = seconds, cb = cb }
          end,
        }
        log = { warn = function(_) end, info = function(_) end }
    "#,
    )
    .exec()
    .expect("install stubs");

    lua.load(&connector_source)
        .set_name("@cloudflare-stable-urls/cloudflare_stable_urls/connector.lua")
        .exec()
        .expect("load connector");

    lua.load(
        r#"
        local connector = require("cloudflare_stable_urls.connector")
        connector.reconcile("plugin_load")
        assert(prepared_command.request_id ~= nil)

        local ok, install_url = connector.handle_plugin_command_prepared({
          request_id = prepared_command.request_id,
          error = "cloudflared not found",
          error_kind = "command_missing",
        })
        assert(ok == true)
        assert(install_url:match("developers.cloudflare.com"))
        assert(created_accessory == nil)
        assert(saved_connector.status == "reconciling")
        assert(saved_connector.message:match("cloudflared to be installed"))
        assert(retry_timer.seconds == 5)
    "#,
    )
    .exec()
    .expect("exercise missing binary path");
}

#[test]
fn cloudflare_stable_urls_reconcile_reuses_one_current_connector_and_closes_stale_generations() {
    let lua = new_lua();
    let connector_source =
        std::fs::read_to_string(plugin_path("cloudflare_stable_urls/connector.lua"))
            .expect("read connector");

    lua.load(
        r#"
        _G.saved_connector = { connector_generation = 0, retry_count = 0 }
        _G.current_session = {
          session_uuid = "current-conn",
          status = "running",
          metadata = {
            owner_plugin = "cloudflare-stable-urls",
            system_kind = "cloudflare_stable_urls_connector",
            connector_generation = 9,
          },
          close = function(_) _G.current_closed = true end,
        }
        _G.stale_session = {
          session_uuid = "stale-conn",
          status = "hidden",
          metadata = {
            owner_plugin = "cloudflare-stable-urls",
            system_kind = "cloudflare_stable_urls_connector",
            connector_generation = 8,
          },
          close = function(_) _G.stale_closed = true end,
        }
        package.loaded["cloudflare_stable_urls.repo"] = {
          connector = function() return _G.saved_connector end,
          save_connector = function(attrs)
            for k, v in pairs(attrs or {}) do _G.saved_connector[k] = v end
            return _G.saved_connector
          end,
          active_claims = function() return {} end,
          list_claims = function() return {} end,
          mark_claims_status = function(status, message)
            _G.claim_status = { status = status, message = message }
          end,
        }
        package.loaded["cloudflare_stable_urls.entities"] = {
          snapshot = function() _G.entity_snapshots = (_G.entity_snapshots or 0) + 1 end,
        }
        package.loaded["lib.session"] = { list = function() return {} end }
        package.loaded["lib.hub"] = {
          get = function()
            return {
              list_owned_sessions = function() return { _G.current_session, _G.stale_session } end,
              prepare_plugin_command = function(_, opts) _G.prepared_command = opts end,
              create_accessory = function(_, opts) _G.created_accessory = opts end,
            }
          end,
        }
        hub = {
          hub_id = function() return "hub-123" end,
          api_token = function() return "hub-api-token" end,
          is_offline = function() return false end,
        }
        config = {
          server_url = function() return "https://trybotster.test" end,
          data_dir = function() return "/tmp/botster-test" end,
        }
        json = {
          decode = function(_)
            return {
              cloudflare_tunnel = {
                cloudflare_tunnel_id = "cf-tunnel-1",
                cloudflare_tunnel_name = "botster-hub-123",
                token_version = 9,
                connector_token = "sentinel-cfargotunnel-token",
              },
            }
          end,
        }
        http = {
          request = function(_, cb)
            cb({ status = 201, body = "broker-json" }, nil)
            return "http-1"
          end,
        }
        secrets = {
          set = function(_, _, value) _G.secret_value = value return true, nil end,
          get = function() return _G.secret_value, nil end,
        }
        fs = { write_private = function(_, _) return true, nil end }
        log = { warn = function(_) end, info = function(_) end }
    "#,
    )
    .exec()
    .expect("install stubs");

    lua.load(&connector_source)
        .set_name("@cloudflare-stable-urls/cloudflare_stable_urls/connector.lua")
        .exec()
        .expect("load connector");

    lua.load(
        r#"
        require("cloudflare_stable_urls.connector").reconcile("plugin_load")
        assert(saved_connector.status == "running")
        assert(saved_connector.connector_session_uuid == "current-conn")
        assert(stale_closed == true)
        assert(current_closed == nil)
        assert(prepared_command == nil)
        assert(created_accessory == nil)
        assert(claim_status.status == "claimed")
    "#,
    )
    .exec()
    .expect("exercise live reconcile");
}

#[test]
fn cloudflare_stable_urls_stale_exit_is_fenced_and_real_entities_omit_secret_fields() {
    let lua = new_lua();

    lua.load(
        r#"
        _G.saved_connector = {
          connector_session_uuid = "current-conn",
          connector_generation = 4,
          token_version = 4,
          token_secret_key = "connector_token_v4",
          token_path = "/tmp/token-4",
          config_path = "/tmp/config.yml",
          status = "running",
          message = "running",
          updated_at = 12345,
        }
        _G.claim_rows = {}
        _G.claim_rows[1] = {
            id = "claim-1",
            hostname = "hook.example.test",
            public_url = "https://hook.example.test",
            owner_plugin = "github",
            owner_key = "repo:owner/name",
            purpose = "webhook",
            local_service_url = "http://127.0.0.1:47123",
            status = "claimed",
            message = nil,
        }
        package.loaded["cloudflare_stable_urls.repo"] = {
          connector = function() return _G.saved_connector end,
          save_connector = function(attrs)
            for k, v in pairs(attrs or {}) do _G.saved_connector[k] = v end
            return _G.saved_connector
          end,
          list_claims = function() return _G.claim_rows end,
          active_claims = function() return _G.claim_rows end,
          mark_claims_status = function(status, message)
            _G.claim_status = { status = status, message = message }
          end,
        }
        package.loaded["lib.hub"] = {
          get = function()
            return {
              list_owned_sessions = function() return {} end,
              entity_snapshot = function(_, entity_type, rows, opts)
                _G.entity_snapshot = { entity_type = entity_type, rows = rows, opts = opts }
              end,
            }
          end,
        }
        package.loaded["lib.session"] = { list = function() return {} end }
        log = { warn = function(_) end, info = function(_) end }
    "#,
    )
    .exec()
    .expect("install stubs");

    lua.load(
        r#"
        assert(#claim_rows == 1)
        assert(#require("cloudflare_stable_urls.repo").list_claims() == 1)
        local connector = require("cloudflare_stable_urls.connector")
        local handled = connector.handle_process_exited({ session_uuid = "stale-conn", exit_code = 2 })
        assert(handled == false)
        assert(saved_connector.status == "running")
        assert(saved_connector.connector_session_uuid == "current-conn")

        require("cloudflare_stable_urls.entities").snapshot()
        assert(entity_snapshot ~= nil, "missing entity snapshot")
        assert(#entity_snapshot.rows == 1, "rows=" .. tostring(#entity_snapshot.rows))
        local row = entity_snapshot.rows[1]
        assert(row.hostname == "hook.example.test", "hostname=" .. tostring(row.hostname))
        assert(tostring(row.token_version) == "4", "token_version=" .. tostring(row.token_version))
        assert(row.token_secret_key == nil, "token_secret_key leaked")
        assert(row.token_path == nil, "token_path leaked")
        assert(row.config_path == nil, "config_path leaked")
        assert(row.connector_token == nil, "connector_token leaked")
        assert(tostring(row):find("connector_token_v4", 1, true) == nil)
    "#,
    )
    .exec()
    .expect("exercise stale exit and entities");
}

#[test]
fn cloudflare_stable_urls_init_wires_production_reconcile_and_exit_handlers() {
    let init_source = std::fs::read_to_string(plugin_path("init.lua")).expect("read init");
    let lua = new_lua();

    lua.load(
        r#"
        _G.db_eval_calls = {}
        _G.claim_rows = {}
        plugin = {
          db = function(_)
            return {
              eval = function(_, sql, params)
                _G.db_eval_calls[#_G.db_eval_calls + 1] = { sql = sql, params = params }
                if sql:find("SELECT %* FROM connector_state") then
                  return _G.connector_row and { _G.connector_row } or {}
                end
                if sql:find("INSERT INTO connector_state") then
                  _G.connector_row = {
                    id = "hub",
                    status = params[10],
                    message = params[11],
                    retry_count = params[12],
                    updated_at = params[13],
                  }
                  return {}
                end
                if sql:find("SELECT %* FROM stable_url_claims") then
                  return _G.claim_rows
                end
                return {}
              end,
            }
          end,
        }
        package.loaded["lib.entity_broadcast"] = {
          register = function(entity_type, opts)
            _G.registered_entity = { entity_type = entity_type, opts = opts }
          end,
        }
        package.loaded["lib.hub"] = {
          get = function()
            return {
              entity_snapshot = function(_, entity_type, rows, opts)
                _G.entity_snapshot = { entity_type = entity_type, rows = rows, opts = opts }
              end,
              list_owned_sessions = function() return {} end,
              prepare_plugin_command = function(_, opts)
                _G.prepared_command = opts
                return "prep-1"
              end,
            }
          end,
        }
        package.loaded["lib.session"] = { list = function() return {} end }
        events = {
          on = function(name, cb)
            _G.events_registered = _G.events_registered or {}
            _G.events_registered[name] = cb
            return "sub-" .. name
          end,
          off = function(_) end,
        }
        hooks = {
          on = function(name, key, cb)
            _G.hooks_registered = _G.hooks_registered or {}
            _G.hooks_registered[name] = { key = key, cb = cb }
          end,
        }
        hub = {
          hub_id = function() return "hub-123" end,
          api_token = function() return "hub-api-token" end,
          is_offline = function() return false end,
        }
        config = {
          server_url = function() return "https://trybotster.test/" end,
          data_dir = function() return "/tmp/botster-test" end,
        }
        http = {
          request = function(opts, _)
            _G.http_request = opts
            return "http-1"
          end,
        }
        log = { warn = function(_) end, info = function(_) end }
    "#,
    )
    .exec()
    .expect("install init stubs");

    lua.load(&init_source)
        .set_name("@cloudflare-stable-urls/init.lua")
        .exec()
        .expect("execute init");

    lua.load(
        r#"
        assert(events_registered.plugin_command_prepared ~= nil)
        assert(events_registered.process_exited ~= nil)
        assert(hooks_registered.agent_created.key == "cloudflare_stable_urls.connector_created")
        assert(http_request.method == "POST")
        assert(http_request.url == "https://trybotster.test/hubs/hub-123/cloudflare_tunnel")
        assert(registered_entity.entity_type == "cloudflare-stable-urls.stable_url")
        assert(entity_snapshot ~= nil)
    "#,
    )
    .exec()
    .expect("assert init wiring");
}
