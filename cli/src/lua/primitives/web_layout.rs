//! Web-layout primitive — `web_layout.render(surface, state)`.
//!
//! Phase 2a of the cross-client UI DSL migration: the hub Lua VM composes a
//! `UiNodeV1` tree for browser surfaces and serialises it to JSON. The
//! primitive is pure (no I/O beyond reading override files on disk, no
//! broadcast) so browsers stay decoupled from the hub event loop until Phase
//! 2b wires transport.
//!
//! # Resolution chain
//!
//! On each `render(surface, state)` call the primitive resolves the layout
//! table in this order, first hit wins:
//!
//! 1. `<repo>/.botster/layout_web.lua`   (web-only override, repo-scoped)
//! 2. `~/.botster/layout_web.lua`        (web-only override, device-scoped)
//! 3. `<repo>/.botster/layout.lua`       (shared override, repo-scoped)
//! 4. `~/.botster/layout.lua`            (shared override, device-scoped)
//! 5. `require("web.layout")`            (embedded default, shipped in cli/lua/web/)
//!
//! Each candidate is a Lua chunk that returns a table keyed by surface name;
//! `table[surface]` is expected to be a function `(state) -> UiNodeV1`.
//!
//! In dev mode (`BOTSTER_DEV=1`), `.botster-dev/` is tried before `.botster/`
//! for each of the repo-scoped and device-scoped paths. Device-scoped paths
//! also honour `BOTSTER_CONFIG_DIR` (as used by the rest of the CLI's test
//! infrastructure).
//!
//! # Error handling
//!
//! Any error raised while resolving, calling, or serialising the layout is
//! wrapped and returned as a fallback `UiNodeV1` tree (an `ui.panel{}` with
//! the error message). The hub Lua VM never observes a Rust error from this
//! primitive — this is required so a broken layout file cannot crash a
//! long-running hub.

// Rust guideline compliant 2026-04-18

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use mlua::{Function, Lua, LuaSerdeExt, Table, Value};

use crate::ui_contract::node::UiNodeV1;

/// The env var that toggles dev-mode config directories (`.botster-dev/` vs
/// `.botster/`). Matches the convention used elsewhere in the CLI.
const DEV_ENV_VAR: &str = "BOTSTER_DEV";

/// Override for the device config directory (`~/.botster/`). Used by the test
/// harness to isolate from real user state; honoured by `primitives::config`
/// for the same reason.
const DEVICE_DIR_OVERRIDE_ENV_VAR: &str = "BOTSTER_CONFIG_DIR";

/// Test-only override for the repo config directory. Lets integration tests
/// place layout files in a tempdir without needing an enclosing git repo.
const REPO_DIR_OVERRIDE_ENV_VAR: &str = "BOTSTER_WEB_LAYOUT_REPO_DIR";

/// Embedded-default module name, resolved via `require()`. Must correspond to
/// `cli/lua/web/layout.lua` on disk (and the embedded searcher's module name).
const EMBEDDED_LAYOUT_MODULE: &str = "web.layout";

/// Override file name for web-only layouts.
const LAYOUT_WEB_FILE: &str = "layout_web.lua";

/// Override file name for shared (TUI + web) layouts. Phase 2a only wires this
/// for web; Phase 2b or later may let the TUI consume the same file.
const LAYOUT_SHARED_FILE: &str = "layout.lua";

