//! Rust-hosted Lua tests for workspace_store.lua dedup_key functionality.
//!
//! Creates a minimal mlua::Lua VM with fs, json, log primitives and a temp dir,
//! then loads workspace_store.lua and hub/hooks.lua to test:
//!
//! - find_workspace(data_dir, dedup_key) — match, no match, empty dir
//! - ensure_workspace() — creates new, finds existing, missing dedup_key
//! - migrate_v2() — converts old manifests, idempotent re-run, branch fallback
//! - build_workspace_groups() — uses dedup_key from manifest
//! - hooks.call("build_dedup_key") with and without interceptor

use mlua::Lua;
use tempfile::TempDir;

/// Create a Lua VM with fs, json, log primitives and package.path pointing
/// to cli/lua/ so require("lib.workspace_store") and require("hub.hooks") work.
///
/// Also sets up a minimal `hooks` global (loads hub/hooks.lua) since
/// workspace_store migration functions call hooks.call().
fn create_lua_vm(data_dir: &std::path::Path) -> Lua {
    let lua = Lua::new();

    // Register core primitives needed by workspace_store
    botster::lua::primitives::fs::register(&lua).expect("fs register");
    botster::lua::primitives::json::register(&lua).expect("json register");
    botster::lua::primitives::log::register(&lua).expect("log register");

    // Set up package.path to find our Lua modules
    let lua_dir = std::env::current_dir()
        .unwrap()
        .join("lua")
        .to_str()
        .unwrap()
        .to_string();
    lua.load(format!(
        r#"package.path = "{lua_dir}/?.lua;{lua_dir}/?/init.lua;" .. package.path"#
    ))
    .exec()
    .expect("set package.path");

    // Load hooks module and make it a global (same as hub/init.lua does)
    lua.load(r#"
        _G.hooks = require("hub.hooks")
    "#)
    .exec()
    .expect("load hooks module");

    // Stub out `worktree` global (migrate() calls worktree.list())
    lua.load(r#"
        _G.worktree = { list = function() return {} end }
    "#)
    .exec()
    .expect("stub worktree");

    lua
}

/// Helper: Load workspace_store module in the VM.
fn load_workspace_store(lua: &Lua) {
    lua.load(r#"ws = require("lib.workspace_store")"#)
        .exec()
        .expect("load workspace_store");
}

// =============================================================================
// Tier 1: find_workspace tests
// =============================================================================

#[test]
fn test_find_workspace_empty_dir() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (id, _manifest): (Option<String>, Option<mlua::Value>) = lua
        .load(format!(
            r#"
            ws.init_dir("{dd}")
            return ws.find_workspace("{dd}", "test:key#1")
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("find_workspace should be callable");

    assert!(id.is_none(), "Should return nil for empty workspaces dir");
}

#[test]
fn test_find_workspace_match() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let found: bool = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            -- Create a workspace with a known dedup_key
            ws.ensure_workspace(dd, {{
                dedup_key = "github:owner/repo#42",
                title = "Test workspace",
                metadata = {{ repo = "owner/repo", issue_number = 42 }},
            }})
            -- Find it
            local id, manifest = ws.find_workspace(dd, "github:owner/repo#42")
            return id ~= nil and manifest ~= nil and manifest.dedup_key == "github:owner/repo#42"
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("find_workspace match test");

    assert!(found, "Should find workspace by dedup_key");
}

#[test]
fn test_find_workspace_no_match() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (id, _): (Option<String>, Option<mlua::Value>) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            ws.ensure_workspace(dd, {{
                dedup_key = "github:owner/repo#42",
                title = "Test workspace",
            }})
            return ws.find_workspace(dd, "github:owner/repo#99")
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("find_workspace no match test");

    assert!(id.is_none(), "Should return nil for non-matching dedup_key");
}

#[test]
fn test_find_workspace_nil_key() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (id, _): (Option<String>, Option<mlua::Value>) = lua
        .load(format!(
            r#"
            ws.init_dir("{dd}")
            return ws.find_workspace("{dd}", nil)
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("find_workspace nil key");

    assert!(id.is_none(), "Should return nil for nil dedup_key");
}

// =============================================================================
// Tier 1: ensure_workspace tests
// =============================================================================

#[test]
fn test_ensure_workspace_creates_new() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (id, created): (Option<String>, bool) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local id, manifest, created = ws.ensure_workspace(dd, {{
                dedup_key = "test:my-key",
                title = "My Workspace",
                metadata = {{ custom = "data" }},
            }})
            return id, created
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("ensure_workspace creates new");

    assert!(id.is_some(), "Should return a workspace_id");
    assert!(created, "Should indicate creation");
}

#[test]
fn test_ensure_workspace_finds_existing() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (same_id, not_created): (bool, bool) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local id1, _, created1 = ws.ensure_workspace(dd, {{
                dedup_key = "test:dedup",
                title = "First",
            }})
            local id2, _, created2 = ws.ensure_workspace(dd, {{
                dedup_key = "test:dedup",
                title = "Second (ignored)",
            }})
            return id1 == id2, not created2
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("ensure_workspace finds existing");

    assert!(same_id, "Should return same workspace_id for same dedup_key");
    assert!(not_created, "Should not create new for existing dedup_key");
}

#[test]
fn test_ensure_workspace_missing_dedup_key() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (id, _): (Option<String>, Option<mlua::Value>) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local id, manifest, created = ws.ensure_workspace(dd, {{
                title = "No Key",
            }})
            return id, manifest
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("ensure_workspace missing dedup_key");

    assert!(id.is_none(), "Should return nil when dedup_key is missing");
}

