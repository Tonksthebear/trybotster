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
fn surfaces_path_derives_base_from_name_and_returns_full_url() {
    let _lock = lock_env();
    let lua = new_test_lua();

    // Phase 4b: `surfaces.path(name)` returns the full hub-scoped URL
    // (`/hubs/<id>/<name>`) — NOT the relative base. Callers writing
    // `ui.action("botster.nav.open", { path = surfaces.path("kanban") })`
    // expect an absolute URL that React Router can `pushState` directly.
    // The hub id is sourced from `hub.server_id()` ("hub-test" in the
    // test harness).
    let path: String = lua
        .load(
            r#"
            surfaces.register("kanban", {
                routes = {
                    { path = "/", render = function() return {type="panel", props={}} end },
                },
            })
            return surfaces.path("kanban")
            "#,
        )
        .eval()
        .expect("path");

    assert_eq!(path, "/hubs/hub-test/kanban");
}

#[test]
fn surfaces_path_interpolates_named_params() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let url: String = lua
        .load(
            r#"
            surfaces.register("kanban", {
                routes = {
                    { path = "/board/:id", render = function() return {type="panel", props={}} end },
                },
            })
            return surfaces.path("kanban", "/board/:id", { id = 42 })
            "#,
        )
        .eval()
        .expect("interpolate");

    assert_eq!(url, "/hubs/hub-test/kanban/board/42");
}

#[test]
fn surfaces_path_honours_legacy_path_escape_hatch() {
    // Regression guard for built-in surfaces like `workspace_panel` that
    // register with `path = "/"` (or any other legacy base) instead of
    // using the name-derived convention. `surfaces.path` must thread the
    // explicit base through the URL builder so
    //   surfaces.path("legacy", "/sub") = "/hubs/:id/weird/thing/sub"
    let _lock = lock_env();
    let lua = new_test_lua();

    let (root, deep): (String, String) = lua
        .load(
            r#"
            surfaces.register("legacy", {
                path = "/weird/thing",
                render = function() return {type="panel", props={}} end,
            })
            local root = surfaces.path("legacy")
            local deep = surfaces.path("legacy", "/sub", nil)
            return root, deep
            "#,
        )
        .eval()
        .expect("legacy escape hatch");

    assert_eq!(root, "/hubs/hub-test/weird/thing");
    assert_eq!(deep, "/hubs/hub-test/weird/thing/sub");
}

#[test]
fn surfaces_path_returns_nil_for_unknown_surface() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let value: Value = lua
        .load(r#"return surfaces.path("never_registered")"#)
        .eval()
        .expect("unknown surface");

    assert!(matches!(value, Value::Nil));
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

// -------------------------------------------------------------------------
// Phase 4b — sub-routes, params, ctx, subpath re-render
// -------------------------------------------------------------------------

#[test]
fn multi_route_dispatcher_routes_subpaths_and_extracts_params() {
    let _lock = lock_env();
    let lua = new_test_lua();

    // Register a multi-route surface; directly exercise the generated
    // `entry.render` dispatcher with synthetic `state.path` values to prove
    // each sub-route fires and `state.params` carries the named captures.
    let (home_hit, details_id, settings_hit): (String, String, String) = lua
        .load(
            r#"
            surfaces.register("kanban", {
                routes = {
                    { path = "/",           render = function(s, _ctx) return { type = "panel", props = { tag = "home", path = s.path } } end },
                    { path = "/board/:id",  render = function(s, _ctx) return { type = "panel", props = { tag = "board", id = s.params.id } } end },
                    { path = "/settings",   render = function(s, _ctx) return { type = "panel", props = { tag = "settings", path = s.path } } end },
                },
            })
            local entry = surfaces.get("kanban")
            local home = entry.render({ hub_id = "hub-test", path = "/" })
            local board = entry.render({ hub_id = "hub-test", path = "/board/42" })
            local settings = entry.render({ hub_id = "hub-test", path = "/settings" })
            return home.props.tag, board.props.id, settings.props.tag
            "#,
        )
        .eval()
        .expect("multi-route dispatcher");

    assert_eq!(home_hit, "home");
    assert_eq!(details_id, "42");
    assert_eq!(settings_hit, "settings");
}

#[test]
fn multi_route_dispatcher_renders_sub_404_for_unknown_subpath() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (ty, title): (String, String) = lua
        .load(
            r#"
            surfaces.register("kanban", {
                routes = {
                    { path = "/", render = function() return { type = "panel", props = { tag = "home" } } end },
                },
            })
            local entry = surfaces.get("kanban")
            local tree = entry.render({ hub_id = "hub-test", path = "/does/not/exist" })
            -- Sub-404 tree is a panel whose first text child is "Sub-route not found".
            local text = tree.children[1].children[1].props.text
            return tree.type, text
            "#,
        )
        .eval()
        .expect("sub-404");

    assert_eq!(ty, "panel");
    assert!(
        title.starts_with("Sub-route not found"),
        "expected sub-404 tree, got: {title}"
    );
}

