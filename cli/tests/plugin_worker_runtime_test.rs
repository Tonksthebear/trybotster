//! Integration tests for isolated plugin worker execution.

#![expect(clippy::unwrap_used, reason = "test-code brevity")]

use std::fs;
use std::time::{Duration, Instant};

use botster::lua::LuaRuntime;
use tempfile::TempDir;

#[test]
fn plugin_owned_ui_action_runs_in_plugin_worker_vm() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        )
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        r#"
        local action = require("lib.action")

        action.on("demo.worker", "main", function()
            local key = rawget(_G, "_plugin_worker_key")
            if key then
                return action.result{ message = "worker:" .. key }
            end
            return action.result{ ok = false, error = "hub closure ran" }
        end, { timeout_ms = 2000 })

        return {}
        "#,
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-plugin", {{ source = "device" }})
            assert(ok, tostring(err))

            local result = require("lib.action").dispatch({{ id = "demo.worker" }}, {{}})
            assert(result.handled == true)
            assert(result.ok == true, result.error)
            assert(result.message == "worker:worker-plugin", result.message)
            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();
}

#[test]
fn plugin_owned_session_action_runs_in_plugin_worker_vm() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        )
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-session-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        r#"
        local actions = require("lib.session_actions")

        actions.register("demo.session.worker", {
            label = "Worker Session",
            timeout_ms = 250,
            run = function()
                local key = rawget(_G, "_plugin_worker_key")
                if key then
                    return "worker:" .. key
                end
                return nil, "hub closure ran"
            end,
        })

        return {}
        "#,
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-session-plugin", {{ source = "device" }})
            assert(ok, tostring(err))

            package.loaded["lib.session"] = {{
                get = function(session_uuid)
                    return {{
                        info = function()
                            return {{ session_uuid = session_uuid, created_at = 1 }}
                        end,
                    }}
                end,
                all_info = function()
                    return {{ {{ session_uuid = "sess-worker", created_at = 1 }} }}
                end,
            }}

            local ok_run, result = require("lib.session_actions").run("sess-worker", "demo.session.worker", {{ params = {{}} }})
            assert(ok_run == true, tostring(result))
            assert(result == "worker:worker-session-plugin", tostring(result))
            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();
}

#[test]
fn plugin_owned_command_runs_in_plugin_worker_vm() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        )
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-command-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        r#"
        local commands = require("lib.commands")

        commands.register("demo_worker_command", function(_, _, command)
            local key = rawget(_G, "_plugin_worker_key")
            if key then
                return "worker:" .. key .. ":" .. tostring(command.value)
            end
            error("hub closure ran")
        end, { timeout_ms = 2000 })

        return {}
        "#,
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-command-plugin", {{ source = "device" }})
            assert(ok, tostring(err))

            local ok_dispatch, result = require("lib.commands").dispatch(nil, nil, {{
                type = "demo_worker_command",
                value = "ok",
            }})
            assert(ok_dispatch == true, tostring(result))
            assert(result == "worker:worker-command-plugin:ok", tostring(result))
            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();
}

#[test]
fn plugin_owned_hook_interceptor_runs_in_plugin_worker_vm() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        )
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-hook-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        r#"
        local hooks = require("hub.hooks")

        hooks.intercept("demo_worker_intercept", "main", function(payload)
            local key = rawget(_G, "_plugin_worker_key")
            if key then
                return { value = "worker:" .. key .. ":" .. tostring(payload.value) }
            end
            error("hub closure ran")
        end, { timeout_ms = 2000 })

        return {}
        "#,
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-hook-plugin", {{ source = "device" }})
            assert(ok, tostring(err))

            local hooks = require("hub.hooks")
            local listed = hooks.list("demo_worker_intercept")
            assert(#listed == 1, tostring(#listed))
            assert(listed[1].owner_plugin == "worker-hook-plugin", tostring(listed[1].owner_plugin))

            local result = hooks.call("demo_worker_intercept", {{ value = "ok" }})
            assert(type(result) == "table", tostring(result))
            assert(result.value == "worker:worker-hook-plugin:ok", tostring(result.value))
            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();
}

