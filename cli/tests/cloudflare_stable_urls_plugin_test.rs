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
          write = function(path, content)
            _G.config_write = { path = path, content = content }
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

        assert(config_write.path:match("/plugin%-data/cloudflare%-stable%-urls/runtime/config%.yml$"))
        assert(config_write.content:match("tunnel: cf%-tunnel%-1"))
        assert(config_write.content:match("token%-file: .-/token%-7"))
        assert(config_write.content:match("hostname: hook%.example%.test"))
        assert(config_write.content:match("service: http://127%.0%.0%.1:47123"))
        assert(config_write.content:match("service: http_status:404"))
        assert(not config_write.content:find("sentinel-cfargotunnel-token", 1, true))

        assert(prepared_command.command == "cloudflared")
        assert(prepared_command.config_contents == config_write.content)
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
        assert(args:match("%-%-config"))
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
fn cloudflare_stable_urls_init_wires_production_reconcile_and_exit_handlers() {
    let init_source = std::fs::read_to_string(plugin_path("init.lua")).expect("read init");

    assert!(
        init_source.contains("pcall(connector.reconcile, \"plugin_load\")"),
        "init.lua must call the production reconcile entry point on plugin load"
    );
    assert!(
        init_source.contains("events.on(\"process_exited\""),
        "init.lua must register process_exited handling for connector lifecycle"
    );
    assert!(
        init_source.contains("events.on(\"plugin_command_prepared\""),
        "init.lua must register prepared-command handling for connector spawn"
    );
    assert!(
        init_source.contains("hooks.on(\"agent_created\""),
        "init.lua must observe connector creation to publish running state"
    );
}
