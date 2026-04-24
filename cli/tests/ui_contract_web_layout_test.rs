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
/// Also clears the Rust-side override cache so one test's cached winner
/// doesn't leak into the next. In production the cache is only cleared by
/// an explicit `web_layout.reload()` call; tests simulate that from the
/// Rust side via `_clear_override_cache_for_tests`.
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
#[test]
fn single_running_session_matches_golden() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_SINGLE_RUNNING);
    check_golden("single_running", &tree);
}

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
#[test]
fn mixed_activity_matches_golden() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_MIXED_ACTIVITY);
    check_golden("mixed_activity", &tree);
}

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
#[test]
fn preview_error_matches_golden() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_PREVIEW_ERROR);
    check_golden("preview_error", &tree);
}

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
#[test]
fn only_active_sessions_render_activity_dot() {
    let _lock = lock_render_env();
    let lua = new_web_layout_lua();
    let tree = render_via_lua(&lua, FIXTURE_MIXED_ACTIVITY);
    let json = tree.to_string();
    // Only active sessions contribute a status_dot. Accessory and idle
    // sessions are quiet — no dot at all. Mixed-activity fixture has one
    // active session, so exactly one dot.
    let dots = json.matches("\"type\":\"status_dot\"").count();
    assert_eq!(
        dots, 1,
        "only active sessions render status_dot — expected 1 dot (active only), got {dots}: {json}"
    );
}

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
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

    // Tests explicitly invalidate the Rust-side override cache between
    // filesystem mutations. In production the user would call
    // `web_layout.reload()`; from Rust tests we hit the same invalidation
    // via `_clear_override_cache_for_tests` to avoid needing a shared Lua
    // VM across the test steps.
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
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

    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
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

    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
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
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
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
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
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
    botster::lua::primitives::web_layout::_clear_override_cache_for_tests();
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

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
#[test]
fn cache_invalidates_on_reload_and_restores_embedded_when_removed() {
    // New semantics (plugin-reload-parity): the override cache is held
    // indefinitely and only cleared by an explicit reload. File edits are
    // NOT auto-detected — the user invokes `web_layout.reload()` when they
    // want their changes picked up.
    let _lock = lock_render_env();

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

    // 2. Add a repo override. Because the previous render cached "no
    // override won", the new file is NOT picked up until we reload.
    let override_path = repo_dir.join("layout_web.lua");
    std::fs::write(
        &override_path,
        r#"return {
            workspace_surface = function(_state)
                return ui.panel{ title = "OVERRIDE-v1", tone = "muted" }
            end,
        }"#,
    )
    .unwrap();
    let pre_reload = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        pre_reload.get("type").and_then(|v| v.as_str()),
        Some("stack"),
        "without reload the cached 'no override' result still serves: {pre_reload}"
    );

    // 3. Explicit reload picks up the new file.
    botster::lua::primitives::web_layout::reload(&lua).expect("reload");
    let v1 = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        v1.get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("OVERRIDE-v1"),
        "override picked up after explicit reload: {v1}"
    );

    // 4. Subsequent renders without another reload reuse the cached v1.
    let v1_again = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        v1_again
            .get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("OVERRIDE-v1"),
        "steady-state render reuses cached override without re-reading: {v1_again}"
    );

    // 5. Rewrite the override on disk — still ignored until we reload.
    std::fs::write(
        &override_path,
        r#"return {
            workspace_surface = function(_state)
                return ui.panel{ title = "OVERRIDE-v2", tone = "muted" }
            end,
        }"#,
    )
    .unwrap();
    let still_v1 = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        still_v1
            .get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("OVERRIDE-v1"),
        "edit without reload must NOT take effect: {still_v1}"
    );

    // 6. Reload picks up the edit.
    botster::lua::primitives::web_layout::reload(&lua).expect("reload after edit");
    let v2 = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        v2.get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("OVERRIDE-v2"),
        "reload after edit picks up the new content: {v2}"
    );

    // 7. Delete the override, reload, and fall back to embedded.
    std::fs::remove_file(&override_path).unwrap();
    botster::lua::primitives::web_layout::reload(&lua).expect("reload after deletion");
    let after_delete = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        after_delete.get("type").and_then(|v| v.as_str()),
        Some("stack"),
        "reload after deletion restores embedded default: {after_delete}"
    );
}

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
#[test]
fn reload_is_callable_from_lua_and_clears_caches() {
    // `web_layout.reload()` is the Lua-visible entry point that matches the
    // `reload_plugin` pattern — users/callers explicitly opt in.
    let _lock = lock_render_env();

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("repo").join(".botster");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _env_repo = EnvGuard::set(
        "BOTSTER_WEB_LAYOUT_REPO_DIR",
        repo_dir.to_str().expect("utf-8 repo path"),
    );

    let lua = new_web_layout_lua();
    // Seed the cache with "no override wins".
    let _ = render_via_lua(&lua, FIXTURE_EMPTY);

    // Add an override AFTER the first render seeded the cache.
    let override_path = repo_dir.join("layout_web.lua");
    std::fs::write(
        &override_path,
        r#"return {
            workspace_surface = function(_state)
                return ui.panel{ title = "LUA-RELOAD", tone = "muted" }
            end,
        }"#,
    )
    .unwrap();

    // Call `web_layout.reload()` from Lua — same entrypoint users hit.
    lua.load("web_layout.reload()").exec().expect("lua reload");

    let after = render_via_lua(&lua, FIXTURE_EMPTY);
    assert_eq!(
        after
            .get("props")
            .and_then(|p| p.get("title"))
            .and_then(|t| t.as_str()),
        Some("LUA-RELOAD"),
        "lua-driven reload must pick up the new override: {after}"
    );
}

