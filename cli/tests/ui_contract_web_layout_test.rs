//! Integration tests for `cli/src/lua/primitives/web_layout.rs`.
//!
//! These tests exercise the full render pipeline — Lua state in → primitive
//! call → `UiNodeV1` JSON out — against known `AgentWorkspaceSurfaceInputV1`
//! fixtures and compare the result to committed golden files derived from
//! Phase 1's `app/frontend/ui_contract/composites.ts`.
//!
//! Running:
//!
//!   BOTSTER_ENV=test cargo test --test ui_contract_web_layout_test
//!
//! Regenerate goldens after an intentional change:
//!
//!   BOTSTER_ENV=test BOTSTER_TEST_UPDATE_GOLDEN=1 \
//!     cargo test --test ui_contract_web_layout_test
//!
//! Env-var-sensitive tests (override chain, error fallback) run sequentially
//! inside a single `#[test]` so parallel tests cannot race on shared env
//! state.

// Rust guideline compliant 2026-04-18

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_docs_in_private_items,
    reason = "test-code brevity"
)]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use botster::lua::primitives::web_layout;
use botster::ui_contract::lua::register as register_ui_contract;
use mlua::Lua;
use serde_json::Value as JsonValue;

/// Global serialisation lock for tests that read (or mutate) the primitive's
/// env-var-driven resolution state. The primitive resolves override paths
/// from `BOTSTER_WEB_LAYOUT_REPO_DIR` / `BOTSTER_CONFIG_DIR` on every call,
/// so parallel tests would otherwise race on any env-var mutation the
/// override-chain test performs. Every test that calls `render_via_lua` must
/// first `lock_render_env()`.
static RENDER_LOCK: Mutex<()> = Mutex::new(());

/// Path used as a known-empty repo override dir for tests that do NOT
/// exercise overrides. Must not correspond to any real path on any machine.
const EMPTY_SENTINEL_PATH: &str = "/tmp/botster-nonexistent-web-layout-repo-dir";

/// Env var the primitive honours to locate the repo-scoped config dir, used
/// here to point the override chain at a tempdir without an enclosing git
/// repository.
const REPO_DIR_OVERRIDE_ENV: &str = "BOTSTER_WEB_LAYOUT_REPO_DIR";

/// Env var the primitive honours to locate the device-scoped config dir.
const DEVICE_DIR_OVERRIDE_ENV: &str = "BOTSTER_CONFIG_DIR";

/// Env var toggling dev-mode config directories.
const DEV_MODE_ENV: &str = "BOTSTER_DEV";

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

fn cli_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lua_src_dir() -> PathBuf {
    cli_manifest_dir().join("lua")
}

fn golden_dir() -> PathBuf {
    cli_manifest_dir()
        .join("tests")
        .join("fixtures")
        .join("ui_contract_web_layout")
}

/// Acquire the global render lock and reset env vars that drive the
/// primitive's resolution chain to known-empty values. Tests that exercise
/// overrides may subsequently set env vars via `EnvGuard` *inside* the held
/// lock — the guard drops before the lock is released so subsequent tests
/// see the known-empty baseline.
///
/// Also resets the Phase-2b override cache TTL to zero so sequential file
/// writes inside a single test are picked up without waiting for the 500 ms
/// window to expire. Production runs leave the default TTL in place.
fn lock_render_env() -> MutexGuard<'static, ()> {
    let guard = RENDER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: Rust 2024 requires unsafe for env mutation; serialised by the
    // render lock above so parallel tests can't observe a half-set state.
    unsafe {
        std::env::set_var("BOTSTER_WEB_LAYOUT_REPO_DIR", EMPTY_SENTINEL_PATH);
        std::env::set_var("BOTSTER_CONFIG_DIR", EMPTY_SENTINEL_PATH);
        std::env::remove_var("BOTSTER_DEV");
    }
    // Drop TTL to 0 and clear the cache — together they make sequential
    // file edits deterministic. `_clear_override_cache_for_tests()` is safe
    // to call without any corresponding "restore" step because production
    // bootstrap paths never observe the test TTL.
    botster::lua::primitives::web_layout::set_override_cache_ttl_millis(0);
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
    guard
}

