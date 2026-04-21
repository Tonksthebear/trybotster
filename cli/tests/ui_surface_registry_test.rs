//! Phase 4a — `lib.surfaces` registry tests + multi-surface broadcast
//! integration tests.
//!
//! Covers:
//!   1. `surfaces.register/unregister/list/get/path` Lua API.
//!   2. `web_layout.render(surface_name, state)` falling back to
//!      `surfaces.render_node` when the layout table has no entry.
//!   3. `lib.layout_broadcast.build_frames` iterating the registry and
//!      producing one frame per registered surface, deduped per
//!      `(subscription_key, surface_name)`.
//!   4. `surfaces.build_route_registry_payload` shape assertions.
//!
//! Run with: `./test.sh -- ui_surface_registry` from `cli/`.
//
// Rust guideline compliant 2026-04-20

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    clippy::needless_raw_string_hashes,
    clippy::match_single_binding,
    clippy::ignored_unit_patterns,
    clippy::redundant_closure_for_method_calls,
    reason = "test-code brevity"
)]

use std::path::PathBuf;
use std::sync::Mutex;

use botster::lua::primitives::web_layout;
use botster::ui_contract::lua::register as register_ui_contract;
use mlua::{Lua, Table, Value};

/// Global lock to serialise tests that mutate process env (the `web_layout`
/// primitive reads `BOTSTER_CONFIG_DIR` / `BOTSTER_WEB_LAYOUT_REPO_DIR`).
static RENDER_LOCK: Mutex<()> = Mutex::new(());

fn cli_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lua_src_dir() -> PathBuf {
    cli_manifest_dir().join("lua")
}

fn install_lua_stubs(lua: &Lua) {
    let globals = lua.globals();

    let log_tbl = lua.create_table().unwrap();
    for name in ["debug", "info", "warn", "error"] {
        let f = lua.create_function(|_, _: Value| Ok(())).unwrap();
        log_tbl.set(name, f).unwrap();
    }
    globals.set("log", log_tbl).unwrap();

    let hub_tbl = lua.create_table().unwrap();
    let server_id = lua.create_function(|_, _: ()| Ok("hub-test")).unwrap();
    hub_tbl.set("server_id", server_id).unwrap();
    globals.set("hub", hub_tbl).unwrap();

    // Simple in-memory hooks stub: `hooks.notify` is a no-op so surfaces.lua
    // doesn't need a live hub event bus for these tests; the bus itself is
    // covered by the layout_transport tests. We keep a counter visible
    // through a global so tests can assert `surfaces_changed` fired.
    let hooks_tbl: Table = lua
        .load(
            r#"
            local h = { notify_count = {}, fired = {} }
            function h.call(_event, payload) return payload end
            function h.notify(event, payload)
                h.notify_count[event] = (h.notify_count[event] or 0) + 1
                h.fired[#h.fired + 1] = { event = event, payload = payload }
            end
            function h.on(_event, _name, _fn) end
            function h.off(_event, _name) end
            return h
            "#,
        )
        .eval()
        .unwrap();
    globals.set("hooks", hooks_tbl).unwrap();

    let state_tbl: Table = lua
        .load(
            r#"
            local M = {}
            local store = {}
            function M.get(key, default)
                if store[key] == nil then store[key] = default end
                return store[key]
            end
            function M.set(key, value) store[key] = value end
            function M.class(_name) return {} end
            function M.clear(key) store[key] = nil end
            return M
            "#,
        )
        .eval()
        .unwrap();
    globals.set("state", state_tbl).unwrap();
}

fn new_test_lua() -> Lua {
    let lua = Lua::new();
    register_ui_contract(&lua).expect("register ui_contract");
    web_layout::register(&lua).expect("register web_layout");
    botster::lua::primitives::json::register(&lua).expect("register json");

    install_lua_stubs(&lua);

    let dir = lua_src_dir();
    let code = format!(
        "package.path = \"{dir}/?.lua;{dir}/?/init.lua;\" .. package.path",
        dir = dir.display()
    );
    lua.load(&code).exec().expect("update package.path");

    web_layout::_clear_override_cache_for_tests();

    // Load the registry module and expose it as _G.surfaces so the Rust
    // fallback in web_layout.rs finds it. Matches the production wiring in
    // hub/init.lua where `_G.surfaces = safe_require("lib.surfaces")`.
    lua.load(
        r#"
        _G.surfaces = require("lib.surfaces")
        _G.surfaces._reset_for_tests()
        "#,
    )
    .exec()
    .expect("install _G.surfaces");

    lua
}

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    let guard = RENDER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: Rust 2024 requires `unsafe` for env mutation; serialised by
    // RENDER_LOCK so parallel tests cannot observe a half-set state.
    unsafe {
        std::env::set_var(
            "BOTSTER_WEB_LAYOUT_REPO_DIR",
            "/tmp/botster-nonexistent-ui-surface-registry",
        );
        std::env::set_var(
            "BOTSTER_CONFIG_DIR",
            "/tmp/botster-nonexistent-ui-surface-registry",
        );
        std::env::remove_var("BOTSTER_DEV");
    }
    guard
}

