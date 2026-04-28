//! Regression tests for plugin reload discovery.
//!
//! `reload_plugin` is an MCP/browser-facing affordance, so it needs to handle
//! the common edit/install flow where a plugin now exists on disk but the live
//! hub registry has not seen it yet.

#![expect(clippy::unwrap_used, reason = "test-code brevity")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mlua::{Lua, LuaOptions, StdLib, Table};
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn cli_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lua_string(path: &Path) -> String {
    serde_json::to_string(&path.to_string_lossy().to_string()).unwrap()
}

fn set_config_dir(dir: &Path) {
    // SAFETY: Tests serialise via ENV_LOCK so no other thread observes the
    // mutation concurrently.
    unsafe { std::env::set_var("BOTSTER_CONFIG_DIR", dir) };
}

fn new_loader_lua() -> Lua {
    let lua =
        unsafe { Lua::unsafe_new_with(StdLib::ALL_SAFE | StdLib::FFI, LuaOptions::default()) };

    botster::lua::primitives::fs::register(&lua).expect("register fs");
    botster::lua::primitives::json::register(&lua).expect("register json");
    botster::lua::primitives::log::register(&lua).expect("register log");
    botster::lua::primitives::config::register(&lua).expect("register config");

    let globals = lua.globals();
    lua.load(
        r#"
        _G.hooks = {
            _observers = {},
            on = function(event, name, fn)
                _G.hooks._observers[event] = _G.hooks._observers[event] or {}
                _G.hooks._observers[event][name] = fn
            end,
            notify = function(event, payload)
                local count = 0
                for _, fn in pairs(_G.hooks._observers[event] or {}) do
                    fn(payload)
                    count = count + 1
                end
                return count
            end,
        }
        package.loaded["hub.hooks"] = _G.hooks
        _G.mcp = {
            begin_batch = function() end,
            end_batch = function() end,
            reset = function() end,
        }
        "#,
    )
    .exec()
    .expect("install stubs");

    let lua_base = cli_manifest_dir().join("lua");
    let base = lua_base.to_string_lossy();
    let package: Table = globals.get("package").unwrap();
    let current_path: String = package.get("path").unwrap();
    let new_path =
        format!("{base}/?.lua;{base}/?/init.lua;{base}/lib/?.lua;{base}/hub/?.lua;{current_path}");
    package.set("path", new_path).unwrap();

    lua
}

#[test]
fn reload_plugin_discovers_repo_plugin_missing_from_registry() {
    let _lock = lock_env();
    let tmp = TempDir::new().unwrap();
    let device_root = tmp.path().join(".botster-dev");
    let repo_root = tmp.path().join("project");
    let plugin_dir = repo_root
        .join(".botster-dev")
        .join("plugins")
        .join("jupiter-worktree-lifecycle");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join("init.lua"),
        r#"
        _G.jupiter_reload_discovery_loaded = (_G.jupiter_reload_discovery_loaded or 0) + 1
        return { name = "jupiter-worktree-lifecycle" }
        "#,
    )
    .unwrap();

    fs::create_dir_all(&device_root).unwrap();
    set_config_dir(&device_root);

    let lua = new_loader_lua();
    lua.load(format!(
        r#"
        local state = require("hub.state")
        state.set("plugin_resolver_opts", {{
            device_root = {device_root},
            repo_roots = {{ {repo_root} }},
        }})

        local loader = require("hub.loader")
        local ok, err = loader.reload_plugin("jupiter-worktree-lifecycle")
        assert(ok, tostring(err))

        local registry = state.get("plugin_registry", {{}})
        local key = "repo:" .. {repo_root} .. ":jupiter-worktree-lifecycle"
        local entry = registry[key]
        assert(entry ~= nil, "expected registry entry")
        assert(entry.status == "loaded", "expected loaded, got " .. tostring(entry.status))
        assert(entry.name == "jupiter-worktree-lifecycle")
        assert(entry.repo_root == {repo_root})
        assert(entry.path:match("/%.botster%-dev/plugins/jupiter%-worktree%-lifecycle/init%.lua$"), entry.path)
        assert(_G.jupiter_reload_discovery_loaded == 1, "plugin should load once")
        "#,
        device_root = lua_string(&device_root),
        repo_root = lua_string(&repo_root),
    ))
    .exec()
    .expect("reload should discover and load repo plugin");
}