/// Install `web_layout` as a global Lua table with two methods: `render` and
/// `reload`.
///
/// ```lua
/// local json = web_layout.render("workspace_surface", {
///     hub_id = "hub-1",
///     agents = { ... },
///     open_workspaces = { ... },
///     selected_session_uuid = nil,
///     surface = "panel",
/// })
/// ```
///
/// The returned string is a JSON-encoded [`UiNodeV1`] tree ready to be shipped
/// to browsers by the Phase 2b transport wiring.
///
/// # Errors
///
/// Returns an error if the `web_layout` table or `render` function cannot be
/// created. Never propagates errors from layout evaluation — those collapse
/// into a fallback tree returned by `render` itself.
pub fn register(lua: &Lua) -> Result<()> {
    let table = lua
        .create_table()
        .map_err(|e| anyhow!("Failed to create web_layout table: {e}"))?;

    let render_fn = lua
        .create_function(|lua, (surface_name, state): (String, Value)| {
            let json = match render_surface(lua, &surface_name, state) {
                Ok(json) => json,
                Err(err) => {
                    log::warn!(
                        "web_layout.render: surface={surface_name} failed — returning fallback tree: {err:#}"
                    );
                    error_fallback_json(&surface_name, &format!("{err:#}"))
                }
            };
            Ok(json)
        })
        .map_err(|e| anyhow!("Failed to create web_layout.render: {e}"))?;

    table
        .set("render", render_fn)
        .map_err(|e| anyhow!("Failed to set web_layout.render: {e}"))?;

    // `web_layout.reload()` — explicit invalidation of all caches. Matches
    // the `reload_plugin` pattern: callers explicitly opt in so editor fs
    // chatter doesn't trigger spurious reloads mid-edit. The hub does not
    // watch files.
    let reload_fn = lua
        .create_function(|lua, (): ()| {
            reload(lua).map_err(|e| mlua::Error::external(format!("{e:#}")))?;
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create web_layout.reload: {e}"))?;

    table
        .set("reload", reload_fn)
        .map_err(|e| anyhow!("Failed to set web_layout.reload: {e}"))?;

    lua.globals()
        .set("web_layout", table)
        .map_err(|e| anyhow!("Failed to register web_layout global: {e}"))?;

    log::debug!("Registered web_layout primitive");
    Ok(())
}

/// Invalidate every layer of caching so the next `render()` re-reads the
/// override file from disk, re-evaluates it, and refreshes the embedded
/// module. Safe to call from Lua (`web_layout.reload()`) and from Rust.
///
/// Does NOT itself broadcast — callers (e.g. the `reload_layout` command
/// handler) are responsible for triggering `broadcast_ui_layout_trees` after
/// invalidation so subscribers re-render.
pub fn reload(lua: &Lua) -> Result<()> {
    // Rust-side override cache (path/mtime/content)
    if let Ok(mut guard) = OVERRIDE_CACHE.lock() {
        *guard = None;
    }
    // Lua-side compiled override table (content-hash cache)
    let globals = lua.globals();
    globals
        .set(LUA_OVERRIDE_HASH_KEY, Value::Nil)
        .map_err(|e| anyhow!("reload: clear override-hash cache: {e}"))?;
    globals
        .set(LUA_OVERRIDE_RESULT_KEY, Value::Nil)
        .map_err(|e| anyhow!("reload: clear override-result cache: {e}"))?;
    // Embedded `require("web.layout")` cache — dropped so a fresh require()
    // re-runs the shipped Lua source. Defense against overrides that may have
    // mutated the singleton during their lifetime.
    reset_embedded_module(lua)?;
    log::info!("web_layout.reload: all caches invalidated");
    Ok(())
}

/// Core render pipeline: resolve the layout table, look up the surface
/// function, call it with the state, and serialise the resulting node.
fn render_surface(lua: &Lua, surface_name: &str, state: Value) -> Result<String> {
    let layout_table = resolve_layout_table(lua)?;

    let surface_fn: Function = layout_table
        .get(surface_name)
        .map_err(|e| anyhow!("layout table has no surface `{surface_name}`: {e}"))?;

    let returned: Value = surface_fn
        .call(state)
        .map_err(|e| anyhow!("surface `{surface_name}` raised: {e}"))?;

    let node: UiNodeV1 = lua
        .from_value(returned)
        .map_err(|e| anyhow!("surface `{surface_name}` did not return a UiNodeV1: {e}"))?;

    serde_json::to_string(&node)
        .map_err(|e| anyhow!("failed to serialise UiNodeV1 for `{surface_name}`: {e}"))
}

/// Process-wide cache of the last successful override-chain scan.
///
/// Held for the lifetime of the hub process. Only explicit `reload()` calls
/// invalidate it — there is no TTL and no filesystem watcher. This matches
/// the `reload_plugin` pattern for plugins: editor fs chatter mid-edit
/// shouldn't trigger spurious hub activity; users explicitly opt in to
/// re-reading when they finish editing.
static OVERRIDE_CACHE: Mutex<Option<OverrideCache>> = Mutex::new(None);

/// Snapshot of the override-chain resolution, guarded by `OVERRIDE_CACHE`.
///
/// `winning` records which candidate (if any) won the scan. `None` means the
/// embedded default took over. The cache is only cleared by [`reload`]; once
/// populated, `resolve_layout_table` never re-stats the four candidate paths.
#[derive(Clone)]
struct OverrideCache {
    winning: Option<CachedOverride>,
}

/// Cached payload for one override file: source text observed at scan time.
/// Stored as `String` (not `Table`) because Lua tables can't outlive their
/// parent `Lua`; the Lua-side content-hash cache in
/// [`load_override_from_cache`] handles Table reuse across renders.
#[derive(Clone)]
struct CachedOverride {
    path: PathBuf,
    content: String,
}

/// Walk the resolution chain and return the first layout table that loads
/// successfully. Falls back to the embedded default via `require`.
///
/// Hot path after the first call is entirely zero-I/O:
/// - `OVERRIDE_CACHE` holds the winning override's path + content (or
///   `None` meaning embedded wins);
/// - `load_override_cached` looks up the compiled Table from the Lua-side
///   content-hash cache and returns it directly.
///
/// The only way to re-stat the filesystem or re-evaluate a chunk is to call
/// [`reload`]. See the module docstring for rationale.
fn resolve_layout_table(lua: &Lua) -> Result<Table> {
    if let Some(cached) = cache_hit() {
        match cached.winning {
            Some(entry) => return load_override_cached(lua, &entry.path, &entry.content),
            None => return load_embedded(lua),
        }
    }
    scan_and_load(lua)
}

/// Clear `package.loaded[EMBEDDED_LAYOUT_MODULE]` so the next
/// `require("web.layout")` call — whether from inside an override or from
/// `load_embedded` — re-evaluates the embedded source and returns a fresh
/// table. Called only from [`reload`]; the Lua-side content-hash cache
/// means per-render resets are unnecessary in the steady state.
fn reset_embedded_module(lua: &Lua) -> Result<()> {
    let package: Table = lua
        .globals()
        .get("package")
        .map_err(|e| anyhow!("cannot find `package` global: {e}"))?;
    let loaded: Table = package
        .get("loaded")
        .map_err(|e| anyhow!("`package.loaded` missing: {e}"))?;
    loaded
        .set(EMBEDDED_LAYOUT_MODULE, Value::Nil)
        .map_err(|e| anyhow!("failed to clear package.loaded[`{EMBEDDED_LAYOUT_MODULE}`]: {e}"))?;
    Ok(())
}

/// Cache key for the Lua-side compiled-override Table. Hashed by content so
/// re-evaluation happens at most once per content change, not once per
/// render.
const LUA_OVERRIDE_RESULT_KEY: &str = "__botster_web_layout_override_module";
const LUA_OVERRIDE_HASH_KEY: &str = "__botster_web_layout_override_hash";

/// Hash of the override source content, formatted as a hex string to dodge
/// any number-representation subtleties on the Lua side. Non-cryptographic;
/// just a collision-resistant identity for cache lookup.
fn content_hash(content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Attempt to load a compiled-override table from a Lua-side cache keyed by
/// content hash. Returns `Ok(None)` if no cached entry exists for this
/// content; evaluation proceeds normally in that case.
fn load_override_from_cache(lua: &Lua, content: &str) -> Result<Option<Table>> {
    let globals = lua.globals();
    let stored_hash: Option<String> = globals
        .get::<Option<String>>(LUA_OVERRIDE_HASH_KEY)
        .map_err(|e| anyhow!("failed to read override-hash cache: {e}"))?;
    if stored_hash.as_deref() != Some(content_hash(content).as_str()) {
        return Ok(None);
    }
    let table: Option<Table> = globals
        .get::<Option<Table>>(LUA_OVERRIDE_RESULT_KEY)
        .map_err(|e| anyhow!("failed to read override-result cache: {e}"))?;
    Ok(table)
}

/// Store a newly compiled override table in the Lua-side cache keyed by its
/// content hash. Subsequent calls with the same content reuse this exact
/// table instead of re-evaluating the chunk.
fn store_override_cache(lua: &Lua, table: &Table, content: &str) -> Result<()> {
    let globals = lua.globals();
    globals
        .set(LUA_OVERRIDE_HASH_KEY, content_hash(content))
        .map_err(|e| anyhow!("failed to write override-hash cache: {e}"))?;
    globals
        .set(LUA_OVERRIDE_RESULT_KEY, table.clone())
        .map_err(|e| anyhow!("failed to write override-result cache: {e}"))?;
    Ok(())
}

/// Clear the Lua-side override-module cache. Tests use this between runs so
/// an earlier override's compiled table doesn't leak into a fresh test.
#[cfg(test)]
pub fn _clear_lua_override_cache_for_tests(lua: &Lua) {
    let globals = lua.globals();
    let _ = globals.set(LUA_OVERRIDE_HASH_KEY, Value::Nil);
    let _ = globals.set(LUA_OVERRIDE_RESULT_KEY, Value::Nil);
}

/// Resolve an override to a compiled Lua Table. If a previous evaluation's
/// result is still cached for this exact content, reuse it; otherwise
/// evaluate the chunk and cache the result.
fn load_override_cached(lua: &Lua, path: &Path, content: &str) -> Result<Table> {
    if let Some(cached) = load_override_from_cache(lua, content)? {
        return Ok(cached);
    }
    let table = load_override_from_str(lua, path, content)?;
    store_override_cache(lua, &table, content)?;
    Ok(table)
}

/// Return the cached entry if one exists. The cache is held for the
/// lifetime of the hub and only cleared by [`reload`].
fn cache_hit() -> Option<OverrideCache> {
    let guard = OVERRIDE_CACHE.lock().ok()?;
    guard.as_ref().cloned()
}

/// Perform a full override-chain scan (one-time, on a cold cache) and load
/// the winning candidate (or the embedded default) into `lua`. After this
/// runs once, subsequent renders skip disk I/O entirely until `reload()` is
/// called.
fn scan_and_load(lua: &Lua) -> Result<Table> {
    let candidates = override_candidates();
    for candidate in candidates {
        if !candidate.is_file() {
            continue;
        }
        let content = std::fs::read_to_string(&candidate)
            .map_err(|e| anyhow!("failed to read {}: {e}", candidate.display()))?;
        let entry = CachedOverride {
            path: candidate,
            content,
        };
        let table = load_override_cached(lua, &entry.path, &entry.content)?;
        store_cache(Some(entry));
        return Ok(table);
    }

    // No override won — record the negative result so we don't re-scan on
    // every render. Cleared by `reload()` if the user adds an override file
    // later.
    store_cache(None);
    load_embedded(lua)
}

/// Persist the resolution snapshot. No expiry; `reload()` is the only path
/// that clears it.
fn store_cache(winning: Option<CachedOverride>) {
    let new_cache = OverrideCache { winning };
    match OVERRIDE_CACHE.lock() {
        Ok(mut guard) => *guard = Some(new_cache),
        Err(poisoned) => *poisoned.into_inner() = Some(new_cache),
    }
}

/// Invalidate the process-wide override cache. Tests use this between runs
/// to simulate a `reload()` from the Rust side without needing a live Lua
/// VM. Production callers use [`reload`] which also clears the Lua-side
/// caches.
#[doc(hidden)]
pub fn _clear_override_cache_for_tests() {
    match OVERRIDE_CACHE.lock() {
        Ok(mut guard) => *guard = None,
        Err(poisoned) => *poisoned.into_inner() = None,
    }
}

/// Override file paths to check, in priority order.
fn override_candidates() -> Vec<PathBuf> {
    let repo = repo_config_dir();
    let device = device_config_dir();

    let mut candidates = Vec::with_capacity(4);
    if let Some(dir) = &repo {
        candidates.push(dir.join(LAYOUT_WEB_FILE));
    }
    if let Some(dir) = &device {
        candidates.push(dir.join(LAYOUT_WEB_FILE));
    }
    if let Some(dir) = &repo {
        candidates.push(dir.join(LAYOUT_SHARED_FILE));
    }
    if let Some(dir) = &device {
        candidates.push(dir.join(LAYOUT_SHARED_FILE));
    }
    candidates
}

/// Resolve the repo-scoped config dir. Walks up from CWD (or the test-override
/// dir if set) to find `.git/`, then appends `.botster-dev/` or `.botster/`.
fn repo_config_dir() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var(REPO_DIR_OVERRIDE_ENV_VAR) {
        return Some(PathBuf::from(custom));
    }
    let start = std::env::current_dir().ok()?;
    let mut cursor: &Path = start.as_path();
    loop {
        if cursor.join(".git").exists() {
            return Some(cursor.join(config_dir_name(cursor)));
        }
        cursor = cursor.parent()?;
    }
}

/// Resolve the device-scoped config dir: `~/.botster/` (or the test override).
fn device_config_dir() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var(DEVICE_DIR_OVERRIDE_ENV_VAR) {
        return Some(PathBuf::from(custom));
    }
    let home = dirs::home_dir()?;
    Some(home.join(config_dir_name(&home)))
}

/// Pick `.botster-dev` or `.botster` for the given containing directory.
///
/// Dev mode is opt-in (`BOTSTER_DEV=1`) AND requires the `.botster-dev`
/// directory to already exist in `at`. Otherwise we fall back to the
/// production `.botster` name — which may itself be missing, in which case the
/// caller's `is_file()` check will simply skip it.
fn config_dir_name(at: &Path) -> &'static str {
    let dev_requested = std::env::var(DEV_ENV_VAR).is_ok_and(|v| v == "1");
    if dev_requested && at.join(".botster-dev").is_dir() {
        ".botster-dev"
    } else {
        ".botster"
    }
}