#[test]
fn plugin_owned_surface_route_runs_in_plugin_worker_vm() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        )
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-surface-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        r#"
        local surfaces = require("lib.surfaces")

        surfaces.register("worker_surface", {
            label = "Worker Surface",
            timeout_ms = 2000,
            routes = {
                {
                    path = "/item/:id",
                    render = function(state, ctx)
                        local key = rawget(_G, "_plugin_worker_key")
                        if key then
                            return {
                                type = "text",
                                props = {
                                    text = "worker:" .. key .. ":" .. tostring(state.params.id) .. ":" .. ctx.path("/item/:id", { id = state.params.id }),
                                },
                            }
                        end
                        error("hub closure ran")
                    end,
                },
            },
        })

        return {}
        "#,
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-surface-plugin", {{ source = "device" }})
            assert(ok, tostring(err))

            local surfaces = require("lib.surfaces")
            local entry = surfaces.get("worker_surface")
            assert(entry.owner_plugin == "worker-surface-plugin", tostring(entry.owner_plugin))

            local tree = surfaces.render_node("worker_surface", {{
                hub_id = "hub-test",
                path = "/item/42",
            }})
            assert(tree.type == "text", tostring(tree.type))
            assert(
                tree.props.text == "worker:worker-surface-plugin:42:/hubs/hub-test/worker_surface/item/42",
                tostring(tree.props.text)
            )
            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();
}

#[test]
fn hub_visible_plugin_surfaces_survive_plugin_worker_bootstrap() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        );
        std::env::set_var("BOTSTER_REPO", "botster/test");
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-bootstrap-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        r#"
        local surfaces = require("lib.surfaces")

        local repo = hub.detect_repo()
        if repo ~= "botster/test" then
            error("unexpected repo: " .. tostring(repo))
        end

        local db = plugin.db({
            version = 1,
            memory = true,
            models = {
                entries = {
                    id = true,
                    label = "text",
                },
            },
        })
        db.entries:insert({ label = "booted" })

        mcp.tool("worker_boot_probe", {
            description = "worker bootstrap probe",
            input_schema = { type = "object", properties = {} },
        }, function()
            return rawget(_G, "_plugin_worker_key") or "hub"
        end)

        surfaces.register("worker_boot_surface", {
            label = "Worker Boot Surface",
            routes = {
                {
                    path = "/",
                    render = function()
                        return {
                            type = "text",
                            props = {
                                text = rawget(_G, "_plugin_worker_key") or "hub",
                            },
                        }
                    end,
                },
            },
        })

        return {}
        "#,
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            _G.hub = {{
                hub_id = function() return "hub-test" end,
                server_id = function() return "hub-test" end,
                detect_repo = function() return "botster/test" end,
            }}
            _G.mcp = require("lib.mcp")
            local plugin_db = require("lib.plugin_db")
            plugin_db.install()

            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-bootstrap-plugin", {{ source = "device" }})
            assert(ok, tostring(err))

            local surfaces = require("lib.surfaces")
            local entry = surfaces.get("worker_boot_surface")
            assert(entry ~= nil, "surface was unregistered after worker load")
            assert(entry.owner_plugin == "worker-bootstrap-plugin", tostring(entry.owner_plugin))
            assert(entry.path ~= nil, "surface path missing")
            assert(entry.clients == nil or entry.clients.web == true, "surface not web-visible")

            local payload = surfaces.build_route_registry_payload("hub-test")
            local found = false
            for _, route in ipairs(payload.routes) do
                if route.surface == "worker_boot_surface" then found = true end
            end
            assert(found == true, "surface missing from hub route registry payload: " .. json.encode(payload))

            local tree = surfaces.render_node("worker_boot_surface", {{
                hub_id = "hub-test",
                path = "/",
            }})
            assert(tree.props.text == "worker-bootstrap-plugin", tostring(tree.props.text))

            local content, tool_err = require("lib.mcp").call_tool("worker_boot_probe", {{}}, {{}})
            assert(tool_err == nil, tostring(tool_err))
            assert(content[1].text == "worker-bootstrap-plugin", tostring(content[1] and content[1].text))
            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();
}

