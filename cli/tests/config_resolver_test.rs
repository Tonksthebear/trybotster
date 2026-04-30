//! Rust-hosted Lua coverage for the current `.botster/` config resolver.
//!
//! These tests pin the post-migration steady state: two config layers
//! (device + repo), repo-local `.botster[-dev]` wins on collisions, and list
//! helpers expose only the current agents/accessories/workspaces directories.

#![expect(clippy::unwrap_used, reason = "test-code brevity")]

use std::fs;
use std::path::{Path, PathBuf};

use mlua::Lua;
use tempfile::TempDir;

fn cli_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lua_string(path: &Path) -> String {
    serde_json::to_string(&path.to_string_lossy().to_string()).unwrap()
}

fn create_lua_vm() -> Lua {
    let lua = Lua::new();

    botster::lua::primitives::fs::register(&lua).expect("register fs");
    botster::lua::primitives::json::register(&lua).expect("register json");
    botster::lua::primitives::log::register(&lua).expect("register log");

    let lua_base = cli_manifest_dir().join("lua");
    lua.load(format!(
        r#"package.path = "{}/?.lua;{}/?/init.lua;" .. package.path"#,
        lua_base.to_string_lossy(),
        lua_base.to_string_lossy()
    ))
    .exec()
    .expect("set package.path");

    lua.load(r#"ConfigResolver = require("lib.config_resolver")"#)
        .exec()
        .expect("load config_resolver");

    lua
}

fn write_file(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn write_agent(root: &Path, name: &str, init: &str, manifest: Option<&str>) {
    let dir = root.join("agents").join(name);
    write_file(&dir.join("initialization"), init);
    if let Some(manifest) = manifest {
        write_file(&dir.join("manifest.json"), manifest);
    }
}

fn write_accessory(root: &Path, name: &str, init: &str, port_forward: bool) {
    let dir = root.join("accessories").join(name);
    write_file(&dir.join("initialization"), init);
    if port_forward {
        write_file(&dir.join("port_forward"), "");
    }
}

fn write_workspace(root: &Path, name: &str, manifest: &str) {
    write_file(
        &root.join("workspaces").join(format!("{name}.json")),
        manifest,
    );
}

fn write_plugin(root: &Path, name: &str) {
    write_file(
        &root.join("plugins").join(name).join("init.lua"),
        "return {}\n",
    );
}

#[test]
fn config_resolver_resolve_all_merges_current_device_and_repo_layers() {
    let tmp = TempDir::new().unwrap();
    let device_root = tmp.path().join(".botster-dev");
    let repo_root = tmp.path().join("repo");
    let repo_config = repo_root.join(".botster-dev");

    write_agent(
        &device_root,
        "codex",
        "device codex",
        Some(r#"{"target_id":"device-codex","label":"Device Codex","device_only":true}"#),
    );
    write_agent(&device_root, "reviewer", "device reviewer", None);
    write_agent(&device_root, "missing-init", "", None);
    fs::remove_file(
        device_root
            .join("agents")
            .join("missing-init")
            .join("initialization"),
    )
    .unwrap();
    write_accessory(&device_root, "rails-server", "device rails", true);
    write_workspace(
        &device_root,
        "dev",
        r#"{"agents":["codex"],"accessories":["rails-server"]}"#,
    );
    write_plugin(&device_root, "alpha");

    write_agent(
        &repo_config,
        "codex",
        "repo codex",
        Some(r#"{"label":"Repo Codex","repo_only":true}"#),
    );
    write_agent(&repo_config, "cursor", "repo cursor", None);
    write_accessory(&repo_config, "rails-server", "repo rails", false);
    write_accessory(&repo_config, "worker", "repo worker", true);
    write_workspace(
        &repo_config,
        "dev",
        r#"{"agents":["cursor"],"accessories":["worker"]}"#,
    );
    write_workspace(&repo_config, "review", r#"{"agents":["codex"]}"#);
    write_plugin(&repo_config, "beta");
    write_plugin(&repo_config, "alpha");
    write_file(
        &repo_config
            .join("plugins")
            .join("beta")
            .join("nested")
            .join("extra.lua"),
        "",
    );

    let ok: bool = lua_eval_bool(
        &create_lua_vm(),
        &format!(
            r#"
            local resolved, err = ConfigResolver.resolve_all({{
              device_root = {device_root},
              repo_root = {repo_root},
            }})
            assert(resolved ~= nil, tostring(err))

            assert(resolved.agents.codex.source == "repo")
            assert(resolved.agents.codex.initialization:match("/repo/%.botster%-dev/agents/codex/initialization$"))
            assert(resolved.agents.codex.manifest.target_id == "device-codex")
            assert(resolved.agents.codex.manifest.label == "Repo Codex")
            assert(resolved.agents.codex.manifest.device_only == true)
            assert(resolved.agents.codex.manifest.repo_only == true)
            assert(resolved.agents.reviewer.source == "device")
            assert(resolved.agents.cursor.source == "repo")
            assert(resolved.agents["missing-init"] == nil)

            assert(resolved.accessories["rails-server"].source == "repo")
            assert(resolved.accessories["rails-server"].port_forward == false)
            assert(resolved.accessories.worker.source == "repo")
            assert(resolved.accessories.worker.port_forward == true)

            assert(resolved.workspaces.dev.source == "repo")
            assert(resolved.workspaces.dev.agents[1] == "cursor")
            assert(resolved.workspaces.review.source == "repo")

            assert(#resolved.plugins == 2)
            assert(resolved.plugins[1].name == "alpha")
            assert(resolved.plugins[1].source == "repo")
            assert(resolved.plugins[2].name == "beta")
            assert(resolved.plugins[2].files[1] == "init.lua")
            assert(resolved.plugins[2].files[2] == "nested/extra.lua")
            return true
            "#,
            device_root = lua_string(&device_root),
            repo_root = lua_string(&repo_root),
        ),
    );

    assert!(ok, "resolve_all should expose current merged config");
}

#[test]
fn config_resolver_resolve_all_can_skip_agent_requirement_for_config_discovery() {
    let tmp = TempDir::new().unwrap();
    let device_root = tmp.path().join(".botster");
    fs::create_dir_all(&device_root).unwrap();

    let ok: bool = lua_eval_bool(
        &create_lua_vm(),
        &format!(
            r#"
            local missing, err = ConfigResolver.resolve_all({{
              device_root = {device_root},
            }})
            assert(missing == nil)
            assert(err:match("No agents found"))

            local resolved, skip_err = ConfigResolver.resolve_all({{
              device_root = {device_root},
              require_agent = false,
            }})
            assert(resolved ~= nil, tostring(skip_err))
            assert(next(resolved.agents) == nil)
            assert(next(resolved.accessories) == nil)
            assert(next(resolved.workspaces) == nil)
            assert(#resolved.plugins == 0)
            return true
            "#,
            device_root = lua_string(&device_root),
        ),
    );

    assert!(ok, "require_agent=false should allow empty current config");
}

#[test]
fn config_resolver_list_helpers_return_sorted_current_names_without_legacy_sources() {
    let tmp = TempDir::new().unwrap();
    let device_root = tmp.path().join(".botster-dev");
    let repo_root = tmp.path().join("repo");
    let repo_config = repo_root.join(".botster-dev");

    write_agent(&device_root, "z-device", "device", None);
    write_agent(&device_root, "shared", "device", None);
    write_file(
        &device_root
            .join("agents")
            .join("ignored-no-init")
            .join("notes.md"),
        "no init",
    );
    write_agent(&repo_config, "a-repo", "repo", None);
    write_agent(&repo_config, "shared", "repo", None);

    write_accessory(&device_root, "rails-server", "device", false);
    write_accessory(&repo_config, "worker", "repo", true);
    write_accessory(&repo_config, "terminal", "custom terminal", false);

    write_workspace(&device_root, "z-device", "{}");
    write_workspace(&device_root, "shared", "{}");
    write_file(&device_root.join("workspaces").join("notes.txt"), "ignored");
    write_workspace(&repo_config, "a-repo", "{}");
    write_workspace(
        &repo_config,
        "shared",
        "{not valid json but still a json file",
    );

    let ok: bool = lua_eval_bool(
        &create_lua_vm(),
        &format!(
            r#"
            local agents = ConfigResolver.list_agents({device_root}, {repo_root})
            assert(table.concat(agents, ",") == "a-repo,shared,z-device", table.concat(agents, ","))

            local accessories = ConfigResolver.list_accessories({device_root}, {repo_root})
            assert(table.concat(accessories, ",") == "terminal,rails-server,worker", table.concat(accessories, ","))

            local workspaces = ConfigResolver.list_workspaces({device_root}, {repo_root})
            assert(table.concat(workspaces, ",") == "a-repo,shared,z-device", table.concat(workspaces, ","))
            return true
            "#,
            device_root = lua_string(&device_root),
            repo_root = lua_string(&repo_root),
        ),
    );

    assert!(
        ok,
        "list helpers should expose sorted current directory names only"
    );
}

#[test]
fn config_resolver_repo_root_uses_default_botster_dir_when_device_root_is_nil() {
    let tmp = TempDir::new().unwrap();
    let repo_root = tmp.path().join("repo");
    let repo_config = repo_root.join(".botster");

    write_agent(&repo_config, "codex", "repo codex", None);
    write_accessory(&repo_config, "rails-server", "repo rails", false);
    write_workspace(&repo_config, "dev", r#"{"agents":["codex"]}"#);

    let ok: bool = lua_eval_bool(
        &create_lua_vm(),
        &format!(
            r#"
            local resolved, err = ConfigResolver.resolve_all({{
              repo_root = {repo_root},
            }})
            assert(resolved ~= nil, tostring(err))
            assert(resolved.agents.codex.source == "repo")
            assert(resolved.accessories["rails-server"].source == "repo")
            assert(resolved.workspaces.dev.source == "repo")

            local agents = ConfigResolver.list_agents(nil, {repo_root})
            local accessories = ConfigResolver.list_accessories(nil, {repo_root})
            local workspaces = ConfigResolver.list_workspaces(nil, {repo_root})
            assert(table.concat(agents, ",") == "codex")
            assert(table.concat(accessories, ",") == "terminal,rails-server")
            assert(table.concat(workspaces, ",") == "dev")
            return true
            "#,
            repo_root = lua_string(&repo_root),
        ),
    );

    assert!(ok, "nil device_root should resolve repo/.botster");
}

fn lua_eval_bool(lua: &Lua, code: &str) -> bool {
    lua.load(code).eval().expect("Lua assertions should pass")
}