// -------------------------------------------------------------------------
// surfaces.lua API
// -------------------------------------------------------------------------

#[test]
fn surfaces_register_stores_entry_and_list_returns_it() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (name, path, label, icon, list_len): (String, String, String, String, i64) = lua
        .load(
            r#"
            surfaces.register("hello", {
                path = "/plugins/hello",
                label = "Hello",
                icon = "sparkle",
                render = function(_state) return { type = "panel", props = { title = "hi" } } end,
            })
            local all = surfaces.list()
            local entry = all[1]
            return entry.name, entry.path, entry.label, entry.icon, #all
            "#,
        )
        .eval()
        .expect("register + list");

    assert_eq!(name, "hello");
    assert_eq!(path, "/plugins/hello");
    assert_eq!(label, "Hello");
    assert_eq!(icon, "sparkle");
    assert_eq!(list_len, 1);
}

#[test]
fn surfaces_list_is_deterministically_ordered() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let ordered: Vec<String> = lua
        .load(
            r#"
            -- mix ordered and unordered entries, out of alphabetical order
            surfaces.register("zeta", {
                render = function() return {type="panel", props={}} end,
                order = 10,
            })
            surfaces.register("alpha", {
                render = function() return {type="panel", props={}} end,
                order = 10,  -- same order as zeta, should fall back to seq
            })
            surfaces.register("middle", {
                render = function() return {type="panel", props={}} end,
                order = 5,
            })
            surfaces.register("last", {
                render = function() return {type="panel", props={}} end,
                -- no order = math.huge
            })
            local out = {}
            for _, e in ipairs(surfaces.list()) do
                out[#out + 1] = e.name
            end
            return out
            "#,
        )
        .eval()
        .expect("list order");

    // Expected: order=5 (middle) < order=10 (zeta seq=1, alpha seq=2) < no-order (last).
    assert_eq!(ordered, vec!["middle", "zeta", "alpha", "last"]);
}

#[test]
fn surfaces_unregister_removes_entry() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (before, after, get_after): (i64, i64, Value) = lua
        .load(
            r#"
            surfaces.register("tmp", {
                render = function() return {type="panel", props={}} end,
            })
            local b = #surfaces.list()
            local removed = surfaces.unregister("tmp")
            assert(removed == true, "unregister should return true for existing")
            local a = #surfaces.list()
            return b, a, surfaces.get("tmp")
            "#,
        )
        .eval()
        .expect("unregister");

    assert_eq!(before, 1);
    assert_eq!(after, 0);
    assert!(matches!(get_after, Value::Nil));
}

#[test]
fn surfaces_path_resolves_registered_path() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let path: String = lua
        .load(
            r#"
            surfaces.register("routed", {
                path = "/foo/bar",
                render = function() return {type="panel", props={}} end,
            })
            return surfaces.path("routed")
            "#,
        )
        .eval()
        .expect("path");

    assert_eq!(path, "/foo/bar");
}