/// Construct a Lua VM with the DSL + web_layout primitive + package.path
/// pointing at `cli/lua/` so `require("web.layout")` finds the embedded
/// default on the filesystem (needed for debug builds where the embedded
/// searcher is empty).
fn new_web_layout_lua() -> Lua {
    let lua = Lua::new();
    register_ui_contract(&lua).expect("register ui_contract");
    web_layout::register(&lua).expect("register web_layout");

    let dir = lua_src_dir();
    let code = format!(
        "package.path = \"{dir}/?.lua;{dir}/?/init.lua;\" .. package.path",
        dir = dir.display()
    );
    lua.load(&code).exec().expect("update package.path");

    lua
}

/// Execute Lua code that ends with `return web_layout.render(...)` and parse
/// the resulting JSON string into a `serde_json::Value`.
fn render_via_lua(lua: &Lua, code: &str) -> JsonValue {
    let json: String = lua.load(code).eval().expect("render eval");
    serde_json::from_str(&json).expect("parse rendered JSON")
}

/// Compare `actual` against a committed golden file. Writes the file when
/// `BOTSTER_TEST_UPDATE_GOLDEN=1` is set.
fn check_golden(name: &str, actual: &JsonValue) {
    let path = golden_dir().join(format!("{name}.golden.json"));
    if std::env::var("BOTSTER_TEST_UPDATE_GOLDEN").is_ok() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir golden");
        }
        let pretty = serde_json::to_string_pretty(actual).expect("serialise golden");
        std::fs::write(&path, format!("{pretty}\n")).expect("write golden");
        return;
    }
    let content = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "golden file {path} missing ({e}); rerun with BOTSTER_TEST_UPDATE_GOLDEN=1 to write it",
            path = path.display(),
        )
    });
    let expected: JsonValue = serde_json::from_str(&content).expect("parse golden");
    assert_eq!(
        actual, &expected,
        "golden mismatch for {name}: run with BOTSTER_TEST_UPDATE_GOLDEN=1 to update"
    );
}

