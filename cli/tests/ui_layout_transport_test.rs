//! Integration tests for Phase 2b — hub-side UI DSL transport + actions.
//!
//! These tests spin up a minimal Lua VM with just enough shim around the real
//! `cli/lua/` sources to exercise the three observable Phase-2b behaviors:
//!
//!   1. `lib.layout_broadcast` hashes the rendered tree JSON and skips
//!      re-emitting frames whose version matches the last-sent baseline.
//!   2. `lib.action` dispatches envelopes to registered handlers and falls
//!      back to the Phase-1 legacy hub command for unhandled semantic action
//!      ids that the browser already knew how to emit.
//!   3. `handlers.commands` registers a `ui_action_v1` command that routes
//!      envelopes through `action.dispatch(...)`.
//!
//! The VM here does NOT boot the full hub event loop — mocking every hub
//! primitive would obscure the behavior under test. Instead we stub out the
//! Lua-side dependencies that the production code expects (`log`, `hub`,
//! `web_layout`, etc.) and load the real `lib.*` modules directly so we are
//! asserting against shipped code.
//!
//! Run with: `./test.sh -- ui_layout_transport` from `cli/`.

// Rust guideline compliant 2026-04-18

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    clippy::match_single_binding,
    clippy::needless_raw_string_hashes,
    clippy::needless_borrows_for_generic_args,
    clippy::stable_sort_primitive,
    clippy::ignored_unit_patterns,
    reason = "test-code brevity"
)]

use std::path::PathBuf;
use std::sync::Mutex;

use botster::lua::primitives::web_layout;
use botster::ui_contract::lua::register as register_ui_contract;
use mlua::{Lua, Table, Value};

// -------------------------------------------------------------------------
// Test VM setup
// -------------------------------------------------------------------------

/// Global lock to serialise tests that mutate process env (the `web_layout`
/// primitive reads `BOTSTER_CONFIG_DIR` / `BOTSTER_WEB_LAYOUT_REPO_DIR`).
static RENDER_LOCK: Mutex<()> = Mutex::new(());

fn cli_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lua_src_dir() -> PathBuf {
    cli_manifest_dir().join("lua")
}

/// Stub `log` and `hub` globals that the real Lua libs expect at load time.
/// We use no-op stubs because this test cares about logic outcomes, not
/// observability. `hub.server_id` returns a stable test id so layout frames
/// carry a predictable `hub_id`.
fn install_lua_stubs(lua: &Lua) {
    let globals = lua.globals();

    // log.{debug,info,warn,error}
    let log_tbl = lua.create_table().unwrap();
    for name in ["debug", "info", "warn", "error"] {
        let f = lua.create_function(|_, _: Value| Ok(())).unwrap();
        log_tbl.set(name, f).unwrap();
    }
    globals.set("log", log_tbl).unwrap();

    // hub.server_id -> stable id; other hub functions return nil/noops.
    let hub_tbl = lua.create_table().unwrap();
    let server_id = lua.create_function(|_, _: ()| Ok("hub-test")).unwrap();
    hub_tbl.set("server_id", server_id).unwrap();
    globals.set("hub", hub_tbl).unwrap();

    // json.encode/decode — the libs rely on a global `json` table. Bridge
    // through mlua's JSON helpers so encoded shapes match production.
    let json_tbl: Table = lua.load(r#"
        local j = {}
        local serpent_encode  -- forward decl
        -- Minimal round-trip via Lua's own json serialization is awkward; use
        -- the `json` primitive already installed by register_all IF present.
        return j
    "#).eval().unwrap_or_else(|_| lua.create_table().unwrap());
    // Fall back to the real primitive if it got registered below; tests that
    // need json.encode for fingerprinting will exercise the primitive path.
    let _ = json_tbl;

    // hooks — the real lib.commands calls hooks.call / hooks.notify. Stub
    // to pass-through / no-op.
    let hooks_tbl: Table = lua
        .load(
            r#"
            local h = {}
            function h.call(_event, payload) return payload end
            function h.notify(_event, _payload) end
            function h.on(_event, _name, _fn) end
            return h
            "#,
        )
        .eval()
        .unwrap();
    globals.set("hooks", hooks_tbl).unwrap();

    // state — lib/action + lib/layout_broadcast use state.get/set. Minimal
    // in-memory implementation.
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
            return M
            "#,
        )
        .eval()
        .unwrap();
    globals.set("state", state_tbl).unwrap();
}