// -----------------------------------------------------------------------------
// Regression tests for the override-evaluation lifecycle.
//
// Phase 2a's loader re-evaluated the override chunk on every render (even on
// cache HIT — the string content was re-loaded into Lua each call) AND
// `require("web.layout")` inside the override returned the VM singleton from
// `package.loaded`. Overrides that monkey-patched `base.workspace_surface`
// therefore stacked wrapper layers on every re-evaluation, producing N
// banners after N renders.
//
// These tests lock in the post-fix behavior:
// 1. Override that mutates `base.workspace_surface` does NOT accumulate
//    wrappers across renders — exactly one banner shows regardless of how
//    many times render is called.
// 2. Override is evaluated at most once per content hash — a counter
//    incremented at eval time advances exactly once even over N renders.
// 3. Deleting the override restores the embedded default to an unpolluted
//    state (no residual wrappers leaking from the prior override's session).
// -----------------------------------------------------------------------------

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
#[test]
fn override_monkey_patching_does_not_stack_wrappers_across_renders() {
    let _lock = lock_render_env();

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("repo").join(".botster");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _env_repo = EnvGuard::set(
        "BOTSTER_WEB_LAYOUT_REPO_DIR",
        repo_dir.to_str().expect("utf-8 repo path"),
    );

    // Classic "wrap the embedded function" override shape — exactly what a
    // user writing `.botster/layout_web.lua` would naïvely try. Pre-fix this
    // pattern accumulated one wrapper per render; the test renders five times
    // and asserts exactly ONE banner exists in the final output.
    let override_path = repo_dir.join("layout_web.lua");
    std::fs::write(
        &override_path,
        r#"
            local base = require("web.layout")
            local original = base.workspace_surface
            function base.workspace_surface(state)
                local inner = original(state)
                return {
                    type = "stack",
                    props = { direction = "vertical", gap = "3" },
                    children = {
                        { type = "panel", props = { title = "WRAPPED", tone = "muted" } },
                        inner,
                    },
                }
            end
            return base
        "#,
    )
    .unwrap();

    let lua = new_web_layout_lua();
    let mut last: JsonValue = JsonValue::Null;
    for _ in 0..5 {
        last = render_via_lua(&lua, FIXTURE_EMPTY);
    }
    let json = last.to_string();
    let banner_count = json.matches("\"title\":\"WRAPPED\"").count();
    assert_eq!(
        banner_count, 1,
        "override that wraps base.workspace_surface must NOT stack wrappers \
         across renders; got {banner_count} banners in: {json}"
    );
}

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
#[test]
fn override_module_evaluated_at_most_once_per_content_hash() {
    let _lock = lock_render_env();

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("repo").join(".botster");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _env_repo = EnvGuard::set(
        "BOTSTER_WEB_LAYOUT_REPO_DIR",
        repo_dir.to_str().expect("utf-8 repo path"),
    );

    // The override increments a global counter on every module-evaluation.
    // If the loader evaluates the chunk once per render (pre-fix), the
    // counter will equal the render count. If the loader caches the
    // evaluated module by content hash (post-fix), the counter stays at 1
    // regardless of render count.
    let override_path = repo_dir.join("layout_web.lua");
    std::fs::write(
        &override_path,
        r#"
            _G._botster_override_eval_count = (_G._botster_override_eval_count or 0) + 1
            return {
                workspace_surface = function(_state)
                    return {
                        type = "panel",
                        props = { title = "COUNTER-TEST", tone = "muted" },
                    }
                end,
            }
        "#,
    )
    .unwrap();

    let lua = new_web_layout_lua();
    lua.globals().set("_botster_override_eval_count", 0i64).unwrap();

    for _ in 0..4 {
        let _ = render_via_lua(&lua, FIXTURE_EMPTY);
    }

    let count: i64 = lua.globals().get("_botster_override_eval_count").unwrap();
    assert_eq!(
        count, 1,
        "override module must be evaluated at most once per unchanged content, \
         got {count} evaluations across 4 renders"
    );
}