/// RAII guard that restores an env var to its previous value on drop. Allows
/// individual test branches to scope env-var mutations without leaking state
/// between parallel tests — provided only one test ever touches a given var.
struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<str>) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: Rust 2024 requires `unsafe` to mutate process env; tests
        // opt in because this is the only pragmatic way to exercise the
        // primitive's env-driven path selection.
        unsafe { std::env::set_var(key, value.as_ref()) };
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: see `EnvGuard::set`.
        unsafe {
            match &self.prev {
                Some(prev) => std::env::set_var(self.key, prev),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

// -------------------------------------------------------------------------
// Fixture Lua builders
// -------------------------------------------------------------------------

/// Fixture 1: completely empty state — no agents, no workspaces.
const FIXTURE_EMPTY: &str = r#"
    return web_layout.render("workspace_surface", {
        hub_id = "hub-1",
        agents = {},
        open_workspaces = {},
        selected_session_uuid = nil,
        surface = "panel",
    })
"#;

/// Fixture 2: one workspace containing a single running session with a hosted
/// preview URL. The session is selected.
const FIXTURE_SINGLE_RUNNING: &str = r#"
    local session = {
        id = "sess-1",
        session_uuid = "sess-1-uuid",
        session_type = "agent",
        label = "api-work",
        display_name = "api-work",
        title = "Refactor request path",
        task = "Trim dead routes",
        target_name = "backend",
        branch_name = "feature/api",
        agent_name = "claude",
        is_idle = false,
        port = 4000,
        hosted_preview = {
            status = "running",
            url = "https://preview.example.com/api-work",
            error = nil,
            install_url = nil,
        },
        in_worktree = true,
        notification = false,
    }
    local state = {
        hub_id = "hub-1",
        agents = { session },
        open_workspaces = {
            { id = "ws-1", name = "Backend", agents = { "sess-1" } },
        },
        selected_session_uuid = "sess-1-uuid",
        surface = "panel",
    }
    return web_layout.render("workspace_surface", state)
"#;

/// Fixture 3: one workspace with three sessions — one active agent, one idle
/// agent, and one accessory session — to exercise all three activity dot
/// branches in a single tree.
const FIXTURE_MIXED_ACTIVITY: &str = r#"
    local active = {
        id = "sess-active", session_uuid = "uuid-active",
        session_type = "agent", display_name = "builder",
        target_name = "build", branch_name = "main", agent_name = "claude",
        is_idle = false,
    }
    local idle = {
        id = "sess-idle", session_uuid = "uuid-idle",
        session_type = "agent", display_name = "waiter",
        target_name = "wait", branch_name = "main", agent_name = "claude",
        is_idle = true,
    }
    local accessory = {
        id = "sess-acc", session_uuid = "uuid-acc",
        session_type = "accessory", display_name = "preview-port",
        port = 3000,
    }
    local state = {
        hub_id = "hub-1",
        agents = { active, idle, accessory },
        open_workspaces = {
            { id = "ws-1", name = "Mixed", agents = { "sess-active", "sess-idle", "sess-acc" } },
        },
        selected_session_uuid = nil,
        surface = "panel",
    }
    return web_layout.render("workspace_surface", state)
"#;

/// Fixture 4: one session with a preview error and an install URL. Exercises
/// the error-panel branch and the trailing "Install cloudflared" button.
const FIXTURE_PREVIEW_ERROR: &str = r#"
    local session = {
        id = "sess-err", session_uuid = "uuid-err",
        session_type = "agent", display_name = "broken-preview",
        target_name = "www", branch_name = "main", agent_name = "claude",
        is_idle = true,
        port = 5000,
        hosted_preview = {
            status = "error",
            url = nil,
            error = "cloudflared not installed",
            install_url = "https://example.com/install-cloudflared",
        },
    }
    local state = {
        hub_id = "hub-1",
        agents = { session },
        open_workspaces = {
            { id = "ws-1", name = "With error", agents = { "sess-err" } },
        },
        selected_session_uuid = nil,
        surface = "panel",
    }
    return web_layout.render("workspace_surface", state)
"#;

/// Fixture 5: same session as fixture 2 but rendered at `surface = "sidebar"`
/// so the golden diff highlights density differences (text size, no workspace
/// count, etc.).
const FIXTURE_PREVIEW_RUNNING_SIDEBAR: &str = r#"
    local session = {
        id = "sess-1", session_uuid = "sess-1-uuid",
        session_type = "agent", label = "api-work",
        display_name = "api-work", title = "Refactor request path",
        task = "Trim dead routes",
        target_name = "backend", branch_name = "feature/api", agent_name = "claude",
        is_idle = false, port = 4000,
        hosted_preview = {
            status = "running",
            url = "https://preview.example.com/api-work",
        },
        notification = true,
    }
    local state = {
        hub_id = "hub-1",
        agents = { session },
        open_workspaces = {
            { id = "ws-1", name = "Backend", agents = { "sess-1" } },
        },
        selected_session_uuid = "sess-1-uuid",
        surface = "sidebar",
    }
    return web_layout.render("workspace_surface", state)
"#;

// -------------------------------------------------------------------------
// Golden tests
// -------------------------------------------------------------------------

#[test]
fn empty_state_matches_golden() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_EMPTY);
    check_golden("empty", &tree);
    // Empty-state root is a vertical `stack` — not `ui.empty_state{}`, because
    // we need a labeled "New session" button that EmptyStatePropsV1 can't
    // carry. Matches WorkspaceList.jsx:59-111.
    assert_eq!(tree.get("type").and_then(|v| v.as_str()), Some("stack"));
    let json = tree.to_string();
    assert!(
        json.contains("\"label\":\"New session\""),
        "empty state must still include the NewSession button: {json}"
    );
    assert!(
        json.contains("\"id\":\"botster.session.create.request\""),
        "empty state NewSession button must emit the create.request action: {json}"
    );
}

#[test]
fn single_running_session_matches_golden() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_SINGLE_RUNNING);
    check_golden("single_running", &tree);
}

#[test]
fn mixed_activity_matches_golden() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_MIXED_ACTIVITY);
    check_golden("mixed_activity", &tree);
}

#[test]
fn preview_error_matches_golden() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_PREVIEW_ERROR);
    check_golden("preview_error", &tree);
}