#[test]
fn surfaces_changed_hook_fires_on_register_and_unregister() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (after_register, after_unregister): (i64, i64) = lua
        .load(
            r#"
            surfaces.register("a", {
                render = function() return {type="panel", props={}} end,
            })
            local after_register = hooks.notify_count["surfaces_changed"] or 0
            surfaces.unregister("a")
            local after_unregister = hooks.notify_count["surfaces_changed"] or 0
            return after_register, after_unregister
            "#,
        )
        .eval()
        .expect("hooks.notify fires");

    assert_eq!(after_register, 1, "register should fire surfaces_changed once");
    assert_eq!(
        after_unregister, 2,
        "unregister should fire surfaces_changed once more"
    );
}

// -------------------------------------------------------------------------
// web_layout.render falls back to surfaces.render_node
// -------------------------------------------------------------------------

#[test]
fn web_layout_render_falls_back_to_surfaces_registry() {
    let _lock = lock_env();
    let lua = new_test_lua();

    // The embedded `web.layout` has no "custom" entry, and we're using the
    // empty override directory — so the only path to a tree is the Phase
    // 4a fallback into _G.surfaces.render_node.
    let json: String = lua
        .load(
            r#"
            surfaces.register("custom_demo", {
                render = function(state)
                    return {
                        type = "panel",
                        props = { title = "demo", customValue = state.echo or "missing" },
                    }
                end,
            })
            return web_layout.render("custom_demo", { echo = "from-state" })
            "#,
        )
        .eval()
        .expect("render custom surface");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse JSON");
    let ty = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(ty, "panel", "fallback should render the registered tree");
    let title = parsed
        .pointer("/props/title")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(title, "demo");
    let custom = parsed
        .pointer("/props/customValue")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        custom, "from-state",
        "state should flow through the registered render fn"
    );
}

#[test]
fn web_layout_render_missing_surface_returns_error_fallback() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let json: String = lua
        .load(r#"return web_layout.render("nope_not_registered", {})"#)
        .eval()
        .expect("render missing surface");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse JSON");
    let title = parsed
        .pointer("/props/title")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        title.contains("Layout error"),
        "missing surface must yield the error-fallback tree; got title={title}"
    );
}

// -------------------------------------------------------------------------
// Multi-surface broadcast: layout_broadcast iterates the registry
// -------------------------------------------------------------------------

#[test]
fn layout_broadcast_emits_one_frame_per_registered_surface() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let names: Vec<String> = lua
        .load(
            r#"
            -- Only register lightweight non-workspace surfaces so we don't
            -- need the full AgentWorkspaceSurfaceInputV1 fixture here. Each
            -- supplies its own input_builder that returns a trivial state.
            surfaces.register("alpha", {
                path = "/alpha",
                render = function(s) return {type="panel", props={title="alpha", h=s.hub_id or "?"}} end,
                input_builder = function(_c, _s) return { hub_id = "hub-test" } end,
                order = 1,
            })
            surfaces.register("beta", {
                path = "/beta",
                render = function(s) return {type="panel", props={title="beta", h=s.hub_id or "?"}} end,
                input_builder = function(_c, _s) return { hub_id = "hub-test" } end,
                order = 2,
            })
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()
            local frames = LayoutBroadcast.build_frames(nil, {
                force = true,
                subscription_key = "sub-alpha",
            })
            local out = {}
            for _, f in ipairs(frames) do out[#out + 1] = f.target_surface end
            return out
            "#,
        )
        .eval()
        .expect("build frames");

    assert_eq!(names, vec!["alpha", "beta"]);
}

#[test]
fn layout_broadcast_dedups_per_subscription_per_surface() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (first_len, second_len): (i64, i64) = lua
        .load(
            r#"
            surfaces.register("alpha", {
                render = function(s) return {type="panel", props={title="alpha", hub=s.hub_id or "?"}} end,
                input_builder = function(_c, _s) return { hub_id = "hub-test" } end,
            })
            surfaces.register("beta", {
                render = function(s) return {type="panel", props={title="beta", hub=s.hub_id or "?"}} end,
                input_builder = function(_c, _s) return { hub_id = "hub-test" } end,
            })
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()

            local f1 = LayoutBroadcast.build_frames(nil, { subscription_key = "s1" })
            LayoutBroadcast.mark_sent(f1, { subscription_key = "s1" })
            local f2 = LayoutBroadcast.build_frames(nil, { subscription_key = "s1" })
            return #f1, #f2
            "#,
        )
        .eval()
        .expect("dedup");

    assert_eq!(first_len, 2, "first emission must emit both surfaces");
    assert_eq!(
        second_len, 0,
        "identical second emission must dedup to zero"
    );
}

