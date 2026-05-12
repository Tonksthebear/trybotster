//! Lua tests for scoped plugin notification policies.

#![expect(clippy::unwrap_used, clippy::expect_used, reason = "test-code brevity")]

use std::path::PathBuf;

use botster::lua::primitives::log;
use mlua::{Lua, LuaSerdeExt, Value};
use serde_json::{json, Value as JsonValue};

fn lua_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua")
}

fn new_lua() -> Lua {
    let lua = Lua::new();
    log::register(&lua).expect("register log");
    botster::lua::primitives::hook_timeout::register(&lua).expect("register hook timeout");

    let dir = lua_src_dir();
    let setup = format!(
        "package.path = \"{dir}/?.lua;{dir}/?/init.lua;\" .. package.path",
        dir = dir.display()
    );
    lua.load(&setup).exec().expect("update package.path");
    lua
}

fn install_connection_harness(lua: &Lua) {
    lua.load(
        r#"
        hooks = require("hub.hooks")
        require("lib.notifications")._reset_for_tests()

        __pushes = {}
        __frames = {}
        __suppressed = {}

        timer = {
          every = function() return "timer-output-activity" end,
          after_idle = function(_id, _delay, fn) fn() end,
          cancel = function() end,
        }
        events = {
          on = function() return "event-sub" end,
          off = function() end,
        }
        push = {
          send = function(payload)
            __pushes[#__pushes + 1] = payload
          end,
        }
        hub = {
          server_id = function() return "hub-test" end,
          write_pty = function() end,
          get_worktrees = function() return {} end,
        }

        local agent = {
          session_uuid = "sess-owned",
          repo = "owner/repo",
          owner_plugin = "pipeline",
          surface = "pipeline",
          visibility = "plugin",
          notification = false,
          metadata = {},
          get_meta = function(self, key) return self.metadata[key] end,
          update = function(self, fields)
            for k, v in pairs(fields or {}) do self[k] = v end
          end,
        }

        package.loaded["lib.agent"] = {
          get = function(session_uuid)
            if session_uuid == agent.session_uuid then return agent end
            return nil
          end,
          list = function() return { agent } end,
          __agent = agent,
        }
        package.loaded["lib.session"] = {
          get = function(session_uuid)
            if session_uuid == agent.session_uuid then return agent end
            return nil
          end,
          list = function() return {} end,
          all_info = function() return {} end,
          is_system_session = function() return false end,
        }
        package.loaded["lib.terminal_clients"] = {
          is_any_focused = function() return false end,
          set_focused = function() end,
          get_focused_sessions = function() return {} end,
        }
        package.loaded["lib.entity_model"] = {
          upsert_session_workspace = function() end,
          publish_session = function() end,
          remove_session = function() end,
          upsert_workspace = function() end,
          patch_session = function() end,
          upsert_hub = function() end,
          remove_session_action = function() end,
        }
        package.loaded["lib.session_actions"] = {
          publish_for_session = function() end,
          action_ids = function() return {} end,
          purge_session = function() end,
        }
        package.loaded["lib.entity_broadcast"] = {
          set_broadcaster = function() end,
        }
        package.loaded["lib.surfaces"] = {
          path = function(surface, pattern, params)
            return "/" .. surface .. "/sessions/" .. params.session_uuid
          end,
        }
        package.loaded["lib.hub"] = {
          get = function()
            return {
              list_workspaces = function() return {} end,
            }
          end,
        }

        hooks.on("pty_notification_suppressed", "test.capture_suppressed", function(info)
          __suppressed[#__suppressed + 1] = info
        end)

        local connections = require("handlers.connections")
        connections.register_client("peer-1", {
          transport = { type = "test" },
          subscriptions = { sub = { channel = "hub" } },
          send = function(self, frame)
            __frames[#__frames + 1] = frame
          end,
          disconnect = function() end,
        })
        "#,
    )
    .exec()
    .expect("install connection harness");
}

#[test]
fn scoped_claim_decides_matching_session_only() {
    let lua = new_lua();

    let result: JsonValue = lua
        .load(
            r#"
            local notifications = require("lib.notifications")
            notifications._reset_for_tests()

            notifications.claim({
              name = "demo.owner",
              scope = { session_uuid = "sess-owned" },
              handler = function(intent)
                return {
                  core = "replace",
                  reason = "claimed",
                  custom = {
                    title = "Owned",
                    body = intent.message,
                    push = false,
                  },
                }
              end,
            })

            return {
              owned = notifications.evaluate({
                session_uuid = "sess-owned",
                message = "hello",
              }),
              other = notifications.evaluate({
                session_uuid = "sess-other",
                message = "hello",
              }),
            }
            "#,
        )
        .eval::<Value>()
        .map(|value| lua.from_value::<JsonValue>(value).unwrap())
        .unwrap();

    assert_eq!(result["owned"]["core"], json!("replace"));
    assert_eq!(result["owned"]["reason"], json!("claimed"));
    assert_eq!(result["owned"]["custom"]["title"], json!("Owned"));
    assert_eq!(result["owned"]["custom"]["body"], json!("hello"));
    assert_eq!(result["owned"]["owner"], json!("demo.owner"));

    assert_eq!(result["other"]["core"], json!("default"));
    assert!(result["other"].get("owner").is_none());
}

#[test]
fn observers_run_without_changing_decision() {
    let lua = new_lua();

    let result: JsonValue = lua
        .load(
            r#"
            local notifications = require("lib.notifications")
            notifications._reset_for_tests()
            local seen = {}

            notifications.observe({
              name = "demo.observe_all",
              scope = { all_sessions = true },
              capabilities = { "notifications.global_observe" },
              phase = "both",
              handler = function(phase, intent, decision)
                seen[#seen + 1] = {
                  phase = phase,
                  session_uuid = intent.session_uuid,
                  core = decision and decision.core or nil,
                }
              end,
            })

            local decision = notifications.evaluate({
              session_uuid = "sess-a",
              message = "permission required",
            })
            notifications.notify_observers("after", {
              session_uuid = "sess-a",
              message = "permission required",
            }, decision)

            return { decision = decision, seen = seen }
            "#,
        )
        .eval::<Value>()
        .map(|value| lua.from_value::<JsonValue>(value).unwrap())
        .unwrap();

    assert_eq!(result["decision"]["core"], json!("default"));
    assert_eq!(result["seen"][0]["phase"], json!("before"));
    assert_eq!(result["seen"][0]["session_uuid"], json!("sess-a"));
    assert_eq!(result["seen"][1]["phase"], json!("after"));
    assert_eq!(result["seen"][1]["core"], json!("default"));
}

#[test]
fn global_scopes_require_explicit_capabilities() {
    let lua = new_lua();

    let result: JsonValue = lua
        .load(
            r#"
            local notifications = require("lib.notifications")
            notifications._reset_for_tests()

            local observe_ok, observe_err = pcall(function()
              notifications.observe({
                name = "demo.observe_denied",
                scope = { all_sessions = true },
                handler = function() end,
              })
            end)

            local claim_ok, claim_err = pcall(function()
              notifications.claim({
                name = "demo.claim_denied",
                scope = { all_sessions = true },
                handler = function() return { core = "default" } end,
              })
            end)

            local observe_allowed = pcall(function()
              notifications.observe({
                name = "demo.observe_allowed",
                scope = { all_sessions = true },
                capabilities = { "notifications.global_observe" },
                handler = function() end,
              })
            end)

            local claim_allowed = pcall(function()
              notifications.claim({
                name = "demo.claim_allowed",
                scope = { all_sessions = true },
                capabilities = { "notifications.global_claim" },
                handler = function() return { core = "default" } end,
              })
            end)

            return {
              observe_ok = observe_ok,
              observe_err = tostring(observe_err),
              claim_ok = claim_ok,
              claim_err = tostring(claim_err),
              observe_allowed = observe_allowed,
              claim_allowed = claim_allowed,
            }
            "#,
        )
        .eval::<Value>()
        .map(|value| lua.from_value::<JsonValue>(value).unwrap())
        .unwrap();

    assert_eq!(result["observe_ok"], json!(false));
    assert!(result["observe_err"]
        .as_str()
        .unwrap()
        .contains("notifications.global_observe"));
    assert_eq!(result["claim_ok"], json!(false));
    assert!(result["claim_err"]
        .as_str()
        .unwrap()
        .contains("notifications.global_claim"));
    assert_eq!(result["observe_allowed"], json!(true));
    assert_eq!(result["claim_allowed"], json!(true));
}

#[test]
fn connection_flow_default_delivery_sets_badge_push_and_transient() {
    let lua = new_lua();
    install_connection_harness(&lua);

    let result: JsonValue = lua
        .load(
            r#"
            hooks.notify("_pty_notification_raw", {
              session_uuid = "sess-owned",
              session_name = "agent",
              type = "osc9",
              message = "permission required",
            })

            return {
              notification = require("lib.agent").__agent.notification,
              push = __pushes[1],
              frame = __frames[1],
              suppressed_count = #__suppressed,
            }
            "#,
        )
        .eval::<Value>()
        .map(|value| lua.from_value::<JsonValue>(value).unwrap())
        .unwrap();

    assert_eq!(result["notification"], json!(true));
    assert_eq!(result["push"]["title"], json!("repo"));
    assert_eq!(result["push"]["body"], json!("permission required"));
    assert_eq!(result["push"]["kind"], json!("agent_alert"));
    assert_eq!(result["frame"]["type"], json!("transient_event"));
    assert_eq!(result["frame"]["event_type"], json!("pty_notification"));
    assert_eq!(result["frame"]["body"], json!("permission required"));
    assert_eq!(result["suppressed_count"], json!(0));
}

#[test]
fn connection_flow_default_suppression_skips_delivery() {
    let lua = new_lua();
    install_connection_harness(&lua);

    let result: JsonValue = lua
        .load(
            r#"
            hooks.notify("_pty_notification_raw", {
              session_uuid = "sess-owned",
              session_name = "agent",
              type = "osc9",
              message = "routine output",
            })

            return {
              notification = require("lib.agent").__agent.notification,
              push_count = #__pushes,
              frame_count = #__frames,
              suppressed_count = #__suppressed,
              policy = __suppressed[1] and __suppressed[1].notification_policy or nil,
            }
            "#,
        )
        .eval::<Value>()
        .map(|value| lua.from_value::<JsonValue>(value).unwrap())
        .unwrap();

    assert_eq!(result["notification"], json!(false));
    assert_eq!(result["push_count"], json!(0));
    assert_eq!(result["frame_count"], json!(0));
    assert_eq!(result["suppressed_count"], json!(1));
    assert_eq!(result["policy"], json!("suppressed"));
}

#[test]
fn connection_flow_claim_can_suppress_or_replace_delivery() {
    let lua = new_lua();
    install_connection_harness(&lua);

    let result: JsonValue = lua
        .load(
            r#"
            local notifications = require("lib.notifications")
            notifications.claim({
              name = "pipeline.suppress",
              scope = { owner_plugin = "pipeline" },
              handler = function(intent)
                if intent.message == "handled elsewhere" then
                  return { core = "suppress", reason = "pipeline_handled" }
                end
                return {
                  core = "replace",
                  reason = "pipeline_replaced",
                  custom = {
                    kind = "pipeline_alert",
                    title = "Pipeline",
                    body = "custom: " .. intent.message,
                    push = true,
                    transient = true,
                    badge = true,
                  },
                }
              end,
            })

            hooks.notify("_pty_notification_raw", {
              session_uuid = "sess-owned",
              session_name = "agent",
              type = "osc9",
              message = "handled elsewhere",
            })

            hooks.notify("_pty_notification_raw", {
              session_uuid = "sess-owned",
              session_name = "agent",
              type = "osc9",
              message = "routine output",
            })

            return {
              notification = require("lib.agent").__agent.notification,
              suppressed_count = #__suppressed,
              suppressed_policy = __suppressed[1] and __suppressed[1].notification_policy or nil,
              push_count = #__pushes,
              push = __pushes[1],
              frame = __frames[1],
            }
            "#,
        )
        .eval::<Value>()
        .map(|value| lua.from_value::<JsonValue>(value).unwrap())
        .unwrap();

    assert_eq!(result["suppressed_count"], json!(1));
    assert_eq!(result["suppressed_policy"], json!("pipeline_handled"));
    assert_eq!(result["notification"], json!(true));
    assert_eq!(result["push_count"], json!(1));
    assert_eq!(result["push"]["kind"], json!("pipeline_alert"));
    assert_eq!(result["push"]["title"], json!("Pipeline"));
    assert_eq!(result["push"]["body"], json!("custom: routine output"));
    assert_eq!(result["frame"]["event_type"], json!("pty_notification"));
    assert_eq!(result["frame"]["title"], json!("Pipeline"));
}
