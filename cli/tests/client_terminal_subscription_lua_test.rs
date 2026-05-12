#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    missing_docs,
    reason = "test-code brevity"
)]

use std::path::PathBuf;

use mlua::Lua;

fn lua_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua")
}

fn new_client_test_lua() -> Lua {
    let lua = Lua::new();
    let dir = lua_src_dir();
    let code = format!(
        r#"
        package.path = "{dir}/?.lua;{dir}/?/init.lua;" .. package.path

        log = {{}}
        for _, level in ipairs({{"debug", "info", "warn", "error"}}) do
            log[level] = function(_) end
        end

        local store = {{}}
        package.loaded["hub.state"] = {{
            get = function(key, default)
                if store[key] == nil then store[key] = default end
                return store[key]
            end,
            set = function(key, value) store[key] = value end,
            class = function(_name)
                local cls = {{}}
                cls.__index = cls
                return cls
            end,
        }}

        package.loaded["lib.agent"] = {{}}

        hooks = {{
            call = function(_event, payload) return payload end,
            notify = function(_event, _payload) end,
        }}

        resize_calls = {{}}
        hub = {{
            resize_pty = function(session_uuid, rows, cols)
                table.insert(resize_calls, {{ session_uuid = session_uuid, rows = rows, cols = cols }})
            end,
        }}
        "#,
        dir = dir.display()
    );
    lua.load(&code)
        .exec()
        .expect("install Lua client test stubs");
    lua
}

#[test]
fn duplicate_terminal_subscribe_with_stale_lua_handle_does_not_recreate_data_plane() {
    let lua = new_client_test_lua();

    let result: (i64, i64, i64) = lua
        .load(
            r#"
            local Client = require("lib.client")
            local subscribe_count = 0
            local stop_count = 0
            local sent_count = 0

            local transport = {
                send = function(_msg)
                    sent_count = sent_count + 1
                end,
                subscribe_terminal = function(_opts)
                    subscribe_count = subscribe_count + 1
                    local handle = { active = true }
                    function handle:is_active()
                        return self.active
                    end
                    function handle:stop()
                        stop_count = stop_count + 1
                        self.active = false
                    end
                    return handle
                end,
            }

            local client = Client.new("browser-stale-handle", transport)
            client:handle_subscribe({
                type = "subscribe",
                channel = "terminal",
                subscriptionId = "terminal_sess",
                params = { session_uuid = "sess-stale-handle", rows = 41, cols = 132 },
            })

            client.terminal_subscriptions["terminal_sess"].active = false

            client:handle_subscribe({
                type = "subscribe",
                channel = "terminal",
                subscriptionId = "terminal_sess",
                params = { session_uuid = "sess-stale-handle", rows = 41, cols = 132 },
            })

            return subscribe_count, stop_count, sent_count
            "#,
        )
        .eval()
        .expect("client duplicate subscribe regression");

    assert_eq!(
        result.0, 1,
        "duplicate subscribe must not recreate terminal data plane"
    );
    assert_eq!(
        result.1, 0,
        "duplicate subscribe must not stop the stale handle"
    );
    assert_eq!(
        result.2, 2,
        "both subscribe messages should be acknowledged"
    );
}
