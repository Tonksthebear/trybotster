//! Rust-hosted Lua tests for config_resolver.lua.

use mlua::Lua;
use tempfile::TempDir;

fn create_lua_vm() -> Lua {
    let lua = Lua::new();

    botster::lua::primitives::fs::register(&lua).expect("fs register");
    botster::lua::primitives::json::register(&lua).expect("json register");
    botster::lua::primitives::log::register(&lua).expect("log register");

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

    lua
}

#[test]
fn profiles_directory_does_not_trigger_migration() {
    let dir = TempDir::new().unwrap();
    let device_root = dir.path().join("device");
    let repo_root = dir.path().join("repo");

    std::fs::create_dir_all(device_root.join("profiles")).unwrap();
    std::fs::create_dir_all(repo_root.join(".botster-dev/profiles")).unwrap();

    let lua = create_lua_vm();
    let needs_migration: bool = lua
        .load(format!(
            r#"
            local resolver = require("lib.config_resolver")
            return resolver.needs_migration("{device_root}", "{repo_root}")
            "#,
            device_root = device_root.to_str().unwrap(),
            repo_root = repo_root.to_str().unwrap()
        ))
        .eval()
        .expect("needs_migration should be callable");

    assert!(
        !needs_migration,
        "old profiles directories should not trigger migration"
    );
}

#[test]
fn shared_sessions_directory_still_triggers_migration() {
    let dir = TempDir::new().unwrap();
    let device_root = dir.path().join("device");
    let repo_root = dir.path().join("repo");

    std::fs::create_dir_all(device_root.join("shared/sessions")).unwrap();
    std::fs::create_dir_all(&repo_root).unwrap();

    let lua = create_lua_vm();
    let needs_migration: bool = lua
        .load(format!(
            r#"
            local resolver = require("lib.config_resolver")
            return resolver.needs_migration("{device_root}", "{repo_root}")
            "#,
            device_root = device_root.to_str().unwrap(),
            repo_root = repo_root.to_str().unwrap()
        ))
        .eval()
        .expect("needs_migration should be callable");

    assert!(
        needs_migration,
        "old shared/sessions directories should still trigger migration"
    );
}