#[test]
fn ctx_path_builds_full_hub_scoped_url_from_subpath_template() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (root_url, board_url, missing_param): (String, String, String) = lua
        .load(
            r#"
            local captured_root, captured_board, captured_missing
            surfaces.register("kanban", {
                routes = {
                    { path = "/", render = function(_s, ctx)
                        captured_root = ctx.path("/")
                        captured_board = ctx.path("/board/:id", { id = 99 })
                        -- Missing params leave the `:name` literal so tests
                        -- can catch the miss rather than silently rendering
                        -- a broken URL.
                        captured_missing = ctx.path("/board/:id", {})
                        return { type = "panel", props = {} }
                    end },
                },
            })
            local entry = surfaces.get("kanban")
            entry.render({ hub_id = "hub-test", path = "/" })
            return captured_root, captured_board, captured_missing
            "#,
        )
        .eval()
        .expect("ctx.path");

    assert_eq!(root_url, "/hubs/hub-test/kanban");
    assert_eq!(board_url, "/hubs/hub-test/kanban/board/99");
    assert_eq!(missing_param, "/hubs/hub-test/kanban/board/:id");
}

#[test]
fn ctx_surface_and_base_path_are_exposed_to_sub_route_render() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (surface_name, base_path, hub_id): (String, String, String) = lua
        .load(
            r#"
            local captured
            surfaces.register("kanban", {
                routes = {
                    { path = "/", render = function(_s, ctx)
                        captured = { surface = ctx.surface, base_path = ctx.base_path, hub_id = ctx.hub_id }
                        return { type = "panel", props = {} }
                    end },
                },
            })
            surfaces.get("kanban").render({ hub_id = "hub-test", path = "/" })
            return captured.surface, captured.base_path, captured.hub_id
            "#,
        )
        .eval()
        .expect("ctx introspection");

    assert_eq!(surface_name, "kanban");
    assert_eq!(base_path, "/kanban");
    assert_eq!(hub_id, "hub-test");
}

#[test]
fn backwards_compat_top_level_render_wraps_in_single_route() {
    let _lock = lock_env();
    let lua = new_test_lua();

    // Plugins that pass a single top-level `render` (Phase 4a style) are
    // internally wrapped into `routes = { { path = "/", render = fn } }`
    // so the dispatcher path is uniform. This test proves the wrapper:
    //   * entry.compiled_routes contains exactly one route
    //   * the render fn still runs for state.path == "/"
    //   * a non-root path falls through to the sub-404 renderer
    let (route_count, home_hit, unknown_tag): (i64, String, String) = lua
        .load(
            r#"
            surfaces.register("legacy", {
                render = function(s, _ctx) return { type = "panel", props = { tag = "legacy", path = s.path } } end,
            })
            local entry = surfaces.get("legacy")
            local home = entry.render({ hub_id = "hub-test", path = "/" })
            local miss = entry.render({ hub_id = "hub-test", path = "/elsewhere" })
            -- Sub-404 top-level is a panel wrapping a stack that contains
            -- a text whose text starts with "Sub-route not found".
            local miss_text = miss.children[1].children[1].props.text
            return #entry.compiled_routes, home.props.tag, miss_text
            "#,
        )
        .eval()
        .expect("back-compat wrapper");

    assert_eq!(route_count, 1);
    assert_eq!(home_hit, "legacy");
    assert!(
        unknown_tag.starts_with("Sub-route not found"),
        "expected sub-404 for non-root path in back-compat mode, got: {unknown_tag}"
    );
}