#[test]
fn repo_plugins_with_same_name_load_as_distinct_instances() {
    let _lock = lock_env();
    let tmp = TempDir::new().unwrap();
    let device_root = tmp.path().join(".botster-dev");
    let repo_one = tmp.path().join("repo-one");
    let repo_two = tmp.path().join("repo-two");
    let plugin_name = "rails-worktree-lifecycle";

    for (repo, marker) in [(&repo_one, "one"), (&repo_two, "two")] {
        let plugin_dir = repo.join(".botster-dev").join("plugins").join(plugin_name);
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::write(
            plugin_dir.join("init.lua"),
            format!(
                r#"
                _G.repo_plugin_loads = _G.repo_plugin_loads or {{}}
                table.insert(_G.repo_plugin_loads, "{marker}")
                local hooks = require("hub.hooks")
                hooks.on("worktree_created", "rails-worktree-lifecycle.worktree-created", function()
                    _G.repo_plugin_hook_calls = _G.repo_plugin_hook_calls or {{}}
                    table.insert(_G.repo_plugin_hook_calls, "{marker}")
                end)
                return {{ marker = "{marker}" }}
                "#
            ),
        )
        .unwrap();
    }

    fs::create_dir_all(&device_root).unwrap();
    set_config_dir(&device_root);

    let lua = new_loader_lua();
    lua.load(format!(
        r#"
        local state = require("hub.state")
        local loader = require("hub.loader")

        local ok_one, err_one = loader.load_plugin(
          {repo_one_plugin},
          "rails-worktree-lifecycle",
          {{ source = "repo", repo_root = {repo_one} }}
        )
        assert(ok_one, tostring(err_one))

        local ok_two, err_two = loader.load_plugin(
          {repo_two_plugin},
          "rails-worktree-lifecycle",
          {{ source = "repo", repo_root = {repo_two} }}
        )
        assert(ok_two, tostring(err_two))

        local key_one = "repo:" .. {repo_one} .. ":rails-worktree-lifecycle"
        local key_two = "repo:" .. {repo_two} .. ":rails-worktree-lifecycle"
        local registry = state.get("plugin_registry", {{}})
        assert(registry[key_one] ~= nil, "expected repo-one scoped registry entry")
        assert(registry[key_two] ~= nil, "expected repo-two scoped registry entry")
        assert(registry[key_one].name == "rails-worktree-lifecycle")
        assert(registry[key_two].name == "rails-worktree-lifecycle")
        assert(registry[key_one].repo_root == {repo_one})
        assert(registry[key_two].repo_root == {repo_two})
        assert(#_G.repo_plugin_loads == 2, "expected both plugin instances to load")
        assert(hooks.notify("worktree_created", {{}}) == 2, "expected both scoped hooks to fire")
        table.sort(_G.repo_plugin_hook_calls)
        assert(_G.repo_plugin_hook_calls[1] == "one")
        assert(_G.repo_plugin_hook_calls[2] == "two")
        "#,
        repo_one = lua_string(&repo_one),
        repo_two = lua_string(&repo_two),
        repo_one_plugin = lua_string(
            &repo_one
                .join(".botster-dev")
                .join("plugins")
                .join(plugin_name)
                .join("init.lua")
        ),
        repo_two_plugin = lua_string(
            &repo_two
                .join(".botster-dev")
                .join("plugins")
                .join(plugin_name)
                .join("init.lua")
        ),
    ))
    .exec()
    .expect("same-name repo plugins should be scoped by repo");
}
