//! Runtime tests for the Cloudflare stable URL pool plugin.

#![expect(clippy::unwrap_used, reason = "test-code brevity")]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::{fs, path::Path};

use botster::lua::LuaRuntime;
use tempfile::TempDir;

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn test_lock() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn repo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli has repo parent")
        .to_path_buf()
}

fn plugin_path() -> PathBuf {
    repo_dir()
        .join("catalog")
        .join("templates")
        .join("plugins")
        .join("cloudflare-stable-urls")
        .join("init.lua")
}

fn read_tree(path: &std::path::Path, out: &mut String) {
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            read_tree(&path, out);
        } else {
            out.push_str(&std::fs::read_to_string(path).unwrap());
            out.push('\n');
        }
    }
}

fn write_consumer_plugin(dir: &Path) -> PathBuf {
    let plugin_dir = dir.join("stable-url-consumer");
    fs::create_dir_all(&plugin_dir).unwrap();
    let init_path = plugin_dir.join("init.lua");
    fs::write(
        &init_path,
        r#"
        local stable_urls = require("lib.stable_urls")

        mcp.tool("consumer_stable_url_claim", {
          description = "Claim a stable URL through the shared facade from a consumer worker.",
          input_schema = { type = "object", properties = {} },
        }, function()
          return stable_urls.claim({
            owner_plugin = "consumer",
            owner_key = "fixture",
            purpose = "webhook",
            local_service_url = "http://127.0.0.1:48111",
          })
        end)

        mcp.tool("consumer_stable_url_list", {
          description = "List stable URLs through the shared facade from a consumer worker.",
          input_schema = { type = "object", properties = {} },
        }, function()
          return stable_urls.list({ owner_plugin = "consumer", owner_key = "fixture" })
        end)

        return {}
        "#,
    )
    .unwrap();
    init_path
}

fn runtime() -> (TempDir, LuaRuntime) {
    let dir = TempDir::new().unwrap();
    unsafe {
        std::env::set_var("BOTSTER_CONFIG_DIR", dir.path());
        std::env::set_var(
            "BOTSTER_LUA_PATH",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua"),
        );
    }
    (dir, LuaRuntime::new().expect("runtime"))
}

fn load_plugin(runtime: &LuaRuntime) {
    let init_path = serde_json::to_string(&plugin_path().to_string_lossy()).unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            _G.mcp = require("lib.mcp")
            local plugin_db = require("lib.plugin_db")
            plugin_db.install()

            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({init_path}, "cloudflare-stable-urls", {{ source = "device" }})
            assert(ok, tostring(err))
            "#
        ))
        .exec()
        .expect("load stable url plugin");
}