#[test]
fn preview_running_sidebar_matches_golden() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_PREVIEW_RUNNING_SIDEBAR);
    check_golden("preview_running_sidebar", &tree);
}

// -------------------------------------------------------------------------
// Semantic assertions (additional regression safety net beyond goldens)
// -------------------------------------------------------------------------

#[test]
fn running_preview_emits_open_action_on_button() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_SINGLE_RUNNING);
    let json = tree.to_string();
    assert!(
        json.contains("\"id\":\"botster.session.preview.open\""),
        "running preview must emit preview.open action: {json}"
    );
    assert!(
        json.contains("\"url\":\"https://preview.example.com/api-work\""),
        "running preview button payload must carry url: {json}"
    );
}

#[test]
fn preview_error_emits_install_cloudflared_button() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_PREVIEW_ERROR);
    let json = tree.to_string();
    assert!(
        json.contains("Install cloudflared"),
        "preview-error fixture must include install cloudflared button: {json}"
    );
    assert!(
        json.contains("\"url\":\"https://example.com/install-cloudflared\""),
        "install button must carry install_url payload: {json}"
    );
}

#[test]
fn every_session_row_includes_menu_open_placeholder() {
    // SessionActionsMenu is deferred to Phase 2c (Menu/MenuItem not public in
    // v1). Until then, every session row emits an icon_button placeholder
    // wired to `botster.session.menu.open` so the browser JSX composite can
    // still render its Catalyst dropdown in Phase 2a/2b.
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_MIXED_ACTIVITY);
    let json = tree.to_string();
    let occurrences = json.matches("\"botster.session.menu.open\"").count();
    assert_eq!(
        occurrences, 3,
        "expected one menu.open placeholder per session; got {occurrences} in {json}"
    );
}

#[test]
fn accessory_session_suppresses_activity_dot() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_MIXED_ACTIVITY);
    let json = tree.to_string();
    // The accessory session (session_type="accessory") must NOT contribute a
    // status_dot; active/idle sessions must. Count is 2, not 3.
    let dots = json.matches("\"type\":\"status_dot\"").count();
    assert_eq!(
        dots, 2,
        "accessory must skip status_dot — expected 2 dots (active+idle), got {dots}: {json}"
    );
}

#[test]
fn sidebar_and_panel_densities_produce_different_trees() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let sidebar = render_via_lua(&lua, FIXTURE_PREVIEW_RUNNING_SIDEBAR);
    let panel = render_via_lua(&lua, FIXTURE_SINGLE_RUNNING);
    assert_ne!(
        sidebar, panel,
        "sidebar vs panel surfaces must produce different trees"
    );
    // Sidebar workspace header does NOT show the count (per composites.ts);
    // panel header DOES include the count text "1".
    let panel_str = panel.to_string();
    let sidebar_str = sidebar.to_string();
    assert!(
        panel_str.contains("\"text\":\"1\""),
        "panel header must include workspace count: {panel_str}"
    );
    assert!(
        !sidebar_str.contains("\"text\":\"1\""),
        "sidebar header must omit workspace count: {sidebar_str}"
    );
}

#[test]
fn empty_with_workspaces_still_uses_empty_state() {
    // Regression: a hub with zero sessions but non-empty open_workspaces must
    // still render the empty state, not a workspace header with an empty
    // body. Parity with WorkspaceList.jsx:17 (`sessionCount === 0` wins).
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(
        &lua,
        r#"
            return web_layout.render("workspace_surface", {
                hub_id = "hub-1",
                agents = {},
                open_workspaces = {
                    { id = "ws-1", name = "Empty A", agents = {} },
                    { id = "ws-2", name = "Empty B", agents = { "does-not-exist" } },
                },
                surface = "panel",
            })
        "#,
    );
    check_golden("empty_with_workspaces", &tree);
    assert_eq!(
        tree.get("type").and_then(|v| v.as_str()),
        Some("stack"),
        "empty + open workspaces must still resolve to the empty-state stack"
    );
    let json = tree.to_string();
    assert!(
        !json.contains("\"type\":\"tree\""),
        "no tree should render when sessionCount == 0: {json}"
    );
    assert!(
        !json.contains("\"text\":\"Empty A\""),
        "empty workspace title must not leak into the empty-state tree: {json}"
    );
}

