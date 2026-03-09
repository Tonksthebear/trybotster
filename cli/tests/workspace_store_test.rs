//! Rust-hosted Lua tests for workspace_store.lua name-based workspace functionality.
//!
//! Creates a minimal mlua::Lua VM with fs, json, log primitives and a temp dir,
//! then loads workspace_store.lua to test:
//!
//! - find_workspace(data_dir, name) — match, no match, empty dir
//! - ensure_workspace() — creates new, finds existing, missing name
//! - rename_workspace() — updates name
//! - workspace schema invariants — no workspace-level branch/worktree fields
//! - migrate_v2() — converts old manifests to name format
//! - migrate_v3() — converts dedup_key manifests to name, deletes local: workspaces
//! - build_workspace_groups() — uses name from manifest

use mlua::Lua;
use tempfile::TempDir;

/// Create a Lua VM with fs, json, log primitives and package.path pointing
/// to cli/lua/ so require("lib.workspace_store") works.
fn create_lua_vm(_data_dir: &std::path::Path) -> Lua {
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
    lua.load(
        r#"
        _G.hooks = require("hub.hooks")
    "#,
    )
    .exec()
    .expect("load hooks module");

    // Stub out `worktree` global (migrate() calls worktree.list())
    lua.load(
        r#"
        _G.worktree = { list = function() return {} end }
    "#,
    )
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
            return ws.find_workspace("{dd}", "owner/repo#1")
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
            -- Create a workspace with a known name
            ws.ensure_workspace(dd, {{
                name = "owner/repo#42",
                metadata = {{ repo = "owner/repo", issue_number = 42 }},
            }})
            -- Find it
            local id, manifest = ws.find_workspace(dd, "owner/repo#42")
            return id ~= nil and manifest ~= nil and manifest.name == "owner/repo#42"
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("find_workspace match test");

    assert!(found, "Should find workspace by name");
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
                name = "owner/repo#42",
            }})
            return ws.find_workspace(dd, "owner/repo#99")
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("find_workspace no match test");

    assert!(id.is_none(), "Should return nil for non-matching name");
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

    assert!(id.is_none(), "Should return nil for nil name");
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
                name = "owner/repo#42",
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
                name = "owner/repo#42",
            }})
            local id2, _, created2 = ws.ensure_workspace(dd, {{
                name = "owner/repo#42",
            }})
            return id1 == id2, not created2
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("ensure_workspace finds existing");

    assert!(same_id, "Should return same workspace_id for same name");
    assert!(not_created, "Should not create new for existing name");
}

#[test]
fn test_ensure_workspace_missing_name() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (id, _): (Option<String>, Option<mlua::Value>) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local id, manifest, created = ws.ensure_workspace(dd, {{
                metadata = {{ repo = "owner/repo" }},
            }})
            return id, manifest
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("ensure_workspace missing name");

    assert!(id.is_none(), "Should return nil when name is missing");
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
                name = "owner/repo#42",
                metadata = {{ repo = "owner/repo", issue_number = 42 }},
            }})
            local ok = manifest.id ~= nil
                and manifest.name == "owner/repo#42"
                and manifest.branch == nil
                and manifest.worktree_path == nil
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

    assert!(
        has_fields,
        "Manifest should have all expected top-level fields"
    );
    assert!(meta_ok, "Manifest metadata should preserve plugin data");
}

// =============================================================================
// Tier 1: rename_workspace tests
// =============================================================================

#[test]
fn test_rename_workspace() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (renamed, new_name): (bool, String) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id, _ = ws.ensure_workspace(dd, {{
                name = "owner/repo#42",
            }})
            local ok = ws.rename_workspace(dd, ws_id, "My Custom Name")
            local manifest = ws.read_workspace(dd, ws_id)
            return ok, manifest.name
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("rename_workspace");

    assert!(renamed, "rename should succeed");
    assert_eq!(new_name, "My Custom Name");
}