#[test]
fn register_rejects_both_routes_and_top_level_render() {
    let _lock = lock_env();
    let lua = new_test_lua();

    // Mixing API styles is a programming error — we assert loudly.
    let result: mlua::Result<Value> = lua
        .load(
            r#"
            surfaces.register("hybrid", {
                render = function() return { type = "panel", props = {} } end,
                routes = {
                    { path = "/", render = function() return { type = "panel", props = {} } end },
                },
            })
            return true
            "#,
        )
        .eval();

    match result {
        Ok(_) => panic!("expected assertion error for hybrid registration"),
        Err(err) => {
            let msg = err.to_string();
            assert!(
                msg.contains("EITHER") || msg.contains("routes") || msg.contains("render"),
                "assertion should mention the hybrid API conflict, got: {msg}"
            );
        }
    }
}

#[test]
fn path_segment_matching_escapes_pattern_metacharacters() {
    // A literal "/" in a segment is fine, but any Lua-pattern metachar (".")
    // must be matched literally; otherwise "/board.json" would accidentally
    // match "/boardxjson". This is a regression guard for the pattern
    // compiler in surfaces.lua.
    let _lock = lock_env();
    let lua = new_test_lua();

    let (literal_hit, wrong_type, wrong_tag, wrong_text): (bool, String, String, String) = lua
        .load(
            r#"
            surfaces.register("plain", {
                routes = {
                    { path = "/hello.world", render = function() return { type = "panel", props = { tag = "literal" } } end },
                },
            })
            local entry = surfaces.get("plain")
            local exact = entry.render({ hub_id = "hub-test", path = "/hello.world" })
            local wrong = entry.render({ hub_id = "hub-test", path = "/helloXworld" })
            local is_literal = exact.props and exact.props.tag == "literal"
            local text = ""
            if wrong.children and wrong.children[1] and wrong.children[1].children and wrong.children[1].children[1] then
                text = wrong.children[1].children[1].props.text or ""
            end
            return is_literal, wrong.type or "?", (wrong.props and wrong.props.tag) or "?", text
            "#,
        )
        .eval()
        .expect("pattern escape");

    assert!(literal_hit, "literal dot must match literal dot");
    assert_eq!(wrong_type, "panel", "wrong should be a panel (sub-404 wrapper)");
    assert_ne!(
        wrong_tag, "literal",
        "literal dot must NOT match arbitrary char (got the literal render instead of sub-404); wrong={wrong_type:?} wrong_tag={wrong_tag:?} text={wrong_text:?}"
    );
    assert!(
        wrong_text.contains("Sub-route not found"),
        "expected sub-404 text for arbitrary-char mismatch; got: {wrong_text:?}"
    );
}

#[test]
fn route_pattern_matches_trailing_slash_variants() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (no_slash, with_slash): (String, String) = lua
        .load(
            r#"
            surfaces.register("kanban", {
                routes = {
                    { path = "/board/:id", render = function(s) return { type = "panel", props = { id = s.params.id } } end },
                },
            })
            local entry = surfaces.get("kanban")
            local a = entry.render({ hub_id = "hub-test", path = "/board/7" }).props.id
            local b = entry.render({ hub_id = "hub-test", path = "/board/7/" }).props.id
            return a, b
            "#,
        )
        .eval()
        .expect("trailing slash");

    assert_eq!(no_slash, "7");
    assert_eq!(with_slash, "7");
}