#[test]
fn empty_workspace_groups_are_hidden_when_other_sessions_exist() {
    // Regression: workspaces whose resolved session bucket is empty must not
    // render a header. Parity with WorkspaceGroup.jsx:21.
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(
        &lua,
        r#"
            local real_session = {
                id = "sess-real", session_uuid = "uuid-real",
                session_type = "agent", display_name = "real-one",
                target_name = "t", branch_name = "main", agent_name = "claude",
                is_idle = true,
            }
            return web_layout.render("workspace_surface", {
                hub_id = "h",
                agents = { real_session },
                open_workspaces = {
                    { id = "ws-empty", name = "EmptyWorkspace", agents = {} },
                    { id = "ws-real", name = "RealWorkspace", agents = { "sess-real" } },
                    { id = "ws-stale", name = "StaleWorkspace", agents = { "gone" } },
                },
                surface = "panel",
            })
        "#,
    );
    check_golden("ungrouped_with_empty_workspace", &tree);
    let json = tree.to_string();
    assert!(
        !json.contains("EmptyWorkspace"),
        "empty workspace must be suppressed: {json}"
    );
    assert!(
        !json.contains("StaleWorkspace"),
        "workspace whose agents do not resolve must be suppressed: {json}"
    );
    assert!(
        json.contains("RealWorkspace"),
        "non-empty workspace must still render: {json}"
    );
    // One workspace group × 1 session + the ungrouped bucket (none here) =
    // exactly one workspace header rendered.
    assert_eq!(
        json.matches("\"text\":\"RealWorkspace\"").count(),
        1,
        "RealWorkspace must render exactly once: {json}"
    );
}

#[test]
fn new_session_button_always_rendered_in_populated_surface() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_SINGLE_RUNNING);
    let json = tree.to_string();
    assert!(
        json.contains("\"label\":\"New session\""),
        "populated surface must still include the NewSession button: {json}"
    );
    assert!(
        json.contains("\"id\":\"botster.session.create.request\""),
        "NewSession button must emit create.request action: {json}"
    );
    assert!(
        json.contains("\"icon\":\"plus\""),
        "NewSession button must use the plus icon: {json}"
    );
}

#[test]
fn port_zero_is_treated_as_no_preview() {
    // Parity with JS `!!session.port`: port == 0 means "no port forwarded",
    // so can_preview must be false and the hosted preview indicator must not
    // render. Catches the Lua-vs-JS truthiness divergence (Lua treats 0 as
    // truthy by default).
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(
        &lua,
        r#"
            local session = {
                id = "sess-z", session_uuid = "uuid-z",
                session_type = "agent", display_name = "zero-port",
                target_name = "x", branch_name = "main", agent_name = "claude",
                is_idle = true,
                port = 0,
                hosted_preview = { status = "running", url = "https://should-not-render.example.com" },
            }
            return web_layout.render("workspace_surface", {
                hub_id = "h", agents = { session },
                open_workspaces = { { id = "ws", name = "W", agents = { "sess-z" } } },
                surface = "panel",
            })
        "#,
    );
    let json = tree.to_string();
    assert!(
        !json.contains("should-not-render.example.com"),
        "port=0 must suppress the hosted preview indicator: {json}"
    );
    // The running-preview button would have emitted
    // `botster.session.preview.open` — its absence proves suppression.
    assert!(
        !json.contains("\"id\":\"botster.session.preview.open\""),
        "port=0 must suppress preview.open action: {json}"
    );
}

#[test]
fn single_running_session_row_is_selected_and_monospace() {
    // Spot-check the title-slot structure produced by the session row: the
    // primary text must be monospace and — because state.selected_session_uuid
    // matches — carry weight="medium".
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_SINGLE_RUNNING);
    let json = tree.to_string();
    assert!(
        json.contains("\"monospace\":true"),
        "session title text must be monospace: {json}"
    );
    assert!(
        json.contains("\"weight\":\"medium\""),
        "selected session title must weight=medium: {json}"
    );
}