#[test]
fn plugin_owned_asset_message_runs_in_plugin_worker_vm() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        )
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-asset-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    let asset_path = plugin_dir.join("frame.html");
    fs::write(&asset_path, "<html></html>").unwrap();
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        format!(
            r#"
            local action = require("lib.action")
            local plugin_assets = require("lib.plugin_assets")

            plugin_assets.expose_file("frame", {asset_path}, {{ content_type = "text/html" }})
            plugin_assets.on_message("ping", function(payload, ctx)
                local key = rawget(_G, "_plugin_worker_key")
                if key then
                    return action.result{{
                        message = "worker:" .. key .. ":" .. tostring(payload.value) .. ":" .. tostring(ctx.asset_id) .. ":" .. tostring(ctx.target_surface),
                    }}
                end
                error("hub closure ran")
            end, {{ timeout_ms = 2000 }})

            return {{}}
            "#,
            asset_path = serde_json::to_string(&asset_path.to_string_lossy()).unwrap(),
        ),
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-asset-plugin", {{ source = "device" }})
            assert(ok, tostring(err))

            local plugin_assets = require("lib.plugin_assets")
            plugin_assets._install_action_handler()

            local result = require("lib.action").dispatch({{
                id = "botster.plugin_asset.message",
                payload = {{
                    assetId = "worker-asset-plugin:frame",
                    action = "ping",
                    payload = {{ value = "ok" }},
                }},
            }}, {{
                sub_id = "sub-asset",
                target_surface = "asset_surface",
            }})

            assert(result.handled == true, tostring(result.error))
            assert(result.ok == true, tostring(result.error))
            assert(
                result.message == "worker:worker-asset-plugin:ok:worker-asset-plugin:frame:asset_surface",
                tostring(result.message)
            )
            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();
}

#[test]
fn plugin_owned_timer_handler_runs_in_plugin_worker_vm() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        )
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-timer-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        r#"
        timer.after(60, function()
            local key = rawget(_G, "_plugin_worker_key")
            if key then
                return "worker:" .. key
            end
            error("hub closure ran")
        end)

        return {}
        "#,
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-timer-plugin", {{ source = "device" }})
            assert(ok, tostring(err))

            local result = __plugin_worker_invoke(
                "worker-timer-plugin",
                "timer",
                "worker-timer-plugin:timer_0",
                nil,
                {{}},
                2000
            )
            assert(result == "worker:worker-timer-plugin", tostring(result))
            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();
}

#[test]
fn plugin_owned_event_handler_runs_in_plugin_worker_vm() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        )
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-event-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        r#"
        events.on("demo_worker_event", function(payload)
            local key = rawget(_G, "_plugin_worker_key")
            if key then
                assert(payload.value == "ok", tostring(payload.value))
                return true
            end
            error("hub event closure ran")
        end)

        return {}
        "#,
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-event-plugin", {{ source = "device" }})
            assert(ok, tostring(err))

            local count = events.emit("demo_worker_event", {{ value = "ok" }})
            assert(count == 1, tostring(count))
            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();
}

