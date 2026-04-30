//! Lua tests for internal command ingress through lib.client.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    reason = "test-code brevity"
)]

use std::path::PathBuf;

use botster::lua::primitives::log;
use mlua::Lua;

fn lua_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua")
}

fn new_lua() -> Lua {
    let lua = Lua::new();
    log::register(&lua).expect("register log");
    botster::lua::primitives::hook_timeout::register(&lua).expect("register hook timeout");

    let dir = lua_src_dir();
    lua.load(format!(
        r#"
        package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path
        hooks = require("hub.hooks")
        "#,
        dir = dir.display()
    ))
    .exec()
    .expect("configure lua");

    lua
}

#[test]
fn internal_dispatch_enters_client_command_hooks() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local commands = require("lib.commands")
            local internal = require("lib.internal_client")

            local seen = {}
            hooks.intercept("before_hub_command", "test.before_hub", function(command)
              seen.before_hub = command.type
              return command
            end)
            hooks.intercept("before_command", "test.before_command", function(ctx)
              seen.before_command = ctx.type
              seen.peer_id = ctx.client.peer_id
              return ctx
            end)
            hooks.on("after_hub_command", "test.after_command", function(ctx)
              seen.after_command = ctx.command
              seen.success = ctx.success
            end)

            commands.register("demo_internal", function(client, sub_id, command)
              seen.handler = command.payload
              client:send({
                subscriptionId = sub_id,
                type = "demo:response",
                payload = command.payload,
              })
            end)

            local result = internal.dispatch("audit", {
              type = "demo_internal",
              payload = "ok",
            })

            assert(seen.before_hub == "demo_internal")
            assert(seen.before_command == "demo_internal")
            assert(seen.after_command == "demo_internal")
            assert(seen.success == true)
            assert(seen.handler == "ok")
            assert(seen.peer_id == "internal:audit")
            assert(#result.frames == 1)
            assert(result.frames[1].type == "demo:response")
            return "ok"
            "#,
        )
        .eval()
        .expect("internal dispatch should flow through client.lua");

    assert_eq!(result, "ok");
}

#[test]
fn create_agent_creates_even_when_matching_agent_exists() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local spawned = 0

            config = { data_dir = function() return "/tmp/botster-test" end }
            spawn_targets = {
              get = function(id)
                if id == "target-1" then
                  return { id = "target-1", path = "/repo", enabled = true }
                end
              end,
              inspect = function(_)
                return { repo_name = "demo/repo", is_git_repo = true, repo_root = "/repo" }
              end,
            }

            local existing = {
              session_uuid = "sess-existing",
              metadata = { issue_number = 42 },
              session = {
                send_message = function(_, text)
                  error("create_agent should not notify existing sessions: " .. tostring(text))
                end,
              },
            }

            package.loaded["lib.agent"] = {
              list = function() return { existing } end,
              find_by_workspace = function(name)
                if name == "demo/repo#42" then return { existing } end
                return {}
              end,
              find_by_meta = function(_, _) return {} end,
            }
            package.loaded["handlers.agents"] = {
              handle_create_agent = function()
                spawned = spawned + 1
                return { session_uuid = "sess-new" }
              end,
            }

            require("handlers.commands")
            local result = require("lib.internal_client").dispatch("test", {
              type = "create_agent",
              request_id = "req-create",
              issue_or_branch = "42",
              prompt = "Please look at this",
              target_id = "target-1",
              metadata = {
                issue_number = 42,
                workspace = "demo/repo#42",
              },
            })

            assert(spawned == 1)
            assert(result.frames[1].type == "command_response")
            assert(result.frames[1].ok == true)
            assert(result.frames[1].session_uuid == "sess-new")
            return "ok"
            "#,
        )
        .eval()
        .expect("create_agent should create a new session");

    assert_eq!(result, "ok");
}