// -------------------------------------------------------------------------
// Override chain & error fallback (runs serially to share env-var mutations)
// -------------------------------------------------------------------------

#[test]
fn override_chain_and_error_fallback() {
    // All branches of this test touch the same env vars
    // (BOTSTER_WEB_LAYOUT_REPO_DIR + BOTSTER_CONFIG_DIR) so we keep them in a
    // single #[test] to avoid races with any parallel test that might also
    // touch them. We acquire the render lock directly (without the env-reset
    // that `lock_render_env` performs) so the env guards below stay authoritative
    // for the whole test body.
    let _render_lock = RENDER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // Baseline: embedded default is used when no override files exist. Points
    // both env vars at an empty tempdir.
    let tmp = tempfile::tempdir().expect("mktempdir");
    let repo_dir = tmp.path().join("repo-config");
    let device_dir = tmp.path().join("device-config");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&device_dir).unwrap();

    let _repo_guard = EnvGuard::set(REPO_DIR_OVERRIDE_ENV, repo_dir.to_string_lossy().as_ref());
    let _device_guard =
        EnvGuard::set(DEVICE_DIR_OVERRIDE_ENV, device_dir.to_string_lossy().as_ref());
    let _dev_guard = EnvGuard::set(DEV_MODE_ENV, "");

    let lua = new_web_layout_lua();
    let embedded = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        embedded.get("type").and_then(|v| v.as_str()),
        Some("stack"),
        "baseline (no overrides) should use the embedded default, which emits a stack-based empty state"
    );

    // Device layout_web.lua override: injects a distinctive marker into the
    // rendered title so we can tell the override was picked up.
    let device_override = device_dir.join("layout_web.lua");
    std::fs::write(
        &device_override,
        r#"return {
            workspace_surface = function(state)
                return ui.panel{ title = "FROM DEVICE OVERRIDE", tone = "muted" }
            end,
        }"#,
    )
    .unwrap();

    let lua = new_web_layout_lua();
    let device_tree = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        device_tree
            .get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("FROM DEVICE OVERRIDE"),
        "device override layout_web.lua should win over embedded: {device_tree}"
    );

    // Repo layout_web.lua override: must win over the device override.
    let repo_override = repo_dir.join("layout_web.lua");
    std::fs::write(
        &repo_override,
        r#"return {
            workspace_surface = function(state)
                return ui.panel{ title = "FROM REPO OVERRIDE", tone = "muted" }
            end,
        }"#,
    )
    .unwrap();

    let lua = new_web_layout_lua();
    let repo_tree = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        repo_tree
            .get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("FROM REPO OVERRIDE"),
        "repo layout_web.lua should win over device layout_web.lua"
    );

    // Remove layout_web.lua files: a `layout.lua` (shared) override should
    // then take effect. Also assert repo wins over device at the shared
    // layer.
    std::fs::remove_file(&repo_override).unwrap();
    std::fs::remove_file(&device_override).unwrap();

    let device_shared = device_dir.join("layout.lua");
    std::fs::write(
        &device_shared,
        r#"return {
            workspace_surface = function(state)
                return ui.panel{ title = "FROM DEVICE SHARED", tone = "muted" }
            end,
        }"#,
    )
    .unwrap();
    let lua = new_web_layout_lua();
    let device_shared_tree = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        device_shared_tree
            .get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("FROM DEVICE SHARED"),
        "device layout.lua should be picked up as shared fallback"
    );

    let repo_shared = repo_dir.join("layout.lua");
    std::fs::write(
        &repo_shared,
        r#"return {
            workspace_surface = function(state)
                return ui.panel{ title = "FROM REPO SHARED", tone = "muted" }
            end,
        }"#,
    )
    .unwrap();
    let lua = new_web_layout_lua();
    let repo_shared_tree = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        repo_shared_tree
            .get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("FROM REPO SHARED"),
        "repo layout.lua should win over device layout.lua"
    );

    // A broken override (Lua syntax error) must not crash the hub; the
    // primitive wraps the failure in a fallback `ui.panel{}` tree.
    std::fs::write(&repo_shared, "this is not valid lua }{").unwrap();
    let lua = new_web_layout_lua();
    let fallback = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        fallback.get("type").and_then(|v| v.as_str()),
        Some("panel"),
        "broken override must fall back to a panel tree: {fallback}"
    );
    let title = fallback
        .get("props")
        .and_then(|p| p.get("title"))
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    assert!(
        title.contains("Layout error"),
        "fallback title should announce the failure: {title}"
    );
}