#[test]
fn unregister_purges_per_subscription_dedup_baselines() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (before_removed, after_removed, stale_present_after_reregister): (i64, i64, bool) = lua
        .load(
            r#"
            surfaces.register("alpha", {
                render = function() return {type="panel", props={title="alpha-v1"}} end,
                input_builder = function() return { hub_id = "hub-test" } end,
            })
            surfaces.register("beta", {
                render = function() return {type="panel", props={title="beta-v1"}} end,
                input_builder = function() return { hub_id = "hub-test" } end,
            })
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()

            -- Seed dedup baselines for TWO subscriptions so we exercise the
            -- "across every subscription" contract on forget_surface.
            local f_a = LayoutBroadcast.build_frames(nil, { subscription_key = "sub-A" })
            LayoutBroadcast.mark_sent(f_a, { subscription_key = "sub-A" })
            local f_b = LayoutBroadcast.build_frames(nil, { subscription_key = "sub-B" })
            LayoutBroadcast.mark_sent(f_b, { subscription_key = "sub-B" })

            -- Sanity: both subs now have a baseline for "alpha".
            assert(LayoutBroadcast.last_version("alpha", { subscription_key = "sub-A" }) ~= nil)
            assert(LayoutBroadcast.last_version("alpha", { subscription_key = "sub-B" }) ~= nil)

            -- Unregister "alpha". surfaces.unregister should delegate to
            -- layout_broadcast.forget_surface, purging both subs' entries.
            surfaces.unregister("alpha")

            local alpha_a_gone = LayoutBroadcast.last_version("alpha", { subscription_key = "sub-A" }) == nil
            local alpha_b_gone = LayoutBroadcast.last_version("alpha", { subscription_key = "sub-B" }) == nil
            -- Beta should be untouched (unregister only purges the named surface).
            local beta_still_cached = LayoutBroadcast.last_version("beta", { subscription_key = "sub-A" }) ~= nil
            assert(beta_still_cached, "beta's baseline must survive alpha's unregister")

            local before_removed = (alpha_a_gone and alpha_b_gone) and 2 or 0

            -- Re-register "alpha" with a deliberately identical tree to
            -- reproduce the "stale hash collision" footgun: if forget_surface
            -- didn't run, this render's hash would match the stale baseline
            -- and dedup would swallow the frame.
            surfaces.register("alpha", {
                render = function() return {type="panel", props={title="alpha-v1"}} end,
                input_builder = function() return { hub_id = "hub-test" } end,
            })
            local f2 = LayoutBroadcast.build_frames(nil, { subscription_key = "sub-A" })

            local saw_alpha = false
            for _, frame in ipairs(f2) do
                if frame.target_surface == "alpha" then saw_alpha = true end
            end
            -- Key assertion: re-register + identical tree must re-emit
            -- because the forget_surface call zeroed the baseline.
            return before_removed, 0, not saw_alpha
            "#,
        )
        .eval()
        .expect("unregister purges dedup");

    assert_eq!(
        before_removed, 2,
        "alpha's dedup baseline must be cleared from both subscription buckets"
    );
    assert_eq!(after_removed, 0, "trivially zero (keeps return shape 3-tuple)");
    assert!(
        !stale_present_after_reregister,
        "re-registering alpha with an identical tree must re-emit; stale baseline was not purged"
    );
}