/// Build a Lua VM with:
/// - the real `ui` DSL
/// - the real `web_layout` primitive + `json` global
/// - `package.path` pointing at `cli/lua/` so `require("lib.*")` finds the
///   shipped modules
/// - stubs for runtime globals the shipped code expects to find pre-installed
fn new_test_lua() -> Lua {
    let lua = Lua::new();
    register_ui_contract(&lua).expect("register ui_contract");
    web_layout::register(&lua).expect("register web_layout");
    // Install `json` and other primitive globals that lib code relies on.
    botster::lua::primitives::json::register(&lua).expect("register json");

    install_lua_stubs(&lua);

    let dir = lua_src_dir();
    let code = format!(
        "package.path = \"{dir}/?.lua;{dir}/?/init.lua;\" .. package.path",
        dir = dir.display()
    );
    lua.load(&code).exec().expect("update package.path");

    // Reset per-test global caches.
    web_layout::_clear_override_cache_for_tests();

    lua
}

// -------------------------------------------------------------------------
// Fixture — a realistic AgentWorkspaceSurfaceInputV1 state
// -------------------------------------------------------------------------

/// Shared state fixtures. These are Lua table LITERALS (no leading `return`)
/// so they can be spliced directly into `local state = {state_literal}`.
const STATE_SINGLE: &str = r#"
    {
        hub_id = "hub-test",
        agents = {
            {
                id = "sess-1",
                session_uuid = "uuid-1",
                session_type = "agent",
                label = "api",
                display_name = "api",
                target_name = "backend",
                branch_name = "main",
                agent_name = "claude",
                is_idle = true,
            },
        },
        open_workspaces = {
            { id = "ws-1", name = "Backend", agents = { "sess-1" } },
        },
        selected_session_uuid = nil,
    }
"#;

const STATE_SINGLE_SELECTED: &str = r#"
    {
        hub_id = "hub-test",
        agents = {
            {
                id = "sess-1",
                session_uuid = "uuid-1",
                session_type = "agent",
                label = "api",
                display_name = "api",
                target_name = "backend",
                branch_name = "main",
                agent_name = "claude",
                is_idle = true,
            },
        },
        open_workspaces = {
            { id = "ws-1", name = "Backend", agents = { "sess-1" } },
        },
        selected_session_uuid = "uuid-1",
    }
"#;

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    let guard = RENDER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Point the override chain at a known-empty dir so the embedded default
    // always wins — same pattern as ui_contract_web_layout_test.rs.
    // SAFETY: Rust 2024 requires `unsafe` for env mutation. The calls are
    // serialised by `RENDER_LOCK` above so parallel tests cannot observe a
    // half-set state; single-thread visibility is sufficient for this use.
    unsafe {
        std::env::set_var(
            "BOTSTER_WEB_LAYOUT_REPO_DIR",
            "/tmp/botster-nonexistent-ui-layout-transport",
        );
        std::env::set_var(
            "BOTSTER_CONFIG_DIR",
            "/tmp/botster-nonexistent-ui-layout-transport",
        );
        std::env::remove_var("BOTSTER_DEV");
    }
    guard
}

// -------------------------------------------------------------------------
// layout_broadcast tests
// -------------------------------------------------------------------------