#[test]
fn cloudflare_stable_urls_loads_api_mcp_and_entity_provider() {
    let _guard = test_lock();
    let (_dir, runtime) = runtime();
    load_plugin(&runtime);

    runtime
        .lua()
        .load(
            r#"
            local stable_urls = require("lib.stable_urls")
            local mcp = require("lib.mcp")
            local EB = require("lib.entity_broadcast")

            assert(stable_urls.get({ id = "surl_seed_1" }).hostname == "hook-1.example.invalid")

            local tool_names = {}
            for _, tool in ipairs(mcp.list_tools()) do
              tool_names[tool.name] = true
            end
            assert(tool_names.stable_urls_claim)
            assert(tool_names.stable_urls_release)
            assert(tool_names.stable_urls_list)
            assert(tool_names.stable_urls_get)

            local sent = {}
            local client = { send = function(_, frame) sent[#sent + 1] = frame end }
            EB.send_snapshots_to(client, "sub-stable", {
              entity_types = { "cloudflare-stable-urls.stable_url" },
            })
            local found = false
            for _, frame in ipairs(sent) do
              if frame.type == "entity_snapshot" and frame.entity_type == "cloudflare-stable-urls.stable_url" then
                found = true
                assert(#frame.items == 2)
                assert(frame.items[1].hostname:match("%.example%.invalid$"))
                assert(frame.items[1].token_secret_key == nil)
                assert(frame.items[1].provider_metadata_json == nil)
              end
            end
            assert(found, "stable_url entity snapshot missing")
            "#,
        )
        .exec()
        .expect("api/mcp/entity provider assertions");
}

#[test]
fn cloudflare_stable_urls_claim_release_reclaim_and_audit() {
    let _guard = test_lock();
    let (_dir, runtime) = runtime();
    load_plugin(&runtime);

    runtime
        .lua()
        .load(
            r#"
            local stable_urls = require("lib.stable_urls")
            local repo = require("cloudflare_stable_urls.repo")

            local claimed = stable_urls.claim({
              owner_plugin = "github",
              owner_key = "repo:owner/name",
              purpose = "webhook",
              local_service_url = "http://127.0.0.1:47123",
              session_uuid = "sess-claim",
            })
            assert(claimed.status == "claimed")
            assert(claimed.owner_key == "repo:owner/name")
            assert(claimed.owner_id == nil)
            assert(claimed.token_secret_key == nil)

            local owner_id_ok = pcall(function()
              stable_urls.claim({
                owner_plugin = "github",
                owner_id = "repo:owner/name",
                purpose = "webhook",
                local_service_url = "http://127.0.0.1:47123",
              })
            end)
            assert(owner_id_ok == false, "owner_id should not be accepted")

            local conflict_ok = pcall(function()
              stable_urls.claim({
                id = claimed.id,
                owner_plugin = "slack",
                owner_key = "team:1",
                purpose = "webhook",
                local_service_url = "http://127.0.0.1:47124",
              })
            end)
            assert(conflict_ok == false, "second active claim for same URL should fail")

            local other = stable_urls.claim({
              owner_plugin = "slack",
              owner_key = "team:1",
              purpose = "webhook",
              local_service_url = "http://127.0.0.1:47124",
            })
            assert(other.status == "claimed")

            local bad_release_ok = pcall(function()
              stable_urls.release({
                id = claimed.id,
                owner_plugin = "github",
                owner_key = "repo:other/name",
              })
            end)
            assert(bad_release_ok == false, "non-owner release should fail")

            local released = stable_urls.release({
              id = claimed.id,
              owner_plugin = "github",
              owner_key = "repo:owner/name",
              reason = "test release",
            })
            assert(released.status == "available")
            assert(released.owner_plugin == nil)
            assert(released.owner_key == nil)

            local reclaimed = stable_urls.claim({
              owner_plugin = "github",
              owner_key = "repo:owner/name",
              purpose = "webhook",
              local_service_url = "http://127.0.0.1:47123",
            })
            assert(reclaimed.id == claimed.id)
            assert(reclaimed.status == "claimed")

            local filtered = stable_urls.list({
              status = "claimed",
              owner_plugin = "github",
              owner_key = "repo:owner/name",
            })
            assert(#filtered == 1)
            assert(filtered[1].id == claimed.id)

            local audit = repo.audit_events()
            local seen = {}
            for _, event in ipairs(audit) do
              seen[event.action] = true
              assert(event.metadata_json ~= nil)
              assert(event.token_secret_key == nil)
            end
            local seen_json = json.encode(seen)
            assert(seen.claim, seen_json)
            assert(seen.claim_failed, seen_json)
            assert(seen.release, seen_json)
            assert(seen.release_failed, seen_json)
            "#,
        )
        .exec()
        .expect("claim/release assertions");
}

#[test]
fn cloudflare_stable_urls_mcp_and_reload_keep_redacted_shapes() {
    let _guard = test_lock();
    let (_dir, runtime) = runtime();
    load_plugin(&runtime);

    let init_path = serde_json::to_string(&plugin_path().to_string_lossy()).unwrap();
    runtime
        .lua()
        .load(format!(
            r#"
            local mcp = require("lib.mcp")
            local EB = require("lib.entity_broadcast")

            local content, err = mcp.call_tool("stable_urls_claim", {{
              owner_plugin = "github",
              owner_key = "repo:owner/name",
              purpose = "webhook",
              local_service_url = "http://127.0.0.1:47123",
            }}, {{ session_uuid = "sess-mcp" }})
            assert(err == nil, tostring(err))
            local claimed = json.decode(content[1].text).result
            assert(claimed.status == "claimed")
            assert(claimed.session_uuid == "sess-mcp")
            assert(claimed.token_secret_key == nil)

            local loader = require("hub.loader")
            local ok, reload_err = loader.load_plugin({init_path}, "cloudflare-stable-urls", {{ source = "device" }})
            assert(ok, tostring(reload_err))

            local list_content, list_err = mcp.call_tool("stable_urls_list", {{
              owner_plugin = "github",
              owner_key = "repo:owner/name",
            }}, {{ session_uuid = "sess-mcp" }})
            assert(list_err == nil, tostring(list_err))
            local rows = json.decode(list_content[1].text).result
            assert(#rows == 1)
            assert(rows[1].id == claimed.id)
            assert(rows[1].token_secret_key == nil)

            local stable_urls = require("lib.stable_urls")
            local got = stable_urls.get({{ id = claimed.id }})
            assert(got.id == claimed.id)
            assert(got.token_secret_key == nil)

            local sent = {{}}
            local client = {{ send = function(_, frame) sent[#sent + 1] = frame end }}
            EB.send_snapshots_to(client, "sub-stable-reload", {{
              entity_types = {{ "cloudflare-stable-urls.stable_url" }},
            }})
            local found_reloaded_snapshot = false
            for _, frame in ipairs(sent) do
              if frame.type == "entity_snapshot" and frame.entity_type == "cloudflare-stable-urls.stable_url" then
                found_reloaded_snapshot = true
                assert(#frame.items == 2)
                local claimed_item = nil
                for _, item in ipairs(frame.items) do
                  if item.id == claimed.id then
                    claimed_item = item
                  end
                  assert(item.token_secret_key == nil)
                  assert(item.provider_metadata_json == nil)
                end
                assert(claimed_item ~= nil, "claimed URL missing from reloaded entity snapshot")
                assert(claimed_item.status == "claimed")
                assert(claimed_item.owner_plugin == "github")
                assert(claimed_item.owner_key == "repo:owner/name")
              end
            end
            assert(found_reloaded_snapshot, "stable_url entity snapshot missing after reload")
            "#,
        ))
        .exec()
        .expect("mcp/reload assertions");
}

#[test]
fn cloudflare_stable_urls_consumer_worker_facade_survives_provider_reload() {
    let _guard = test_lock();
    let (_dir, runtime) = runtime();
    load_plugin(&runtime);

    let consumer_root = TempDir::new().unwrap();
    let consumer_path =
        serde_json::to_string(&write_consumer_plugin(consumer_root.path()).to_string_lossy())
            .unwrap();
    let stable_path = serde_json::to_string(&plugin_path().to_string_lossy()).unwrap();

    runtime
        .lua()
        .load(format!(
            r#"
            hub.hub_id = hub.hub_id or function() return "hub-test" end
            hub.server_id = hub.server_id or function() return "hub-test" end

            local loader = require("hub.loader")
            local ok, err = loader.load_plugin({consumer_path}, "stable-url-consumer", {{ source = "device" }})
            assert(ok, tostring(err))

            local content, claim_err = require("lib.mcp").call_tool("consumer_stable_url_claim", {{}}, {{}})
            assert(claim_err == nil, tostring(claim_err))
            local claimed = json.decode(content[1].text)
            assert(claimed.owner_plugin == "consumer")
            assert(claimed.owner_key == "fixture")
            assert(claimed.token_secret_key == nil)

            local reload_ok, reload_err = loader.load_plugin({stable_path}, "cloudflare-stable-urls", {{ source = "device" }})
            assert(reload_ok, tostring(reload_err))

            local list_content, list_err = require("lib.mcp").call_tool("consumer_stable_url_list", {{}}, {{}})
            assert(list_err == nil, tostring(list_err))
            local rows = json.decode(list_content[1].text)
            assert(#rows == 1)
            assert(rows[1].id == claimed.id)
            assert(rows[1].token_secret_key == nil)
            "#,
        ))
        .exec()
        .expect("consumer worker facade assertions");
}

#[test]
fn cloudflare_stable_urls_source_uses_reserved_hostnames_only() {
    let _guard = test_lock();
    let root = repo_dir().join("catalog/templates/plugins/cloudflare-stable-urls");
    let mut joined = String::new();
    read_tree(&root, &mut joined);

    assert!(!joined.contains("trycloudflare.com"));
    assert!(!joined.contains("workers.dev"));
    assert!(!joined.contains("cloudflared tunnel run --url"));
    assert!(joined.contains("example.invalid"));
}