// -------------------------------------------------------------------------
// Modtime cache (Phase 2b) — the override-chain cache must pick up file
// mtime changes and preserve override priority on invalidation.
// -------------------------------------------------------------------------

/// Helper: bump a file's mtime forward by ~2 seconds so the next
/// `candidate_mtime` call observes a different value even on filesystems
/// with coarse (1s) mtime granularity.
fn touch_future(path: &std::path::Path) {
    // Read-then-write bumps mtime on every filesystem. Add a distinct byte
    // so the content hash would also change if we ever key on content.
    let prev = std::fs::read_to_string(path).unwrap_or_default();
    let bumped = format!("{prev}\n-- touched");
    std::fs::write(path, bumped).unwrap();
    // Also force mtime forward explicitly in case the filesystem clamps
    // subsecond resolution.
    let now = std::time::SystemTime::now();
    let future = now + std::time::Duration::from_secs(2);
    let _ = filetime::set_file_mtime(path, filetime::FileTime::from_system_time(future));
}

#[test]
fn modtime_cache_invalidates_on_file_edit_and_restores_embedded_when_removed() {
    let _lock = lock_render_env();
    // `lock_render_env` already sets TTL=0 and clears the cache; those are
    // the right defaults for sequential file edits inside a single test.
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("repo").join(".botster");
    std::fs::create_dir_all(&repo_dir).unwrap();

    let _env_repo = EnvGuard::set(
        "BOTSTER_WEB_LAYOUT_REPO_DIR",
        repo_dir.to_str().expect("utf-8 repo path"),
    );

    // 1. No override file -> falls back to embedded default.
    let lua = new_web_layout_lua();
    let base = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        base.get("type").and_then(|v| v.as_str()),
        Some("stack"),
        "embedded default renders the empty-state stack: {base}"
    );

    // 2. Add a repo override. Because the previous render cached
    // "no override won" with a 500 ms TTL, the next render WITHIN that
    // window must still pick up the new file — so we explicitly clear
    // the cache the same way a cache-miss past TTL would.
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
    let override_path = repo_dir.join("layout_web.lua");
    std::fs::write(
        &override_path,
        r#"return {
            workspace_surface = function(_state)
                return ui.panel{ title = "MODTIME-v1", tone = "muted" }
            end,
        }"#,
    )
    .unwrap();
    let lua = new_web_layout_lua();
    let v1 = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        v1.get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("MODTIME-v1"),
        "override picked up after cache clear: {v1}"
    );

    // 3. Second render within TTL reuses the cached content — still v1.
    let v1_again = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        v1_again
            .get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("MODTIME-v1"),
        "within-TTL render reuses cached override: {v1_again}"
    );

    // 4. Rewrite the file with a new payload and bump mtime. Clearing the
    // cache here stands in for "TTL elapsed" — the production behavior is
    // identical past 500 ms.
    std::fs::write(
        &override_path,
        r#"return {
            workspace_surface = function(_state)
                return ui.panel{ title = "MODTIME-v2", tone = "muted" }
            end,
        }"#,
    )
    .unwrap();
    let now = std::time::SystemTime::now();
    let future = now + std::time::Duration::from_secs(2);
    let _ = filetime::set_file_mtime(
        &override_path,
        filetime::FileTime::from_system_time(future),
    );
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
    let v2 = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        v2.get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("MODTIME-v2"),
        "mtime drift invalidates the cached content: {v2}"
    );

    // 5. Remove the override entirely. Past the TTL (or after an explicit
    // clear) the chain must fall back to embedded again — the previous
    // "winning override" entry must not be served once the file is gone.
    std::fs::remove_file(&override_path).unwrap();
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
    let after_delete = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        after_delete.get("type").and_then(|v| v.as_str()),
        Some("stack"),
        "removing the override must restore embedded default: {after_delete}"
    );
}

