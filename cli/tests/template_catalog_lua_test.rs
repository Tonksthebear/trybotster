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
fn template_catalog_uses_explicit_local_source_root() {
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
        .expect("template catalog should read explicit config.template_catalog_path");

    assert_eq!(dest, "initialization/basic.lua");
}

#[test]
fn template_catalog_fetches_github_catalog_and_caches_template_entities() {
    let dir = TempDir::new().unwrap();
    let cache_root = dir.path().join("botster");
    std::fs::create_dir_all(&cache_root).unwrap();

    let lua = create_lua_vm();
    let cache_root = cache_root.to_str().unwrap();
    lua.load(format!(
        r#"
        _G.config = {{
          data_dir = function() return "{cache_root}" end,
          get = function(_key) return nil end,
          env = function(_key) return nil end,
        }}

        local bodies = {{
          ["https://api.github.com/repos/Tonksthebear/trybotster/contents/catalog/templates?ref=main"] = [[
            [
              {{
                "type":"dir",
                "path":"catalog/templates/plugins/demo",
                "url":"https://api.github.com/demo-dir"
              }},
              {{
                "type":"file",
                "path":"catalog/templates/agents/codex.sh",
                "download_url":"https://raw.example/codex.sh",
                "html_url":"https://github.example/codex.sh"
              }}
            ]
          ]],
          ["https://api.github.com/demo-dir"] = [[
            [
              {{
                "type":"file",
                "path":"catalog/templates/plugins/demo/init.lua",
                "download_url":"https://raw.example/demo.lua",
                "html_url":"https://github.example/demo.lua"
              }},
              {{
                "type":"file",
                "path":"catalog/templates/plugins/demo/readme.txt",
                "download_url":"https://raw.example/readme.txt"
              }}
            ]
          ]],
          ["https://raw.example/codex.sh"] = [[# @template Codex
# @description Codex agent
# @category agents
# @dest agents/codex.sh

codex]],
          ["https://raw.example/demo.lua"] = [[-- @template Demo
-- @description Demo plugin
-- @category plugins
-- @dest plugins/demo/init.lua
-- @scope repo

return {{}}]],
        }}

        _G.http = {{
          get = function(url, _opts)
            local body = bodies[url]
            if body then return {{ status = 200, body = body, headers = {{}} }} end
            return nil, "unexpected url: " .. tostring(url)
          end,
        }}
        "#
    ))
    .exec()
    .expect("stub config/http");

    let ok: bool = lua
        .load(
            r#"
            local catalog = require("lib.template_catalog")
            local refreshed = assert(catalog.refresh())
            local cached = catalog.list()
            return #refreshed == 2
              and #cached == 2
              and cached[1].source == "github"
              and cached[1].dest == "agents/codex.sh"
              and cached[1].source_url == "https://github.example/codex.sh"
              and cached[2].dest == "plugins/demo/init.lua"
              and cached[2].scope == "repo"
              and fs.exists(catalog.cache_path())
            "#,
        )
        .eval()
        .expect("catalog should fetch and cache GitHub contents");

    assert!(
        ok,
        "remote catalog should produce cached hub template entities"
    );
}

#[test]
fn template_catalog_uses_env_and_config_overrides_for_remote_provider() {
    let lua = create_lua_vm();

    let url: String = lua
        .load(
            r#"
            _G.config = {
              get = function(key)
                if key == "template_catalog_url" then return "https://example.test/catalog" end
                if key == "template_catalog_ref" then return "dev" end
                return nil
              end,
              env = function(_key) return nil end,
            }
            local catalog = require("lib.template_catalog")
            return catalog.default_remote_url()
            "#,
        )
        .eval()
        .expect("catalog should read config override");

    assert_eq!(url, "https://example.test/catalog");
}

#[test]
fn template_catalog_async_refresh_publishes_templates_without_sync_http() {
    let dir = TempDir::new().unwrap();
    let cache_root = dir.path().join("botster");
    std::fs::create_dir_all(&cache_root).unwrap();

    let lua = create_lua_vm();
    let cache_root = cache_root.to_str().unwrap();
    lua.load(format!(
        r#"
        _G.config = {{
          data_dir = function() return "{cache_root}" end,
          get = function(_key) return nil end,
          env = function(_key) return nil end,
        }}
        _G.published_templates = {{}}
        package.loaded["lib.entity_model"] = {{
          upsert_template = function(template)
            table.insert(_G.published_templates, template)
          end,
        }}

        local bodies = {{
          ["https://api.github.com/repos/Tonksthebear/trybotster/contents/catalog/templates?ref=main"] = [[
            [
              {{
                "type":"file",
                "path":"catalog/templates/plugins/demo/init.lua",
                "download_url":"https://raw.example/demo.lua",
                "html_url":"https://github.example/demo.lua"
              }}
            ]
          ]],
          ["https://raw.example/demo.lua"] = [[-- @template Demo
-- @description Demo plugin
-- @category plugins
-- @dest plugins/demo/init.lua

return {{}}]],
        }}

        _G.http = {{
          request = function(req, callback)
            local body = bodies[req.url]
            if body then
              callback({{ status = 200, body = body, headers = {{}} }}, nil)
            else
              callback(nil, "unexpected url: " .. tostring(req.url))
            end
          end,
        }}
        "#
    ))
    .exec()
    .expect("stub async config/http");

    let ok: bool = lua
        .load(
            r#"
            local catalog = require("lib.template_catalog")
            local started = catalog.refresh_async()
            local cached = catalog.list()
            assert(started == true, "refresh did not start")
            assert(#_G.published_templates == 1, "published count was " .. tostring(#_G.published_templates))
            assert(_G.published_templates[1].dest == "plugins/demo/init.lua", "bad published dest")
            assert(#cached == 1, "cached count was " .. tostring(#cached))
            assert(cached[1].source == "github", "bad cached source")
            return true
            "#,
        )
        .eval()
        .expect("async refresh should publish and cache templates");

    assert!(
        ok,
        "async refresh should use http.request and publish template entities"
    );
}