#[test]
fn test_ensure_workspace_manifest_fields() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (has_fields, meta_ok): (bool, bool) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local id, manifest = ws.ensure_workspace(dd, {{
                dedup_key = "test:fields",
                title = "Field Test",
                metadata = {{ repo = "owner/repo", issue_number = 42 }},
            }})
            local ok = manifest.id ~= nil
                and manifest.title == "Field Test"
                and manifest.dedup_key == "test:fields"
                and manifest.status == "active"
                and manifest.created_at ~= nil
                and manifest.updated_at ~= nil
            local meta_ok = manifest.metadata ~= nil
                and manifest.metadata.repo == "owner/repo"
                and manifest.metadata.issue_number == 42
            return ok, meta_ok
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("ensure_workspace manifest fields");

    assert!(has_fields, "Manifest should have all expected top-level fields");
    assert!(meta_ok, "Manifest metadata should preserve plugin data");
}

// =============================================================================
// Tier 1: migrate_v2 tests
// =============================================================================

#[test]
fn test_migrate_v2_converts_issue_manifest() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    // No interceptor registered — should use generic fallback
    let (dedup_key, has_metadata): (String, bool) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            -- Write a v1 manifest (has repo but no dedup_key)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                title = "owner/repo — issue #42",
                repo = "owner/repo",
                issue_number = 42,
                status = "active",
                created_at = "2026-01-01T00:00:00Z",
            }})
            -- Run migration
            ws.migrate_v2(dd)
            -- Read back
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.dedup_key,
                   manifest.metadata ~= nil and manifest.metadata.repo == "owner/repo"
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 issue manifest");

    // Generic fallback format (no "github:" prefix — no interceptor registered)
    assert_eq!(dedup_key, "owner/repo#42");
    assert!(has_metadata, "Should populate metadata from legacy fields");
}

#[test]
fn test_migrate_v2_converts_branch_manifest() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let dedup_key: String = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                title = "owner/repo — feature-branch",
                repo = "owner/repo",
                ad_hoc_key = "feature-branch",
                status = "active",
            }})
            ws.migrate_v2(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.dedup_key
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 branch manifest");

    assert_eq!(dedup_key, "owner/repo:feature-branch");
}

#[test]
fn test_migrate_v2_branch_fallback_uses_manifest_branch() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    // v1 manifest with `branch` field but no `ad_hoc_key`
    let dedup_key: String = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                title = "owner/repo — my-branch",
                repo = "owner/repo",
                branch = "my-branch",
                status = "active",
            }})
            ws.migrate_v2(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.dedup_key
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 branch fallback");

    // Should use manifest.branch, not "main"
    assert_eq!(dedup_key, "owner/repo:my-branch");
}

#[test]
fn test_migrate_v2_fallback_to_main() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    // v1 manifest with neither ad_hoc_key nor branch
    let dedup_key: String = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                title = "owner/repo — main",
                repo = "owner/repo",
                status = "active",
            }})
            ws.migrate_v2(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.dedup_key
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 fallback to main");

    assert_eq!(dedup_key, "owner/repo:main");
}

#[test]
fn test_migrate_v2_idempotent() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (key1, key2, same): (String, String, bool) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                title = "Test",
                repo = "owner/repo",
                issue_number = 7,
                status = "active",
            }})
            -- Run twice
            ws.migrate_v2(dd)
            local m1 = ws.read_workspace(dd, ws_id)
            ws.migrate_v2(dd)
            local m2 = ws.read_workspace(dd, ws_id)
            return m1.dedup_key, m2.dedup_key, m1.dedup_key == m2.dedup_key
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 idempotent");

    assert_eq!(key1, "owner/repo#7");
    assert_eq!(key2, "owner/repo#7");
    assert!(same, "Running migrate_v2 twice should produce same result");
}

#[test]
fn test_migrate_v2_skips_already_migrated() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    // Workspace that already has dedup_key should be untouched
    let unchanged: bool = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                title = "Already migrated",
                dedup_key = "custom:my-key",
                repo = "owner/repo",
                status = "active",
                metadata = {{ custom = true }},
            }})
            ws.migrate_v2(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.dedup_key == "custom:my-key"
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 skips already migrated");

    assert!(
        unchanged,
        "Manifests with dedup_key should not be modified"
    );
}

// =============================================================================
// Tier 1: build_workspace_groups tests
// =============================================================================

