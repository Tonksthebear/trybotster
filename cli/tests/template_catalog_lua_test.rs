//! Rust-hosted Lua tests for the hub-owned template catalog loader.

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
fn template_catalog_parses_metadata_and_groups_by_category() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("templates");

    std::fs::create_dir_all(root.join("plugins/demo")).unwrap();
    std::fs::create_dir_all(root.join("agents/codex")).unwrap();

    std::fs::write(
        root.join("plugins/demo/init.lua"),
        r#"-- @template Demo Plugin
-- @description Adds demo behavior
-- @category plugins
-- @dest plugins/demo/init.lua
-- @scope repo

return {}
"#,
    )
    .unwrap();
    std::fs::write(
        root.join("agents/codex/notes.md"),
        r#"<!-- @template Codex Notes -->
<!-- @description Notes for an agent -->
<!-- @category agents -->
<!-- @dest agents/codex/notes.md -->

Notes body.
"#,
    )
    .unwrap();
    std::fs::write(root.join("plugins/demo/ignored.txt"), "not a template").unwrap();
    std::fs::write(root.join("plugins/demo/missing.lua"), "-- no metadata").unwrap();

    let lua = create_lua_vm();
    let root = root.to_str().unwrap();

    let ok: bool = lua
        .load(format!(
            r#"
            local catalog = require("lib.template_catalog")
            local templates = catalog.list({{ source_root = "{root}" }})
            local grouped = catalog.group_by_category(templates)

            return #templates == 2
                and templates[1].category == "agents"
                and templates[1].slug == "agents-agents-codex-notes"
                and templates[1].dest == "agents/codex/notes.md"
                and templates[1].content:match("Notes body") ~= nil
                and templates[2].category == "plugins"
                and templates[2].name == "Demo Plugin"
                and templates[2].scope == "repo"
                and #grouped.agents == 1
                and #grouped.plugins == 1
            "#
        ))
        .eval()
        .expect("template catalog should parse fixture catalog");

    assert!(
        ok,
        "catalog should parse metadata, ignore invalid files, and group templates"
    );
}

#[test]
fn template_catalog_uses_configured_bundled_source_root() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("catalog");
    std::fs::create_dir_all(root.join("initialization")).unwrap();
    std::fs::write(
        root.join("initialization/basic.lua"),
        r#"-- @template Basic Init
-- @description Basic initialization
-- @category initialization
-- @dest initialization/basic.lua

return {}
"#,
    )
    .unwrap();

    let lua = create_lua_vm();
    let root = root.to_str().unwrap();
    lua.load(format!(
        r#"
        _G.config = {{
          template_catalog_path = function() return "{root}" end,
        }}
        "#
    ))
    .exec()
    .expect("stub config");

    let dest: String = lua
        .load(
            r#"
            local catalog = require("lib.template_catalog")
            local templates = catalog.list()
            return templates[1].dest
            "#,
        )
        .eval()
        .expect("template catalog should read config.template_catalog_path");

    assert_eq!(dest, "initialization/basic.lua");
}
