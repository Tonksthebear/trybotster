//! Lua tests for demand-driven plugin surface snapshots.

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
    botster::lua::primitives::json::register(&lua).expect("register json");
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

fn nest_lua() -> &'static str {
    r#"
        local function nest(depth, text)
            if depth == 0 then
                return { type = "text", props = { text = text } }
            end
            return { type = "box", children = { nest(depth - 1, text) } }
        end
    "#
}

#[test]
fn deep_ui_tree_changes_are_detected() {
    let lua = new_lua();
    let ok: bool = lua
        .load(format!(
            r#"
            {nest}
            local surfaces = require("lib.surfaces")
            surfaces._reset_for_tests()
            local a = nest(6, "one")
            local b = nest(6, "two")
            assert(surfaces._trees_equal_for_tests(a, a) == true)
            assert(surfaces._trees_equal_for_tests(a, b) == false)
            return true
            "#,
            nest = nest_lua()
        ))
        .eval()
        .expect("compare deep trees");
    assert!(ok);
}

#[test]
fn forget_subscription_drops_stamped_slots() {
    let lua = new_lua();
    let remaining: i64 = lua
        .load(
            r#"
            local surfaces = require("lib.surfaces")
            surfaces._reset_for_tests()
            surfaces.register("snap_surface", {
                owner_plugin = "snap-plugin",
                routes = {
                    {
                        path = "/",
                        render = function()
                            return { type = "text", props = { text = "ok" } }
                        end,
                    },
                },
            })
            _G.__plugin_worker_async_available = function() return false end
            surfaces.render_node("snap_surface", {
                path = "/",
                subscription_key = "sub-1",
                peer_id = "peer-1",
            })
            assert(surfaces._snapshot_count_for_tests("snap_surface") >= 1)
            local removed = surfaces.forget_subscription("sub-1")
            assert(removed >= 1, tostring(removed))
            return surfaces._snapshot_count_for_tests("snap_surface")
            "#,
        )
        .eval()
        .expect("forget subscription");
    assert_eq!(remaining, 0);
}

#[test]
fn registered_plugin_asset_reads_without_worker_fallback() {
    let lua = new_lua();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("graph.html");
    std::fs::write(&path, "<html>ok</html>").expect("write asset");
    let result: String = lua
        .load(format!(
            r#"
            fs = {{
                read = function(path)
                    local handle = assert(io.open(path, "rb"))
                    local content = handle:read("*a")
                    handle:close()
                    return content
                end,
            }}
            local plugin_assets = require("lib.plugin_assets")
            plugin_assets._register({{
                id = "test-plugin:graph",
                name = "graph",
                plugin_name = "test-plugin",
                path = {path},
                content_type = "text/html",
            }})
            local registered, err = plugin_assets.read("test-plugin:graph")
            assert(registered ~= nil, tostring(err))
            assert(registered.content == "<html>ok</html>")
            local missing, miss_err = plugin_assets.read("test-plugin:missing")
            assert(missing == nil)
            assert(miss_err == "Unknown plugin asset")
            return miss_err
            "#,
            path = serde_json::to_string(&path.to_string_lossy()).unwrap(),
        ))
        .eval()
        .expect("read registered and missing assets");
    assert_eq!(result, "Unknown plugin asset");
}