#[test]
fn layout_broadcast_emits_two_frames_for_two_densities() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let frames: Table = lua
        .load(&format!(
            r#"
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()
            local state = {state}
            return LayoutBroadcast.build_frames(state, {{ force = true }})
            "#,
            state = STATE_SINGLE
        ))
        .eval()
        .expect("build_frames");

    let len = frames.raw_len();
    assert_eq!(
        len, 2,
        "expected one frame per target surface (sidebar + panel), got {len}"
    );

    let f1: Table = frames.raw_get(1).unwrap();
    let f2: Table = frames.raw_get(2).unwrap();
    let t1: String = f1.get("target_surface").unwrap();
    let t2: String = f2.get("target_surface").unwrap();
    let mut surfaces = [t1.as_str(), t2.as_str()];
    surfaces.sort();
    assert_eq!(surfaces, ["workspace_panel", "workspace_sidebar"]);

    // Every frame carries a version hash and the wire type.
    for f in [&f1, &f2] {
        let ty: String = f.get("type").unwrap();
        assert_eq!(ty, "ui_layout_tree_v1");
        let v: String = f.get("version").unwrap();
        assert_eq!(
            v.len(),
            16,
            "fnv1a-64 version must be 16 hex chars, got {v}"
        );
        let hub_id: String = f.get("hub_id").unwrap();
        assert_eq!(hub_id, "hub-test");
        // Tree is a UiNodeV1-shaped table, not a pre-serialised string.
        let tree: Table = f.get("tree").unwrap();
        let node_type: String = tree.get("type").unwrap();
        assert!(!node_type.is_empty());
    }
}

#[test]
fn layout_broadcast_dedups_unchanged_renders() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (first_len, second_len): (i64, i64) = lua
        .load(&format!(
            r#"
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()
            local state = {state}

            -- First emission: both frames are new.
            local f1 = LayoutBroadcast.build_frames(state)
            LayoutBroadcast.mark_sent(f1)

            -- Second emission with identical state: dedup must suppress both.
            local f2 = LayoutBroadcast.build_frames(state)
            return #f1, #f2
            "#,
            state = STATE_SINGLE
        ))
        .eval()
        .expect("dedup eval");

    assert_eq!(first_len, 2, "first emission must ship both densities");
    assert_eq!(
        second_len, 0,
        "second emission with identical state must be fully deduped"
    );
}

#[test]
fn layout_broadcast_reemits_when_selection_changes() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let (first_len, second_len): (i64, i64) = lua
        .load(&format!(
            r#"
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()
            local s1 = {s1}
            local s2 = {s2}
            local f1 = LayoutBroadcast.build_frames(s1)
            LayoutBroadcast.mark_sent(f1)
            local f2 = LayoutBroadcast.build_frames(s2)
            return #f1, #f2
            "#,
            s1 = STATE_SINGLE,
            s2 = STATE_SINGLE_SELECTED
        ))
        .eval()
        .expect("re-emit eval");

    assert_eq!(first_len, 2);
    assert_eq!(
        second_len, 2,
        "changing `selected_session_uuid` must re-emit both densities"
    );
}

#[test]
fn layout_broadcast_force_bypasses_dedup_for_priming() {
    let _lock = lock_env();
    let lua = new_test_lua();

    let len: i64 = lua
        .load(&format!(
            r#"
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()
            local state = {state}
            local first = LayoutBroadcast.build_frames(state)
            LayoutBroadcast.mark_sent(first)
            -- Force mode must ignore the cache and re-emit every frame so a
            -- newly-subscribing browser gets the current tree immediately.
            local primed = LayoutBroadcast.build_frames(state, {{ force = true }})
            return #primed
            "#,
            state = STATE_SINGLE
        ))
        .eval()
        .expect("force eval");

    assert_eq!(
        len, 2,
        "force=true must ship both frames regardless of cache state"
    );
}