#[test]
fn layout_broadcast_reemits_when_one_surface_changes() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (first_len, second_surfaces): (i64, Vec<String>) = lua
        .load(
            r#"
            local tick = 0
            surfaces.register("stable", {
                render = function() return {type="panel", props={title="stable"}} end,
                input_builder = function(_c, _s) return { hub_id = "hub-test" } end,
            })
            surfaces.register("changing", {
                render = function() return {type="panel", props={title="changing-" .. tostring(tick)}} end,
                input_builder = function(_c, _s) return { hub_id = "hub-test" } end,
            })
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()

            local f1 = LayoutBroadcast.build_frames(nil, { subscription_key = "s1" })
            LayoutBroadcast.mark_sent(f1, { subscription_key = "s1" })
            tick = 1  -- changing render now produces a different tree
            local f2 = LayoutBroadcast.build_frames(nil, { subscription_key = "s1" })

            local names = {}
            for _, f in ipairs(f2) do names[#names + 1] = f.target_surface end
            return #f1, names
            "#,
        )
        .eval()
        .expect("partial re-emit");

    assert_eq!(first_len, 2, "first emission must emit both surfaces");
    assert_eq!(
        second_surfaces,
        vec!["changing"],
        "only the changed surface should re-emit; stable must dedup"
    );
}

// -------------------------------------------------------------------------
// Route registry payload
// -------------------------------------------------------------------------

#[test]
fn route_registry_payload_includes_routable_surfaces_and_excludes_pathless() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let json: String = lua
        .load(
            r#"
            surfaces.register("workspace_sidebar", {
                render = function() return {type="panel", props={}} end,
                -- no path: internal/non-routed surface
            })
            surfaces.register("workspace_panel", {
                path = "/",
                label = "Hub",
                icon = "home",
                render = function() return {type="panel", props={}} end,
                order = 0,
            })
            surfaces.register("hello", {
                path = "/plugins/hello",
                label = "Hello",
                icon = "sparkle",
                render = function() return {type="panel", props={}} end,
                order = 1000,
            })
            local payload = surfaces.build_route_registry_payload("hub-test")
            return json.encode(payload)
            "#,
        )
        .eval()
        .expect("build route registry payload");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse JSON");
    assert_eq!(parsed.get("type").and_then(|v| v.as_str()), Some("ui_route_registry_v1"));
    assert_eq!(parsed.get("hub_id").and_then(|v| v.as_str()), Some("hub-test"));
    let routes = parsed.get("routes").and_then(|v| v.as_array()).expect("routes array");
    let paths: Vec<&str> = routes
        .iter()
        .filter_map(|r| r.get("path").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(paths, vec!["/", "/plugins/hello"]);

    // workspace_sidebar had no path, so it must NOT appear.
    let surfaces: Vec<&str> = routes
        .iter()
        .filter_map(|r| r.get("surface").and_then(|v| v.as_str()))
        .collect();
    assert!(!surfaces.contains(&"workspace_sidebar"));
}

// -------------------------------------------------------------------------
// F1: demo plugin env gate
// -------------------------------------------------------------------------
//
// The `plugins/hello_surface/plugin.lua` file is loaded by `hub/init.lua`
// only when BOTSTER_DEV=1 OR BOTSTER_ENV=test. These tests exercise both
// legs of the gate by evaluating the same Lua predicate + safe_require
// block that init.lua uses, then asserting on the resulting registry.

/// Run `body` with `BOTSTER_DEV` and `BOTSTER_ENV` set to the given
/// values (or removed when `None`). Restores the previous values on drop.
fn with_demo_env<F: FnOnce()>(dev: Option<&str>, env: Option<&str>, body: F) {
    let prev_dev = std::env::var("BOTSTER_DEV").ok();
    let prev_env = std::env::var("BOTSTER_ENV").ok();
    // SAFETY: env-var mutation is serialised by RENDER_LOCK (held by the
    // caller via lock_env()) so no other thread can observe a half-set
    // state. Single-thread visibility is sufficient for Rust 2024's
    // set_var/remove_var invariant.
    unsafe {
        match dev {
            Some(v) => std::env::set_var("BOTSTER_DEV", v),
            None => std::env::remove_var("BOTSTER_DEV"),
        }
        match env {
            Some(v) => std::env::set_var("BOTSTER_ENV", v),
            None => std::env::remove_var("BOTSTER_ENV"),
        }
    }
    body();
    // SAFETY: same as the block above — still inside the RENDER_LOCK
    // held by the caller, so the restore is single-threaded.
    unsafe {
        match prev_dev {
            Some(v) => std::env::set_var("BOTSTER_DEV", v),
            None => std::env::remove_var("BOTSTER_DEV"),
        }
        match prev_env {
            Some(v) => std::env::set_var("BOTSTER_ENV", v),
            None => std::env::remove_var("BOTSTER_ENV"),
        }
    }
}

