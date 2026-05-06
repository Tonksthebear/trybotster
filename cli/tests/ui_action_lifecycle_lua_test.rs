//! Lua tests for generic ui_action lifecycle result semantics.

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
        "#,
        dir = dir.display()
    ))
    .exec()
    .expect("configure lua");

    lua
}

#[test]
fn handler_result_table_supplies_message_and_error_metadata() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local action = require("lib.action")
            action._reset_for_tests()

            action.on("demo.ok", "test", function()
              return action.result{
                message = "Saved",
                navigate = { label = "Open", path = "/pipelines/tickets/t1" },
              }
            end)
            action.on("demo.err", "test", function()
              return action.result{ ok = false, error = "No target" }
            end)

            local ok_result = action.dispatch({ id = "demo.ok" }, {})
            assert(ok_result.handled == true)
            assert(ok_result.ok == true)
            assert(ok_result.message == "Saved")
            assert(ok_result.navigate.label == "Open")
            assert(ok_result.navigate.path == "/pipelines/tickets/t1")

            local err_result = action.dispatch({ id = "demo.err" }, {})
            assert(err_result.handled == true)
            assert(err_result.ok == false)
            assert(err_result.error == "No target")
            return "ok"
            "#,
        )
        .eval()
        .expect("result tables should attach metadata");

    assert_eq!(result, "ok");
}

#[test]
fn handler_exception_is_reported_while_other_handlers_continue() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local action = require("lib.action")
            action._reset_for_tests()

            local ran_after = false
            action.on("demo.raise", "bad", function()
              error("boom")
            end)
            action.on("demo.raise", "after", function()
              ran_after = true
              return action.HANDLED
            end)

            local result = action.dispatch({ id = "demo.raise" }, {})
            assert(ran_after == true)
            assert(result.handled == true)
            assert(result.ok == false)
            assert(result.via == "handler")
            assert(string.find(result.error, "boom", 1, true) ~= nil)
            return "ok"
            "#,
        )
        .eval()
        .expect("handler exception should be isolated");

    assert_eq!(result, "ok");
}

#[test]
fn plugin_owned_handler_timeout_is_reported_while_other_handlers_continue() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local action = require("lib.action")
            action._reset_for_tests()

            _G._loading_plugin_key = "slow-plugin"
            action.on("demo.timeout", "slow", function()
              while true do end
            end, { timeout_ms = 5 })
            _G._loading_plugin_key = nil

            local ran_after = false
            action.on("demo.timeout", "after", function()
              ran_after = true
              return action.HANDLED
            end)

            local result = action.dispatch({ id = "demo.timeout" }, {})
            assert(ran_after == true)
            assert(result.handled == true)
            assert(result.ok == false)
            assert(result.via == "handler")
            assert(string.find(result.error, "timeout", 1, true) ~= nil)
            return "ok"
            "#,
        )
        .eval()
        .expect("plugin handler timeout should be isolated");

    assert_eq!(result, "ok");
}

#[test]
fn fallback_route_reports_fallback_via() {
    let lua = new_lua();

    let result: String = lua
        .load(
            r#"
            local action = require("lib.action")
            action._reset_for_tests()

            package.loaded["lib.commands"] = {
              dispatch = function(_, _, command)
                assert(command.type == "select_agent")
                assert(command.session_uuid == "sess-1")
              end,
            }

            local result = action.dispatch({
              id = "botster.session.select",
              payload = { sessionUuid = "sess-1" },
            }, {})
            assert(result.handled == true)
            assert(result.ok == true)
            assert(result.via == "fallback")
            return "ok"
            "#,
        )
        .eval()
        .expect("fallback should report fallback via");

    assert_eq!(result, "ok");
}