#[test]
fn test_build_workspace_groups_uses_dedup_key() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (has_dedup, has_metadata): (bool, bool) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                title = "Test Workspace",
                dedup_key = "github:owner/repo#42",
                status = "active",
                metadata = {{ repo = "owner/repo", issue_number = 42 }},
            }})
            local agents = {{{{
                id = "test-agent",
                workspace_id = ws_id,
                repo = "owner/repo",
                branch_name = "botster-issue-42",
                dedup_key = "github:owner/repo#42",
            }}}}
            local groups = ws.build_workspace_groups(dd, agents)
            local g = groups[1]
            return g.dedup_key == "github:owner/repo#42",
                   g.metadata ~= nil and g.metadata.repo == "owner/repo"
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("build_workspace_groups uses dedup_key");

    assert!(has_dedup, "Workspace group should include dedup_key");
    assert!(has_metadata, "Workspace group should include metadata");
}

// =============================================================================
// Tier 2: hooks.call("build_dedup_key") interceptor tests
// =============================================================================

#[test]
fn test_build_dedup_key_hook_no_interceptor() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());

    // With no interceptor, hooks.call returns original context unchanged
    let (has_dedup, has_repo): (bool, bool) = lua
        .load(
            r#"
            local result = hooks.call("build_dedup_key", {
                repo = "owner/repo",
                issue_number = 42,
                branch_name = "main",
            })
            -- Should return original context (no dedup_key field)
            return result.dedup_key ~= nil, result.repo == "owner/repo"
            "#,
        )
        .eval()
        .expect("hook no interceptor");

    assert!(
        !has_dedup,
        "Without interceptor, result should not have dedup_key"
    );
    assert!(has_repo, "Without interceptor, original context passes through");
}

#[test]
fn test_build_dedup_key_hook_with_interceptor() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());

    // Register an interceptor that builds "github:" keys (simulates github.lua plugin)
    lua.load(
        r##"
        hooks.intercept("build_dedup_key", "test_github", function(context)
            if not context or not context.repo then return context end
            local dk
            if context.issue_number then
                dk = "github:" .. context.repo .. "#" .. tostring(context.issue_number)
            else
                dk = "github:" .. context.repo .. ":" .. (context.branch_name or "main")
            end
            return {
                dedup_key = dk,
                title = context.repo .. " - test",
                metadata = { repo = context.repo, issue_number = context.issue_number },
            }
        end)
        "##,
    )
    .exec()
    .expect("register interceptor");

    let (dedup_key, title): (String, String) = lua
        .load(
            r##"
            local result = hooks.call("build_dedup_key", {
                repo = "owner/repo",
                issue_number = 42,
            })
            return result.dedup_key, result.title
            "##,
        )
        .eval()
        .expect("hook with interceptor");

    assert_eq!(dedup_key, "github:owner/repo#42");
    assert_eq!(title, "owner/repo - test");
}

#[test]
fn test_build_dedup_key_hook_branch_mode() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());

    lua.load(
        r##"
        hooks.intercept("build_dedup_key", "test_github", function(context)
            if not context or not context.repo then return context end
            local dk
            if context.issue_number then
                dk = "github:" .. context.repo .. "#" .. tostring(context.issue_number)
            else
                dk = "github:" .. context.repo .. ":" .. (context.branch_name or "main")
            end
            return {
                dedup_key = dk,
                title = context.repo,
                metadata = { repo = context.repo },
            }
        end)
        "##,
    )
    .exec()
    .expect("register interceptor");

    let dedup_key: String = lua
        .load(
            r##"
            local result = hooks.call("build_dedup_key", {
                repo = "owner/repo",
                branch_name = "feature-x",
            })
            return result.dedup_key
            "##,
        )
        .eval()
        .expect("hook branch mode");

    assert_eq!(dedup_key, "github:owner/repo:feature-x");
}

#[test]
fn test_migrate_v2_with_interceptor() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    // Register interceptor BEFORE migration (simulates plugin load order)
    lua.load(
        r##"
        hooks.intercept("build_dedup_key", "test_github", function(context)
            if not context or not context.repo then return context end
            local dk
            if context.issue_number then
                dk = "github:" .. context.repo .. "#" .. tostring(context.issue_number)
            else
                dk = "github:" .. context.repo .. ":" .. (context.branch_name or "main")
            end
            return {
                dedup_key = dk,
                title = context.repo .. " - intercepted",
                metadata = { repo = context.repo, issue_number = context.issue_number },
            }
        end)
        "##,
    )
    .exec()
    .expect("register interceptor for migrate_v2");

    let (dedup_key, title): (String, String) = lua
        .load(format!(
            r##"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                title = "owner/repo - issue #42",
                repo = "owner/repo",
                issue_number = 42,
                status = "active",
            }})
            ws.migrate_v2(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.dedup_key, manifest.title
            "##,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 with interceptor");

    // With interceptor, should get "github:" prefix
    assert_eq!(dedup_key, "github:owner/repo#42");
    // Title should be preserved from original manifest (not overwritten by interceptor)
    assert_eq!(title, "owner/repo - issue #42");
}