#[test]
fn layout_broadcast_threads_client_subpath_into_render_state() {
    let _lock = lock_env();
    let lua = new_test_lua();

    // Simulate the client carrying a `surface_subpaths` map; `build_frames`
    // should resolve the subpath into `state.path` before the render, so
    // the dispatcher routes to the matching sub-route.
    let rendered_tag: String = lua
        .load(
            r#"
            surfaces.register("kanban", {
                routes = {
                    { path = "/",          render = function(_s, _ctx) return { type = "panel", props = { tag = "home" } } end },
                    { path = "/board/:id", render = function(s, _ctx) return { type = "panel", props = { tag = "board", id = s.params.id } } end },
                },
                input_builder = function(_c, _s) return { hub_id = "hub-test" } end,
            })
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()
            -- Stand-in client with a subpath map (production-side client.lua
            -- owns this; tests stub it so we don't need to boot a full client).
            local fake_client = { surface_subpaths = { kanban = "/board/9" } }
            local frames = LayoutBroadcast.build_frames(nil, {
                subscription_key = "sub-kanban",
                client = fake_client,
                force = true,
            })
            -- Find the frame for "kanban" — iteration order mirrors registration.
            local tree
            for _, f in ipairs(frames) do
                if f.target_surface == "kanban" then tree = f.tree end
            end
            assert(tree, "expected a kanban frame")
            return tree.props.tag
            "#,
        )
        .eval()
        .expect("layout_broadcast subpath threading");

    assert_eq!(rendered_tag, "board");
}

#[test]
fn layout_broadcast_targeted_only_surface_re_renders_one_surface() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let emitted: Vec<String> = lua
        .load(
            r#"
            surfaces.register("alpha", {
                render = function(s, _ctx) return { type = "panel", props = { path = s.path } } end,
                input_builder = function() return { hub_id = "hub-test" } end,
            })
            surfaces.register("beta", {
                render = function(s, _ctx) return { type = "panel", props = { path = s.path } } end,
                input_builder = function() return { hub_id = "hub-test" } end,
            })
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()
            local frames = LayoutBroadcast.build_frames(nil, {
                subscription_key = "sub",
                client = { surface_subpaths = {} },
                only_surface = "beta",
                force = true,
            })
            local names = {}
            for _, f in ipairs(frames) do names[#names + 1] = f.target_surface end
            return names
            "#,
        )
        .eval()
        .expect("only_surface filter");

    assert_eq!(emitted, vec!["beta"]);
}

#[test]
fn route_registry_payload_carries_base_path_and_sub_routes() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let payload_json: String = lua
        .load(
            r#"
            surfaces.register("kanban", {
                label = "Kanban",
                icon = "squares-2x2",
                routes = {
                    { path = "/",          render = function() return { type = "panel", props = {} } end },
                    { path = "/board/:id", render = function() return { type = "panel", props = {} } end },
                    { path = "/settings",  render = function() return { type = "panel", props = {} } end },
                },
            })
            return json.encode(surfaces.build_route_registry_payload("hub-test"))
            "#,
        )
        .eval()
        .expect("route registry payload");

    let parsed: serde_json::Value =
        serde_json::from_str(&payload_json).expect("parse payload");
    let routes = parsed.get("routes").and_then(|v| v.as_array()).unwrap();
    let entry = routes
        .iter()
        .find(|r| r.get("surface").and_then(|v| v.as_str()) == Some("kanban"))
        .expect("kanban entry");

    assert_eq!(entry.get("base_path").and_then(|v| v.as_str()), Some("/kanban"));
    // `path` mirrors `base_path` for routable surfaces so older browsers
    // still see a valid top-level URL.
    assert_eq!(entry.get("path").and_then(|v| v.as_str()), Some("/kanban"));

    let sub_routes = entry.get("routes").and_then(|v| v.as_array()).expect("routes[]");
    let sub_paths: Vec<&str> = sub_routes
        .iter()
        .filter_map(|r| r.get("path").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(sub_paths, vec!["/", "/board/:id", "/settings"]);
}

#[test]
fn demo_hello_plugin_registers_with_two_routes() {
    // Regression guard for `plugins/hello_surface/plugin.lua` — Phase 4b
    // migrated it to the multi-route API. This test is the canonical
    // "the demo still loads and the substrate is wired up" smoke test.
    let _lock = lock_env();
    let lua = new_test_lua();

    with_demo_env(None, Some("test"), || {
        let (name, base_path, route_count): (String, String, i64) = lua
            .load(
                r#"
                require("plugins.hello_surface.plugin")
                local entry = surfaces.get("hello")
                return entry.name, entry.base_path, #entry.compiled_routes
                "#,
            )
            .eval()
            .expect("hello demo plugin registered");

        assert_eq!(name, "hello");
        assert_eq!(base_path, "/hello");
        assert_eq!(route_count, 2);
    });
}

// -------------------------------------------------------------------------
// End-to-end: web_layout.render → registry fallback → plugin render
// -------------------------------------------------------------------------
//
// These tests exercise the exact path the hub takes when the browser sends
// `botster.surface.subpath` for a plugin-registered surface: the Rust
// `web_layout.render(surface, state)` consults the override layout table
// (nothing for plugin names), falls through to `_G.surfaces.render_node`,
// the dispatcher matches the subpath against declared routes, and the
// plugin's sub-route render fn returns a tree.
//
// Regression bar: a user clicking the Hello sidebar entry must receive a
// real Hello home tree, NOT the error-fallback panel (`Layout error:
// hello` / "The hub layout failed to render. Showing fallback.").

fn assert_not_error_fallback(json: &serde_json::Value, surface_name: &str) {
    let title = json
        .pointer("/props/title")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        !title.starts_with("Layout error"),
        "web_layout.render returned the error fallback for `{surface_name}` \
         (title={title:?}); the registry fallback did not resolve the \
         plugin-registered surface. Full tree: {json}"
    );
}