/// Evaluate an override `.lua` chunk (already read into memory) as a Lua
/// table. The chunk is named after the file path so stack traces point at the
/// user's override rather than an anonymous `[string "..."]` entry.
fn load_override_from_str(lua: &Lua, path: &Path, content: &str) -> Result<Table> {
    let chunk_name = format!("@{}", path.display());
    let returned: Value = lua
        .load(content)
        .set_name(chunk_name)
        .eval()
        .map_err(|e| anyhow!("failed to evaluate {}: {e}", path.display()))?;
    match returned {
        Value::Table(t) => Ok(t),
        other => Err(anyhow!(
            "{} must return a table keyed by surface name, got {}",
            path.display(),
            other.type_name()
        )),
    }
}

/// Load the embedded default via `require`. Depends on `package.searchers`
/// being configured to find either the filesystem copy (debug builds) or the
/// embedded copy (release builds via `install_embedded_searcher`).
fn load_embedded(lua: &Lua) -> Result<Table> {
    let require: Function = lua
        .globals()
        .get("require")
        .map_err(|e| anyhow!("cannot find `require` global: {e}"))?;
    let returned: Value = require
        .call(EMBEDDED_LAYOUT_MODULE)
        .map_err(|e| anyhow!("require(\"{EMBEDDED_LAYOUT_MODULE}\") failed: {e}"))?;
    match returned {
        Value::Table(t) => Ok(t),
        other => Err(anyhow!(
            "embedded `{EMBEDDED_LAYOUT_MODULE}` returned {}, expected table",
            other.type_name()
        )),
    }
}