#[ignore = "v1 web layout tests — wire protocol v2 (commit 7) replaced workspace_surface with composites; updated in commit 9"]
#[test]
fn override_deletion_restores_unpolluted_embedded_module() {
    let _lock = lock_render_env();

    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("repo").join(".botster");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let _env_repo = EnvGuard::set(
        "BOTSTER_WEB_LAYOUT_REPO_DIR",
        repo_dir.to_str().expect("utf-8 repo path"),
    );

    // Phase 1: a mutating override that poisons `package.loaded["web.layout"]`.
    let override_path = repo_dir.join("layout_web.lua");
    std::fs::write(
        &override_path,
        r#"
            local base = require("web.layout")
            local original = base.workspace_surface
            function base.workspace_surface(state)
                local inner = original(state)
                return {
                    type = "stack",
                    props = { direction = "vertical", gap = "3" },
                    children = {
                        { type = "panel", props = { title = "POISON", tone = "muted" } },
                        inner,
                    },
                }
            end
            return base
        "#,
    )
    .unwrap();

    let lua = new_web_layout_lua();
    let poisoned = render_via_lua(&lua, FIXTURE_EMPTY);
    assert!(
        poisoned.to_string().contains("\"title\":\"POISON\""),
        "first render should see the poison banner as a sanity check: {poisoned}"
    );

    // Phase 2: delete the override. An explicit reload (matching the
    // plugin-reload pattern) invalidates ALL caches including
    // `package.loaded["web.layout"]`, so the next render falls back to the
    // embedded default from a fresh copy — zero residual wrapping from the
    // poisoned singleton.
    std::fs::remove_file(&override_path).unwrap();
    botster::lua::primitives::web_layout::reload(&lua).expect("reload after delete");

    let restored = render_via_lua(&lua, FIXTURE_EMPTY);
    let json = restored.to_string();
    assert!(
        !json.contains("\"title\":\"POISON\""),
        "after override deletion the embedded default must NOT carry residual \
         wrappers from the prior override: {json}"
    );
    // And the embedded empty-state shape is what we expect.
    assert_eq!(
        restored.get("type").and_then(|v| v.as_str()),
        Some("stack"),
        "deleted-override fallback must return the embedded empty-state stack: {restored}"
    );
}