/// Emulate `hub/init.lua`'s demo-plugin gate + safe_require. Returns true
/// iff the surface ended up registered.
const DEMO_GATE_LUA: &str = r#"
    local demo_env = os.getenv("BOTSTER_DEV") == "1"
        or os.getenv("BOTSTER_ENV") == "test"
    if demo_env then
        require("plugins.hello_surface.plugin")
    end
    return surfaces.get("hello") ~= nil
"#;

#[test]
fn demo_plugin_registered_when_botster_dev_set() {
    let _lock = lock_env();
    let lua = new_test_lua();

    with_demo_env(Some("1"), None, || {
        let registered: bool = lua
            .load(DEMO_GATE_LUA)
            .eval()
            .expect("demo gate with BOTSTER_DEV=1");
        assert!(
            registered,
            "BOTSTER_DEV=1 must load the demo and register `hello`"
        );
    });
}

#[test]
fn demo_plugin_registered_when_botster_env_test() {
    let _lock = lock_env();
    let lua = new_test_lua();

    with_demo_env(None, Some("test"), || {
        let registered: bool = lua
            .load(DEMO_GATE_LUA)
            .eval()
            .expect("demo gate with BOTSTER_ENV=test");
        assert!(
            registered,
            "BOTSTER_ENV=test must load the demo and register `hello`"
        );
    });
}

#[test]
fn demo_plugin_skipped_when_neither_env_var_set() {
    let _lock = lock_env();
    let lua = new_test_lua();

    with_demo_env(None, None, || {
        let registered: bool = lua
            .load(DEMO_GATE_LUA)
            .eval()
            .expect("demo gate with no env vars");
        assert!(
            !registered,
            "without BOTSTER_DEV / BOTSTER_ENV=test the demo must NOT register"
        );
    });
}

#[test]
fn demo_plugin_skipped_when_production_like_env_vars_set() {
    let _lock = lock_env();
    let lua = new_test_lua();

    // Hostile production-like values — neither matches the gate.
    with_demo_env(Some("0"), Some("production"), || {
        let registered: bool = lua
            .load(DEMO_GATE_LUA)
            .eval()
            .expect("demo gate with production-like env");
        assert!(
            !registered,
            "BOTSTER_DEV=0, BOTSTER_ENV=production must NOT register the demo"
        );
    });
}

#[test]
fn route_registry_includes_hide_from_nav_entries_with_flag() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let payload_json: String = lua
        .load(
            r#"
            surfaces.register("visible", {
                path = "/visible",
                render = function() return {type="panel", props={}} end,
            })
            surfaces.register("hidden", {
                path = "/hidden",
                hide_from_nav = true,
                render = function() return {type="panel", props={}} end,
            })
            return json.encode(surfaces.build_route_registry_payload("hub-test"))
            "#,
        )
        .eval()
        .expect("route registry with hide_from_nav");

    let parsed: serde_json::Value =
        serde_json::from_str(&payload_json).expect("parse registry JSON");
    let routes = parsed.get("routes").and_then(|v| v.as_array()).unwrap();
    // Both present; filter happens at sidebar render time, not here — the
    // registry is the authoritative list of routable paths.
    assert_eq!(routes.len(), 2);
    let pairs: Vec<(String, bool)> = routes
        .iter()
        .map(|r| {
            let name = r.get("surface").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let hidden = r
                .get("hide_from_nav")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            (name, hidden)
        })
        .collect();
    assert!(pairs.contains(&("visible".to_string(), false)));
    assert!(pairs.contains(&("hidden".to_string(), true)));
}
