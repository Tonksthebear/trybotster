//! Lua tests for Session owner_plugin index maintenance.

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

#[test]
fn session_owner_index_follows_update_set_meta_and_close() {
    let lua = Lua::new();
    log::register(&lua).expect("register log");
    botster::lua::primitives::hook_timeout::register(&lua).expect("register hook timeout");
    let dir = lua_src_dir();
    let ok: bool = lua
        .load(format!(
            r#"
            package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path
            hub = {{
                revoke_mcp_caller = function() end,
                unregister_session = function() end,
                hub_id = function() return "hub-test" end,
                server_id = function() return "hub-test" end,
            }}
            package.loaded["lib.entity_model"] = {{
                patch_session = function() end,
                publish_session = function() end,
            }}
            package.loaded["lib.session_actions"] = {{
                publish_for_session = function() end,
            }}
            local Session = require("lib.session")
            local session = setmetatable({{
                session_uuid = "sess-owner-index",
                owner_plugin = "plugin-a",
                metadata = {{ owner_plugin = "plugin-a" }},
                status = "running",
                created_at = 1,
                _sync_session_manifest = function() end,
            }}, Session)
            Session._index_owner_for_tests(session)
            assert(#Session.list_owned("plugin-a") == 1)
            assert(#Session.list_owned("plugin-b") == 0)

            session:update({{
                owner_plugin = "plugin-b",
                metadata = {{ owner_plugin = "plugin-b" }},
            }})
            assert(#Session.list_owned("plugin-a") == 0, "old owner must drop")
            assert(#Session.list_owned("plugin-b") == 1, "new owner must index")

            session:set_meta("owner_plugin", "plugin-a")
            assert(#Session.list_owned("plugin-b") == 0)
            assert(#Session.list_owned("plugin-a") == 1)

            session:close(false)
            assert(#Session.list_owned("plugin-a") == 0)
            assert(#Session.list_owned("plugin-b") == 0)
            return true
            "#,
            dir = dir.display()
        ))
        .eval()
        .expect("owner index lifecycle");
    assert!(ok);
}
