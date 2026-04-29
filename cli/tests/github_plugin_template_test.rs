//! Regression tests for the GitHub plugin catalog template shape.

use mlua::Lua;

fn create_lua_vm() -> Lua {
    let lua = Lua::new();

    botster::lua::primitives::fs::register(&lua).expect("fs register");
    botster::lua::primitives::json::register(&lua).expect("json register");
    botster::lua::primitives::log::register(&lua).expect("log register");

    let lua_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
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
fn github_template_catalog_entry_is_a_multi_file_plugin() {
    let catalog_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("catalog/templates");
    let catalog_root = catalog_root.to_str().unwrap();

    let lua = create_lua_vm();

    let files: Vec<String> = lua
        .load(format!(
            r#"
            local catalog = require("lib.template_catalog")
            local templates = catalog.list({{ source_root = "{catalog_root}" }})
            local out = {{}}
            for _, template in ipairs(templates) do
              if template.dest:match("^plugins/github/") then
                out[#out + 1] = template.dest
              end
            end
            table.sort(out)
            return out
            "#
        ))
        .eval()
        .expect("template catalog should load GitHub plugin template files");

    assert_eq!(
        files,
        vec![
            "plugins/github/event_routing.lua",
            "plugins/github/init.lua",
            "plugins/github/mcp_proxy.lua",
            "plugins/github/notifications.lua",
        ]
    );
}