#[test]
fn layout_broadcast_version_matches_fnv1a64_of_tree_json() {
    // Guardrail: the on-wire version must be exactly the 16-char hex FNV-1a
    // of the rendered tree JSON. Drift here would invalidate the
    // browser-side last-version comparison strategy.
    let _lock = lock_env();
    let lua = new_test_lua();

    let ok: bool = lua
        .load(&format!(
            r#"
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()
            local state = {state}
            local frames = LayoutBroadcast.build_frames(state, {{ force = true }})
            assert(#frames == 2)
            for _, frame in ipairs(frames) do
                local reserialised = json.encode(frame.tree)
                -- json.encode may not match web_layout.render's output char-
                -- for-char (key ordering). The version stored on the frame
                -- was computed from the web_layout.render output string,
                -- which we re-derive here via a round-trip render.
                local state_copy = {{}}
                for k, v in pairs(state) do state_copy[k] = v end
                state_copy.surface = (frame.target_surface == "workspace_sidebar") and "sidebar" or "panel"
                local json_str = web_layout.render("workspace_surface", state_copy)
                local expected = LayoutBroadcast._fnv1a64_hex(json_str)
                if frame.version ~= expected then
                    error(string.format(
                        "version drift for %s: frame=%s expected=%s",
                        frame.target_surface, frame.version, expected))
                end
                local _ = reserialised
            end
            return true
            "#,
            state = STATE_SINGLE
        ))
        .eval()
        .expect("version parity eval");
    assert!(ok);
}

// -------------------------------------------------------------------------
// action registry tests
// -------------------------------------------------------------------------

#[test]
fn action_handler_that_returns_handled_sentinel_suppresses_fallback() {
    let lua = new_test_lua();

    let via: String = lua
        .load(
            r#"
            local action = require("lib.action")
            action._reset_for_tests()

            local commands = require("lib.commands")
            local captured
            commands.dispatch = function(_c, _s, cmd) captured = cmd end

            local seen = {}
            action.on("botster.session.select", "owner", function(envelope, _ctx)
                seen[#seen + 1] = envelope.payload.sessionUuid
                return action.HANDLED
            end)

            local result = action.dispatch({
                id = "botster.session.select",
                payload = { sessionUuid = "uuid-1", sessionId = "sess-1" },
            }, {})

            assert(seen[1] == "uuid-1", "handler must receive payload")
            assert(captured == nil,
                "legacy fallback must NOT run when a handler returned HANDLED")
            return result.via
            "#,
        )
        .eval()
        .expect("handler eval");

    assert_eq!(via, "handler");
}

#[test]
fn action_falls_back_to_legacy_command_for_known_ids() {
    let lua = new_test_lua();

    // Provide a stub `lib.commands.dispatch` that records the dispatched
    // command type; this stands in for the real command registry without
    // needing to spin up all the hub handler state.
    let via: String = lua
        .load(
            r#"
            local action = require("lib.action")
            action._reset_for_tests()

            local commands = require("lib.commands")
            -- Replace dispatch with a capturing version for this test.
            local captured
            commands.dispatch = function(_client, _sub_id, cmd)
                captured = cmd
            end

            local result = action.dispatch({
                id = "botster.session.preview.toggle",
                payload = { sessionUuid = "uuid-xyz" },
            }, {})

            assert(captured ~= nil, "fallback must have dispatched a legacy command")
            assert(captured.type == "toggle_hosted_preview",
                string.format("expected legacy type toggle_hosted_preview, got %s", tostring(captured.type)))
            assert(captured.session_uuid == "uuid-xyz", "payload remapped onto session_uuid")
            return result.via
            "#,
        )
        .eval()
        .expect("fallback eval");

    assert_eq!(via, "fallback");
}

#[test]
fn action_reports_unhandled_for_unknown_ids_with_no_handler() {
    let lua = new_test_lua();

    let (via, unhandled): (String, bool) = lua
        .load(
            r#"
            local action = require("lib.action")
            action._reset_for_tests()

            local result = action.dispatch({
                id = "my.custom.unrouted.action",
                payload = { foo = 1 },
            }, {})

            return result.via, (result.handled == false)
            "#,
        )
        .eval()
        .expect("unhandled eval");

    assert_eq!(via, "unhandled");
    assert!(unhandled);
}

#[test]
fn action_off_removes_registered_handler() {
    let lua = new_test_lua();

    let (via_before, count_before): (String, i64) = lua
        .load(
            r#"
            local action = require("lib.action")
            action._reset_for_tests()
            action.on("botster.workspace.toggle", "stub", function() return action.HANDLED end)
            local r = action.dispatch({ id = "botster.workspace.toggle", payload = {} }, {})
            return r.via, r.handler_count
            "#,
        )
        .eval()
        .unwrap();
    assert_eq!(via_before, "handler");
    assert_eq!(count_before, 1);

    let (via_after, count_after): (String, i64) = lua
        .load(
            r#"
            local action = require("lib.action")
            action.off("botster.workspace.toggle", "stub")
            local r = action.dispatch({ id = "botster.workspace.toggle", payload = {} }, {})
            return r.via, r.handler_count
            "#,
        )
        .eval()
        .unwrap();
    // workspace.toggle has no fallback → unhandled after off.
    assert_eq!(via_after, "unhandled");
    assert_eq!(count_after, 0);
}

// -------------------------------------------------------------------------
// F1 regression tests (codex) — observer handlers must NOT swallow fallback
// -------------------------------------------------------------------------

#[test]
fn action_observer_that_returns_nil_still_runs_legacy_fallback() {
    // An observer plugin registers for botster.session.select, returns nil,
    // and the legacy select_agent command MUST still run. Pre-fix, the
    // dispatch treated "any handler ran" as "consumed" and suppressed the
    // fallback — a silent Phase-1 regression.
    let lua = new_test_lua();

    let (via, captured_type, observer_saw): (String, String, String) = lua
        .load(
            r#"
            local action = require("lib.action")
            action._reset_for_tests()

            local commands = require("lib.commands")
            local captured
            commands.dispatch = function(_c, _s, cmd) captured = cmd end

            local observed
            action.on("botster.session.select", "observer", function(envelope, _ctx)
                observed = envelope.payload.sessionUuid
                -- explicit: return nothing (not HANDLED)
            end)

            local result = action.dispatch({
                id = "botster.session.select",
                payload = { sessionUuid = "uuid-observed", sessionId = "sess-o" },
            }, {})

            return result.via, tostring(captured and captured.type), tostring(observed)
            "#,
        )
        .eval()
        .expect("observer eval");

    assert_eq!(
        via, "fallback",
        "observer returning nil must leave fallback intact"
    );
    assert_eq!(
        captured_type, "select_agent",
        "legacy select_agent command must have dispatched"
    );
    assert_eq!(
        observer_saw, "uuid-observed",
        "observer still ran and received the envelope"
    );
}

#[test]
fn action_raising_handler_is_logged_and_fallback_still_fires() {
    // A handler that raises is a bug — but it must NOT take out the
    // legacy fallback. Other observers continue, and the fallback runs
    // unless another handler explicitly claimed HANDLED.
    let lua = new_test_lua();

    let (via, captured_type, second_ran): (String, String, bool) = lua
        .load(
            r#"
            local action = require("lib.action")
            action._reset_for_tests()

            local commands = require("lib.commands")
            local captured
            commands.dispatch = function(_c, _s, cmd) captured = cmd end

            action.on("botster.session.preview.toggle", "broken", function(_envelope)
                error("simulated plugin crash")
            end)

            local second_ran = false
            action.on("botster.session.preview.toggle", "survivor", function(_envelope)
                second_ran = true
                -- return nil -> observer, fallback still runs
            end)

            local result = action.dispatch({
                id = "botster.session.preview.toggle",
                payload = { sessionUuid = "uuid-err" },
            }, {})

            return result.via, tostring(captured and captured.type), second_ran
            "#,
        )
        .eval()
        .expect("raise eval");

    assert_eq!(
        via, "fallback",
        "raising handler does not consume fallback"
    );
    assert_eq!(captured_type, "toggle_hosted_preview");
    assert!(
        second_ran,
        "handler chain continues past a raising handler"
    );
}

#[test]
fn action_multiple_observers_with_one_claim_suppresses_fallback() {
    // Mixed chain: observer + HANDLED + observer. Fallback suppressed
    // because at least one handler claimed ownership. Verifies the
    // short-circuit happens on accumulation (all run), not early-return.
    let lua = new_test_lua();

    let (via, total_ran, captured_nil): (String, i64, bool) = lua
        .load(
            r#"
            local action = require("lib.action")
            action._reset_for_tests()

            local commands = require("lib.commands")
            local captured
            commands.dispatch = function(_c, _s, cmd) captured = cmd end

            local ran = 0
            action.on("botster.session.select", "pre_observer", function() ran = ran + 1 end)
            action.on("botster.session.select", "owner", function()
                ran = ran + 1
                return action.HANDLED
            end)
            action.on("botster.session.select", "post_observer", function() ran = ran + 1 end)

            local result = action.dispatch({
                id = "botster.session.select",
                payload = { sessionUuid = "uuid-m" },
            }, {})

            return result.via, ran, (captured == nil)
            "#,
        )
        .eval()
        .expect("mixed eval");

    assert_eq!(via, "handler");
    assert_eq!(total_ran, 3, "every handler runs regardless of HANDLED claim");
    assert!(captured_nil, "fallback suppressed when any handler returns HANDLED");
}

// -------------------------------------------------------------------------
// Wire format — `ui_action_v1` command registration
// -------------------------------------------------------------------------

#[test]
fn ui_action_v1_command_registers_and_routes_through_action_dispatch() {
    let lua = new_test_lua();

    // Load handlers/commands so it runs its top-level `commands.register`
    // block. We stub out the transitive `handlers.agents` etc. that
    // commands.lua lazy-requires so the file's top-level registrations
    // succeed without booting the full hub.
    let via: String = lua
        .load(
            r#"
            -- Preload shims for lazy requires inside handlers/commands.lua.
            package.loaded["lib.target_context"] = {
                resolve = function() return nil, "stubbed" end,
            }
            package.loaded["handlers.agents"] = {
                handle_delete_session = function() end,
            }
            package.loaded["lib.hub"] = {
                get = function() return { list_workspaces = function() return {} end } end,
            }
            package.loaded["lib.session"] = {}
            package.loaded["lib.hosted_preview"] = {}
            package.loaded["lib.workspace_store"] = {}
            _G.connection = {
                generate = function() end,
                regenerate = function() end,
                copy_url = function() end,
            }
            _G.config = { data_dir = function() return nil end }
            _G.worktree = {}

            -- Minimal commands module is the real lib/commands.lua; handlers
            -- top-level registrations will call .register() on it.
            local commands = require("lib.commands")

            -- Capture dispatch for the fallback assertion below.
            local original_dispatch = commands.dispatch
            local captured
            commands.dispatch = function(client, sub_id, cmd)
                captured = cmd
                return original_dispatch(client, sub_id, cmd)
            end

            -- Execute handlers/commands.lua so ui_action_v1 gets registered.
            -- It's safe to partial-execute — we don't drive any of its other
            -- commands in this test. Wrapping in pcall lets us ignore any
            -- missing globals after the registration we care about.
            local ok, err = pcall(function()
                dofile(package.path:match("([^;]+);?"):gsub("?", "handlers/commands"))
            end)
            -- We don't assert ok here: commands.lua has other requires that
            -- may fail in this stripped harness. What matters is whether the
            -- ui_action_v1 registration landed.
            if not commands.has("ui_action_v1") then
                error("ui_action_v1 command was not registered (pcall err: " .. tostring(err) .. ")")
            end

            -- Dispatch a ui_action_v1 command whose envelope maps onto a
            -- known legacy command via the action fallback chain.
            local action = require("lib.action")
            action._reset_for_tests()

            captured = nil
            commands.dispatch(nil, "sub-1", {
                type = "ui_action_v1",
                target_surface = "workspace_sidebar",
                envelope = {
                    id = "botster.session.preview.toggle",
                    payload = { sessionUuid = "uuid-ui" },
                },
            })

            assert(captured ~= nil, "fallback command was not captured")
            assert(captured.type == "toggle_hosted_preview",
                string.format("captured.type=%s", tostring(captured.type)))
            return "fallback"
            "#,
        )
        .eval()
        .expect("ui_action_v1 routing eval");

    assert_eq!(via, "fallback");
}

// -------------------------------------------------------------------------
// F2 regression tests (codex) — per-subscription selection threading
// -------------------------------------------------------------------------

#[test]
fn layout_broadcast_versions_diverge_per_subscription() {
    // Two subscriptions on the same hub with different selections must
    // produce different version hashes for the same logical surface. If
    // the hash were a function of only the structural state (and not the
    // selection), a selection change on client A could be silently
    // deduped on client B.
    let _lock = lock_env();
    let lua = new_test_lua();

    let (v_a, v_b): (String, String) = lua
        .load(
            r#"
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()

            local state_a = {
                hub_id = "hub-test",
                agents = {
                    { id = "sess-1", session_uuid = "uuid-1", session_type = "agent",
                      display_name = "a", target_name = "t", branch_name = "main", agent_name = "c",
                      is_idle = true },
                    { id = "sess-2", session_uuid = "uuid-2", session_type = "agent",
                      display_name = "b", target_name = "t", branch_name = "main", agent_name = "c",
                      is_idle = true },
                },
                open_workspaces = {
                    { id = "ws-1", name = "W", agents = { "sess-1", "sess-2" } },
                },
                selected_session_uuid = "uuid-1",
            }
            local state_b = {}
            for k, v in pairs(state_a) do state_b[k] = v end
            state_b.selected_session_uuid = "uuid-2"

            local fa = LayoutBroadcast.build_frames(state_a, { force = true, subscription_key = "sub-a" })
            local fb = LayoutBroadcast.build_frames(state_b, { force = true, subscription_key = "sub-b" })

            -- Pick the panel target for comparison; sidebar picks would work
            -- equivalently.
            local va, vb
            for _, f in ipairs(fa) do
                if f.target_surface == "workspace_panel" then va = f.version end
            end
            for _, f in ipairs(fb) do
                if f.target_surface == "workspace_panel" then vb = f.version end
            end
            return va, vb
            "#,
        )
        .eval()
        .expect("per-sub eval");

    assert_ne!(
        v_a, v_b,
        "different `selected_session_uuid` inputs must produce different versions"
    );
}

#[test]
fn layout_broadcast_dedup_is_scoped_per_subscription_key() {
    // sub-a and sub-b share identical state. Each should dedupe against
    // its own baseline independently: sub-a's mark_sent must not silence
    // sub-b's first broadcast.
    let _lock = lock_env();
    let lua = new_test_lua();

    let (a_first, a_second, b_first): (i64, i64, i64) = lua
        .load(&format!(
            r#"
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()
            local state = {state}

            local fa1 = LayoutBroadcast.build_frames(state, {{ subscription_key = "sub-a" }})
            LayoutBroadcast.mark_sent(fa1, {{ subscription_key = "sub-a" }})
            local fa2 = LayoutBroadcast.build_frames(state, {{ subscription_key = "sub-a" }})

            -- sub-b sees the same logical state but has its own baseline —
            -- it should receive the full primed set.
            local fb1 = LayoutBroadcast.build_frames(state, {{ subscription_key = "sub-b" }})
            return #fa1, #fa2, #fb1
            "#,
            state = STATE_SINGLE
        ))
        .eval()
        .expect("per-sub dedup eval");

    assert_eq!(a_first, 2, "sub-a first emission: both densities");
    assert_eq!(a_second, 0, "sub-a second emission (same state): deduped");
    assert_eq!(b_first, 2, "sub-b's first emission is NOT suppressed by sub-a's baseline");
}

#[test]
fn layout_broadcast_forget_drops_subscription_baseline() {
    // After `forget(sub_id)`, the next build_frames for that sub_id must
    // ship the full set even if the state is unchanged — mirrors the
    // Client:handle_unsubscribe / Client:disconnect cleanup path.
    let _lock = lock_env();
    let lua = new_test_lua();

    let (before_forget, after_forget): (i64, i64) = lua
        .load(&format!(
            r#"
            local LayoutBroadcast = require("lib.layout_broadcast")
            LayoutBroadcast._reset_for_tests()
            local state = {state}
            local first = LayoutBroadcast.build_frames(state, {{ subscription_key = "sub-c" }})
            LayoutBroadcast.mark_sent(first, {{ subscription_key = "sub-c" }})

            local deduped = LayoutBroadcast.build_frames(state, {{ subscription_key = "sub-c" }})
            LayoutBroadcast.forget("sub-c")
            local after = LayoutBroadcast.build_frames(state, {{ subscription_key = "sub-c" }})
            return #deduped, #after
            "#,
            state = STATE_SINGLE
        ))
        .eval()
        .expect("forget eval");

    assert_eq!(before_forget, 0, "within-baseline renders are deduped");
    assert_eq!(after_forget, 2, "forget() resets the baseline to primed state");
}

#[test]
fn layout_input_threads_client_selection_into_state() {
    // Regression guard: `build_for_subscription(client, sub_id)` must copy
    // `client.selected_session_uuid` onto the returned state so the
    // layout's `selected = true` marker applies to the right row. Pre-fix,
    // the builder hard-coded nil.
    let lua = new_test_lua();

    // Spin up the real lib/layout_input with stubs: lib/agent and
    // lib/agent_list_payload need to return something valid even if empty.
    let selected: Option<String> = lua
        .load(
            r#"
            -- Stub `lib.agent` + `lib.agent_list_payload` so the builder
            -- doesn't need a full hub. layout_input consumes them only for
            -- the agents/workspaces fields, which we don't assert here.
            package.loaded["lib.agent"] = {
                all_info = function() return {} end,
            }
            package.loaded["lib.agent_list_payload"] = {
                build = function(_) return { agents = {}, workspaces = {} } end,
            }

            local LayoutInput = require("lib.layout_input")
            local client = { selected_session_uuid = "uuid-selected" }
            local state = LayoutInput.build_for_subscription(client, "sub-x")
            return state.selected_session_uuid
            "#,
        )
        .eval()
        .expect("layout_input eval");

    assert_eq!(selected.as_deref(), Some("uuid-selected"));
}

// -------------------------------------------------------------------------
// FNV-1a helper — sanity: known-value test ensures the hash can't silently
// drift in future refactors.
// -------------------------------------------------------------------------

#[test]
fn fnv1a64_is_stable_and_independent_of_test_state() {
    let lua = new_test_lua();
    // Known reference: FNV-1a 64 of the empty string is 0xcbf29ce484222325.
    let hex: String = lua
        .load(
            r#"
            local LB = require("lib.layout_broadcast")
            return LB._fnv1a64_hex("")
            "#,
        )
        .eval()
        .unwrap();
    assert_eq!(hex, "cbf29ce484222325");

    // Two distinct inputs yield distinct hashes.
    let (a, b): (String, String) = lua
        .load(
            r#"
            local LB = require("lib.layout_broadcast")
            return LB._fnv1a64_hex("a"), LB._fnv1a64_hex("b")
            "#,
        )
        .eval()
        .unwrap();
    assert_ne!(a, b);
}