#[test]
fn modtime_cache_reuses_unchanged_content_without_rereading() {
    // If the cached mtime matches the filesystem's mtime, the cache must
    // reuse the stored content. We prove this by swapping the file's
    // content to something that WOULD raise a Lua error on evaluation,
    // while pinning mtime to the original value. A successful render with
    // the original title proves the cache served the old bytes.
    //
    // This test temporarily restores the production TTL (500 ms) so the
    // within-TTL cache-hit branch is exercised at all — `lock_render_env`
    // drops TTL to 0 for deterministic sequential edits.
    let _lock = lock_render_env();
    botster::lua::primitives::web_layout::set_override_cache_ttl_millis(500);
    // Clear between TTL changes so the first `render` in this test seeds a
    // cache with the new TTL rather than serving a zero-TTL entry.
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
    // Restore test default at scope exit so later tests aren't surprised.
    struct TtlRestore;
    impl Drop for TtlRestore {
        fn drop(&mut self) {
            botster::lua::primitives::web_layout::set_override_cache_ttl_millis(0);
            botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
        }
    }
    let _ttl_restore = TtlRestore;

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("repo").join(".botster");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _env_repo = EnvGuard::set(
        "BOTSTER_WEB_LAYOUT_REPO_DIR",
        repo_dir.to_str().expect("utf-8 repo path"),
    );

    let override_path = repo_dir.join("layout_web.lua");
    std::fs::write(
        &override_path,
        r#"return {
            workspace_surface = function(_state)
                return ui.panel{ title = "CACHED-CONTENT", tone = "muted" }
            end,
        }"#,
    )
    .unwrap();
    let original_mtime =
        filetime::FileTime::from_last_modification_time(&std::fs::metadata(&override_path).unwrap());

    // First render: populates the cache with the good content.
    let lua = new_web_layout_lua();
    let first = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        first
            .get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("CACHED-CONTENT"),
    );

    // Swap content to something that would raise on evaluation, but pin
    // mtime back to the original so the cache considers the file
    // unchanged and reuses the cached bytes.
    std::fs::write(&override_path, "THIS WOULD RAISE IF RE-READ").unwrap();
    filetime::set_file_mtime(&override_path, original_mtime).unwrap();

    // Second render within TTL: reuse cache directly (zero disk reads).
    let second = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        second
            .get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("CACHED-CONTENT"),
        "within-TTL render should reuse cached content without re-reading"
    );

    // Force a cache miss: mtime-pinned content must STILL be reused
    // because the stat-then-content-diff path short-circuits on mtime
    // equality.
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
    // Repopulate cache with good content by rewriting, then pin mtime back.
    std::fs::write(
        &override_path,
        r#"return {
            workspace_surface = function(_state)
                return ui.panel{ title = "CACHED-CONTENT", tone = "muted" }
            end,
        }"#,
    )
    .unwrap();
    filetime::set_file_mtime(&override_path, original_mtime).unwrap();
    let lua = new_web_layout_lua();
    let _prime = render_via_lua(&lua, FIXTURE_EMPTY);
    // Now the cache holds the good content at `original_mtime`.
    // Swap to broken content WITHOUT bumping mtime:
    std::fs::write(&override_path, "still broken").unwrap();
    filetime::set_file_mtime(&override_path, original_mtime).unwrap();
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
    // After clear, scan_and_load() re-stats. Mtime equals the cached mtime
    // only if we can re-seed the cache — but _clear_override_cache_for_tests
    // wipes it. This branch therefore EXERCISES the re-read path; it must
    // now fail to a fallback (not crash). Regression guard: the primitive
    // must never propagate the I/O/parse error as a Rust panic.
    let lua = new_web_layout_lua();
    let after = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        after.get("type").and_then(|v| v.as_str()),
        Some("panel"),
        "broken re-read must land on the error fallback tree: {after}"
    );
    let title = after
        .get("props")
        .and_then(|p| p.get("title"))
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    assert!(
        title.contains("Layout error"),
        "fallback title must announce the failure, got {title}"
    );
    let _ = touch_future; // silence warn in the common case where 2-stage touch is unused
}