#[test]
fn test_list_workspaces_returns_sorted_manifests_with_status() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (ordered, statuses): (bool, bool) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)

            ws.write_workspace(dd, "ws-b", {{
                id = "ws-b",
                name = "Second",
                status = "active",
                created_at = "2026-01-02T00:00:00Z",
                updated_at = "2026-01-02T00:00:00Z",
                metadata = {{}},
            }})
            ws.write_workspace(dd, "ws-a", {{
                id = "ws-a",
                name = "First",
                status = "active",
                created_at = "2026-01-01T00:00:00Z",
                updated_at = "2026-01-01T00:00:00Z",
                metadata = {{}},
            }})

            ws.write_session(dd, "ws-a", "sess-1", {{ status = "active" }})
            ws.write_session(dd, "ws-b", "sess-2", {{ status = "closed" }})

            local list = ws.list_workspaces(dd)
            local ordered = #list == 2 and list[1].id == "ws-a" and list[2].id == "ws-b"
            local statuses = list[1].status == "active" and list[2].status == "closed"
            return ordered, statuses
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("list_workspaces returns sorted manifests");

    assert!(ordered, "Workspaces should be sorted by created_at then id");
    assert!(
        statuses,
        "Workspace statuses should be derived from session manifests"
    );
}

// =============================================================================
// Tier 1: workspace schema tests
// =============================================================================

#[test]
fn test_workspace_manifest_omits_worktree_fields() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (no_branch, no_worktree): (bool, bool) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id, _ = ws.ensure_workspace(dd, {{
                name = "owner/repo#42",
            }})
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.branch == nil, manifest.worktree_path == nil
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("workspace manifest schema");

    assert!(no_branch, "Workspace manifest should not include branch");
    assert!(
        no_worktree,
        "Workspace manifest should not include worktree_path"
    );
}

// =============================================================================
// Tier 1: migrate_v2 tests (now produces name instead of dedup_key)
// =============================================================================

#[test]
fn test_migrate_v2_converts_issue_manifest() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (name, has_metadata): (String, bool) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            -- Write a v1 manifest (has repo but no name)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                repo = "owner/repo",
                issue_number = 42,
                status = "active",
                created_at = "2026-01-01T00:00:00Z",
            }})
            -- Run migration
            ws.migrate_v2(dd)
            -- Read back
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.name,
                   manifest.metadata ~= nil and manifest.metadata.repo == "owner/repo"
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 issue manifest");

    assert_eq!(name, "owner/repo#42");
    assert!(has_metadata, "Should populate metadata from legacy fields");
}

#[test]
fn test_migrate_v2_converts_branch_manifest() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let name: String = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                repo = "owner/repo",
                ad_hoc_key = "feature-branch",
                status = "active",
            }})
            ws.migrate_v2(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.name
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 branch manifest");

    assert_eq!(name, "owner/repo:feature-branch");
}

#[test]
fn test_migrate_v2_branch_fallback_uses_manifest_branch() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    // v1 manifest with `branch` field but no `ad_hoc_key`
    let name: String = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                repo = "owner/repo",
                branch = "my-branch",
                status = "active",
            }})
            ws.migrate_v2(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.name
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 branch fallback");

    // Should use manifest.branch, not "main"
    assert_eq!(name, "owner/repo:my-branch");
}

#[test]
fn test_migrate_v2_fallback_to_main() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    // v1 manifest with neither ad_hoc_key nor branch
    let name: String = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                repo = "owner/repo",
                status = "active",
            }})
            ws.migrate_v2(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.name
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 fallback to main");

    assert_eq!(name, "owner/repo:main");
}

#[test]
fn test_migrate_v2_idempotent() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (name1, name2, same): (String, String, bool) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                repo = "owner/repo",
                issue_number = 7,
                status = "active",
            }})
            -- Run twice
            ws.migrate_v2(dd)
            local m1 = ws.read_workspace(dd, ws_id)
            ws.migrate_v2(dd)
            local m2 = ws.read_workspace(dd, ws_id)
            return m1.name, m2.name, m1.name == m2.name
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 idempotent");

    assert_eq!(name1, "owner/repo#7");
    assert_eq!(name2, "owner/repo#7");
    assert!(same, "Running migrate_v2 twice should produce same result");
}