#[test]
fn hello_plugin_home_renders_through_web_layout() {
    let _lock = lock_env();
    let lua = new_test_lua();

    with_demo_env(None, Some("test"), || {
        let json: String = lua
            .load(
                r#"
                require("plugins.hello_surface.plugin")
                return web_layout.render("hello", { hub_id = "hub-test", path = "/" })
                "#,
            )
            .eval()
            .expect("render hello home");

        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_not_error_fallback(&parsed, "hello");

        let text_blob = parsed.to_string();
        assert!(
            text_blob.contains("Phase 4b sub-routes demo"),
            "hello home tree must contain the home_page text, got: {text_blob}"
        );
    });
}

#[test]
fn hello_plugin_details_renders_with_param_through_web_layout() {
    let _lock = lock_env();
    let lua = new_test_lua();

    with_demo_env(None, Some("test"), || {
        let json: String = lua
            .load(
                r#"
                require("plugins.hello_surface.plugin")
                return web_layout.render("hello", {
                    hub_id = "hub-test",
                    path = "/details/7",
                })
                "#,
            )
            .eval()
            .expect("render hello details");

        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_not_error_fallback(&parsed, "hello");

        let text_blob = parsed.to_string();
        assert!(
            text_blob.contains("Details for id=7"),
            "details render must interpolate :id=7 into state.params, got: {text_blob}"
        );
    });
}

#[test]
fn hello_plugin_default_path_falls_through_to_home() {
    // When the browser hasn't yet announced its subpath, the hub renders
    // with an unset state.path; the dispatcher normalises to "/" which
    // must match the home route.
    let _lock = lock_env();
    let lua = new_test_lua();

    with_demo_env(None, Some("test"), || {
        let json: String = lua
            .load(
                r#"
                require("plugins.hello_surface.plugin")
                return web_layout.render("hello", { hub_id = "hub-test" })
                "#,
            )
            .eval()
            .expect("render hello with no path");

        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_not_error_fallback(&parsed, "hello");

        let text_blob = parsed.to_string();
        assert!(
            text_blob.contains("Phase 4b sub-routes demo"),
            "nil/unset state.path must route to the home sub-page, got: {text_blob}"
        );
    });
}

#[test]
fn hello_plugin_unknown_subpath_renders_sub_404_not_error_fallback() {
    // A known surface with an unknown subpath is NOT the same as an unknown
    // surface: the dispatcher returns the sub-route 404 tree, which is
    // still a valid UiNodeV1 and must NOT be confused with the
    // Rust-level "Layout error:" fallback.
    let _lock = lock_env();
    let lua = new_test_lua();

    with_demo_env(None, Some("test"), || {
        let json: String = lua
            .load(
                r#"
                require("plugins.hello_surface.plugin")
                return web_layout.render("hello", {
                    hub_id = "hub-test",
                    path = "/does-not-exist",
                })
                "#,
            )
            .eval()
            .expect("render hello unknown sub");

        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_not_error_fallback(&parsed, "hello");

        let text_blob = parsed.to_string();
        assert!(
            text_blob.contains("Sub-route not found"),
            "unknown subpath must render the dispatcher 404 tree, got: {text_blob}"
        );
    });
}