#[test]
fn hub_command_channel_create_agent_webhook_dispatches_spawn_command() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local callback = nil
            local dispatched = {}

            config = { env = function() return "development" end }
            hub = {
              is_offline = function() return false end,
              server_id = function() return "hub-1" end,
              handle_signaling_message = function() error("unexpected signal") end,
            }
            timer = {
              every = function() return "timer-1" end,
              cancel = function() end,
            }
            events = {
              on = function() return "event-1" end,
              off = function() end,
            }
            action_cable = {
              connect = function() return "conn-1" end,
              subscribe = function(_, _, _, cb)
                callback = cb
                return "channel-1"
              end,
              unsubscribe = function() end,
              perform = function() end,
            }
            spawn_targets = {
              get = function(id)
                if id == "target-1" then
                  return { id = "target-1", path = "/repo", enabled = true }
                end
              end,
              inspect = function(_)
                return { repo_name = "owner/repo", is_git_repo = true, repo_root = "/repo" }
              end,
            }

            package.loaded["hub.state"] = {
              get = function() return {} end,
            }
            package.loaded["lib.agent"] = {
              find_by_workspace = function()
                error("HubCommandChannel create_agent ingress should not dedupe by existing session")
              end,
            }
            package.loaded["lib.internal_client"] = {
              dispatch = function(source, command)
                dispatched[#dispatched + 1] = { source = source, command = command }
              end,
            }

            require("handlers.hub_commands")
            assert(type(callback) == "function")
            callback({
              type = "message",
              event_type = "create_agent",
              payload = {
                target_id = "target-1",
                target_path = "/repo",
                target_repo = "owner/repo",
                issue_number = 42,
                prompt = "Please inspect this",
                issue_url = "https://github.com/owner/repo/issues/42",
              },
            }, "channel-1")

            assert(#dispatched == 1)
            assert(dispatched[1].source == "hub_commands")
            assert(dispatched[1].command.type == "create_agent")
            assert(dispatched[1].command.issue_or_branch == "42")
            assert(dispatched[1].command.prompt == "Please inspect this")
            assert(dispatched[1].command.target_id == "target-1")
            assert(dispatched[1].command.metadata.workspace == "owner/repo#42")
            return "ok"
            "#,
        )
        .eval()
        .expect("HubCommandChannel create_agent webhook should dispatch explicit spawn command");

    assert_eq!(result, "ok");
}

#[test]
fn command_failures_are_observable_as_response_frames() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            config = { data_dir = function() return "/tmp/botster-test" end }
            package.loaded["lib.workspace_store"] = {
              rename_workspace = function() return false end,
            }

            require("handlers.commands")
            local result = require("lib.internal_client").dispatch("test", {
              type = "rename_workspace",
              request_id = "req-rename",
              workspace_id = "ws-1",
              new_name = "New",
            })

            assert(result.frames[1].type == "command_response")
            assert(result.frames[1].request_id == "req-rename")
            assert(result.frames[1].ok == false)
            assert(result.frames[1].error == "failed to rename workspace")
            return "ok"
            "#,
        )
        .eval()
        .expect("failure should be returned as command_response");

    assert_eq!(result, "ok");
}

#[test]
fn update_session_requires_an_actual_field() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local session = {
              session_uuid = "sess-existing",
              update = function()
                error("update should not be called")
              end,
            }
            package.loaded["lib.agent"] = {
              get = function(id)
                if id == "sess-existing" then return session end
              end,
            }

            require("handlers.commands")
            local result = require("lib.internal_client").dispatch("test", {
              type = "update_session",
              request_id = "req-update",
              session_uuid = "sess-existing",
            })

            assert(result.frames[1].type == "command_response")
            assert(result.frames[1].request_id == "req-update")
            assert(result.frames[1].ok == false)
            assert(result.frames[1].error == "label or task is required")
            return "ok"
            "#,
        )
        .eval()
        .expect("update_session should reject no-op updates");

    assert_eq!(result, "ok");
}

#[test]
fn thrown_command_errors_are_observable_as_response_frames() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local commands = require("lib.commands")
            local internal = require("lib.internal_client")

            local after_success = nil
            hooks.on("after_hub_command", "test.after_throwing_command", function(ctx)
              after_success = ctx.success
            end)

            commands.register("throwing_command", function()
              error("boom from handler")
            end)

            local result = internal.dispatch("test", {
              type = "throwing_command",
              request_id = "req-throw",
            })

            assert(result.frames[1].type == "command_response")
            assert(result.frames[1].request_id == "req-throw")
            assert(result.frames[1].ok == false)
            assert(result.frames[1].error:match("boom from handler"))
            assert(after_success == false)
            return "ok"
            "#,
        )
        .eval()
        .expect("throwing handler should return command_response");

    assert_eq!(result, "ok");
}

#[test]
fn internal_dispatch_restores_synthetic_subscription() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local commands = require("lib.commands")
            local internal = require("lib.internal_client")

            commands.register("subscription_probe", function(client, sub_id, _command)
              assert(client.subscriptions[sub_id] ~= nil)
              client:send({
                subscriptionId = sub_id,
                type = "probe_response",
              })
            end)

            local result = internal.dispatch("test-subscriptions", {
              type = "subscription_probe",
            }, {
              subscription_id = "dynamic-subscription",
            })

            assert(result.frames[1].type == "probe_response")
            assert(result.client.subscriptions["dynamic-subscription"] == nil)
            return "ok"
            "#,
        )
        .eval()
        .expect("internal dispatch should clean synthetic subscriptions");

    assert_eq!(result, "ok");
}