/// Produce a minimal fallback tree when layout evaluation fails.
///
/// The shape deliberately uses only v1 primitives so the browser interpreter
/// renders something recognisable instead of erroring. This is the contract
/// the hub promises to transport: `render` returns valid `UiNodeV1` JSON even
/// on failure.
fn error_fallback_json(surface_name: &str, error_msg: &str) -> String {
    let node = serde_json::json!({
        "type": "panel",
        "props": {
            "title": format!("Layout error: {surface_name}"),
            "tone": "muted",
            "border": true,
        },
        "children": [
            {
                "type": "stack",
                "props": { "direction": "vertical", "gap": "2" },
                "children": [
                    {
                        "type": "text",
                        "props": {
                            "text": "The hub layout failed to render. Showing fallback.",
                            "tone": "danger",
                            "size": "sm",
                            "weight": "medium",
                        },
                    },
                    {
                        "type": "text",
                        "props": {
                            "text": error_msg,
                            "tone": "muted",
                            "size": "xs",
                            "monospace": true,
                        },
                    },
                ],
            }
        ],
    });
    serde_json::to_string(&node).unwrap_or_else(|_| {
        // Serialising a hand-built json! literal should never fail. If it does
        // (OOM, etc.) hand back a bare-bones valid JSON string so callers
        // don't have to branch on an empty return.
        String::from(r#"{"type":"panel","props":{"title":"Layout error"}}"#)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_fallback_is_valid_uinode_json() {
        let json = error_fallback_json("workspace_surface", "syntax error: unexpected '}'");
        let node: UiNodeV1 = serde_json::from_str(&json).expect("fallback must deserialise");
        assert_eq!(node.node_type, "panel");
        let title = node.props.get("title").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            title.contains("workspace_surface"),
            "fallback panel title must mention the surface: {title}"
        );
    }

    #[test]
    fn error_fallback_embeds_error_message() {
        let json = error_fallback_json("workspace_surface", "my-distinctive-error-abc");
        assert!(
            json.contains("my-distinctive-error-abc"),
            "fallback JSON must carry the error detail: {json}"
        );
    }

    #[test]
    fn register_exposes_render_function() {
        let lua = Lua::new();
        register(&lua).expect("register web_layout");
        let web_layout: Table = lua.globals().get("web_layout").expect("web_layout global");
        let _render: Function = web_layout.get("render").expect("web_layout.render");
    }
}