#[test]
fn plugin_owned_watch_callback_runs_in_plugin_worker_vm() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        )
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-watch-plugin");
    let watched_dir = tmp.path().join("watched");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::create_dir_all(&watched_dir).unwrap();
    let output_path = tmp.path().join("watch-callback.txt");
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        format!(
            r#"
            local watched_dir = {watched_dir}
            local output_path = {output_path}

            watch.directory(watched_dir, {{ pattern = "*.txt" }}, function(event)
                local key = rawget(_G, "_plugin_worker_key")
                if key then
                    fs.write(output_path, "worker:" .. key .. ":" .. tostring(event.kind) .. ":" .. tostring(event.path))
                else
                    error("hub watch closure ran")
                end
            end)

            return {{}}
            "#,
            watched_dir = serde_json::to_string(&watched_dir.to_string_lossy()).unwrap(),
            output_path = serde_json::to_string(&output_path.to_string_lossy()).unwrap(),
        ),
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-watch-plugin", {{ source = "device" }})
            assert(ok, tostring(err))
            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();

    fs::write(watched_dir.join("changed.txt"), "ok").unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut content = String::new();
    while Instant::now() < deadline {
        if let Ok(value) = fs::read_to_string(&output_path) {
            content = value;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(
        content.starts_with("worker:worker-watch-plugin:"),
        "{content}"
    );
    assert!(content.contains("changed.txt"), "{content}");
}

#[test]
fn plugin_owned_http_callback_runs_in_plugin_worker_vm() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        )
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-http-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    let output_path = tmp.path().join("http-callback.txt");
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        format!(
            r#"
            local output_path = {output_path}

            events.on("demo_worker_http", function()
                local key = rawget(_G, "_plugin_worker_key")
                if not key then error("hub event closure ran") end

                http.request("NOPE", "http://127.0.0.1/", function(_resp, err)
                    local callback_key = rawget(_G, "_plugin_worker_key")
                    if callback_key then
                        fs.write(output_path, "worker:" .. callback_key .. ":" .. tostring(err))
                    else
                        fs.write(output_path, "hub callback ran")
                    end
                end)
            end)

            return {{}}
            "#,
            output_path = serde_json::to_string(&output_path.to_string_lossy()).unwrap(),
        ),
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-http-plugin", {{ source = "device" }})
            assert(ok, tostring(err))

            local count = events.emit("demo_worker_http", {{ value = "ok" }})
            assert(count == 1, tostring(count))
            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut content = String::new();
    while Instant::now() < deadline {
        if let Ok(value) = fs::read_to_string(&output_path) {
            content = value;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(
        content.starts_with("worker:worker-http-plugin:Unsupported HTTP method: NOPE"),
        "{content}"
    );
}

#[test]
fn plugin_owned_mcp_handlers_run_in_plugin_worker_vm() {
    // SAFETY: This integration filter runs this test in isolation and the
    // worker VM must resolve the repository Lua modules instead of user config.
    unsafe {
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        )
    };

    let tmp = TempDir::new().unwrap();
    let plugin_dir = tmp.path().join("worker-mcp-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        r#"
        local mcp = require("lib.mcp")

        mcp.tool("demo_worker_tool", {
            description = "Worker tool",
            input_schema = { type = "object", properties = {} },
            timeout_ms = 2000,
        }, function(params, ctx)
            local key = rawget(_G, "_plugin_worker_key")
            if key then
                return "tool:" .. key .. ":" .. tostring(params.value) .. ":" .. tostring(ctx.session_uuid)
            end
            error("hub tool closure ran")
        end)

        mcp.prompt("demo-worker-prompt", {
            description = "Worker prompt",
            arguments = {},
            timeout_ms = 2000,
        }, function(args)
            local key = rawget(_G, "_plugin_worker_key")
            if key then
                return "prompt:" .. key .. ":" .. tostring(args.value)
            end
            error("hub prompt closure ran")
        end)

        mcp.resource("botster://demo/{id}", {
            name = "Worker Resource",
            description = "Worker resource",
            mimeType = "text/plain",
            timeout_ms = 2000,
        }, function(params, ctx)
            local key = rawget(_G, "_plugin_worker_key")
            if key then
                return "resource:" .. key .. ":" .. tostring(params.id) .. ":" .. tostring(ctx.session_uuid)
            end
            error("hub resource closure ran")
        end)

        return {}
        "#,
    )
    .unwrap();

    let runtime = LuaRuntime::new().unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "worker-mcp-plugin", {{ source = "device" }})
            assert(ok, tostring(err))

            local mcp = require("lib.mcp")
            local content, tool_err = mcp.call_tool("demo_worker_tool", {{ value = "ok" }}, {{ session_uuid = "sess-mcp" }})
            assert(tool_err == nil, tostring(tool_err))
            assert(content[1].text == "tool:worker-mcp-plugin:ok:sess-mcp", tostring(content[1].text))

            local prompt, prompt_err = mcp.get_prompt("demo-worker-prompt", {{ value = "ok" }})
            assert(prompt_err == nil, tostring(prompt_err))
            assert(prompt.messages[1].content.text == "prompt:worker-mcp-plugin:ok", tostring(prompt.messages[1].content.text))

            local contents, resource_err = mcp.read_resource("botster://demo/42", {{ session_uuid = "sess-mcp" }})
            assert(resource_err == nil, tostring(resource_err))
            assert(contents[1].text == "resource:worker-mcp-plugin:42:sess-mcp", tostring(contents[1].text))

            return true
            "#,
            init_path = serde_json::to_string(&init_path.to_string_lossy()).unwrap(),
        ))
        .eval::<bool>()
        .unwrap();
}