#[test]
fn test_migrate_v2_skips_already_migrated() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    // Workspace that already has name should be untouched
    let unchanged: bool = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                name = "custom-workspace",
                repo = "owner/repo",
                status = "active",
                metadata = {{ custom = true }},
            }})
            ws.migrate_v2(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.name == "custom-workspace"
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v2 skips already migrated");

    assert!(unchanged, "Manifests with name should not be modified");
}

// =============================================================================
// Tier 1: migrate_v3 tests (dedup_key → name)
// =============================================================================

#[test]
fn test_migrate_v3_strips_github_prefix() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let name: String = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                title = "owner/repo — issue #42",
                dedup_key = "github:owner/repo#42",
                status = "active",
                metadata = {{ repo = "owner/repo", issue_number = 42 }},
            }})
            ws.migrate_v3(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.name
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v3 strips github prefix");

    assert_eq!(name, "owner/repo#42");
}

#[test]
fn test_migrate_v3_deletes_local_workspace() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let deleted: bool = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                title = "Local workspace",
                dedup_key = "local:owner-repo-main",
                status = "active",
            }})
            ws.migrate_v3(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest == nil
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v3 deletes local workspace");

    assert!(deleted, "local: workspaces should be deleted");
}

#[test]
fn test_migrate_v3_skips_already_migrated() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let unchanged: bool = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                name = "owner/repo#42",
                status = "active",
            }})
            ws.migrate_v3(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.name == "owner/repo#42"
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v3 skips already migrated");

    assert!(
        unchanged,
        "Manifests with name should not be modified by v3 migration"
    );
}

#[test]
fn test_migrate_v3_preserves_non_github_dedup_key() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let name: String = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                title = "Custom workspace",
                dedup_key = "custom:my-workspace",
                status = "active",
            }})
            ws.migrate_v3(dd)
            local manifest = ws.read_workspace(dd, ws_id)
            return manifest.name
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("migrate_v3 preserves non-github dedup_key");

    // Non-github, non-local dedup_key should be used as name directly
    assert_eq!(name, "custom:my-workspace");
}

// =============================================================================
// Tier 1: build_workspace_groups tests
// =============================================================================

#[test]
fn test_build_workspace_groups_uses_name() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let (has_name, has_metadata): (bool, bool) = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                name = "owner/repo#42",
                status = "active",
                metadata = {{ repo = "owner/repo", issue_number = 42 }},
            }})
            local agents = {{{{
                id = "test-agent",
                workspace_id = ws_id,
                repo = "owner/repo",
                branch_name = "botster-issue-42",
                workspace_name = "owner/repo#42",
            }}}}
            local groups = ws.build_workspace_groups(dd, agents)
            local g = groups[1]
            return g.name == "owner/repo#42",
                   g.metadata ~= nil and g.metadata.repo == "owner/repo"
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("build_workspace_groups uses name");

    assert!(has_name, "Workspace group should include name");
    assert!(has_metadata, "Workspace group should include metadata");
}

#[test]
fn test_build_workspace_groups_falls_back_to_branch_name_when_workspace_name_missing() {
    let dir = TempDir::new().unwrap();
    let lua = create_lua_vm(dir.path());
    load_workspace_store(&lua);

    let fallback_name: String = lua
        .load(format!(
            r#"
            local dd = "{dd}"
            ws.init_dir(dd)
            local ws_id = ws.generate_workspace_id()
            ws.write_workspace(dd, ws_id, {{
                id = ws_id,
                status = "active",
                metadata = {{}},
            }})
            local agents = {{{{
                id = "main-agent",
                workspace_id = ws_id,
                repo = "owner/repo",
                branch_name = "main",
            }}}}
            local groups = ws.build_workspace_groups(dd, agents)
            return groups[1].name
            "#,
            dd = dir.path().to_str().unwrap()
        ))
        .eval()
        .expect("build_workspace_groups missing name fallback");

    assert_eq!(
        fallback_name, "main",
        "Workspace group should default to branch name when manifest.name is missing"
    );
}
