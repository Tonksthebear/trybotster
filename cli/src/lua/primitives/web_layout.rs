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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

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

/// Default TTL (milliseconds) for the successful override-chain scan cache.
///
/// Phase 2a flagged that `web_layout.render` stats four candidates on every
/// call. Phase 2b broadcasts trigger that path on every state change, so a
/// per-render TTL collapses bursts of renders into at most one stat set per
/// window. 500 ms balances staleness on edits vs. re-stat chatter.
///
/// Override at runtime via [`set_override_cache_ttl_millis`]; tests use a 0
/// TTL so sequential file writes are observed immediately.
const DEFAULT_OVERRIDE_CACHE_TTL_MILLIS: u64 = 500;

/// Install `web_layout` as a global Lua table with one method: `render`.
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

    lua.globals()
        .set("web_layout", table)
        .map_err(|e| anyhow!("Failed to register web_layout global: {e}"))?;

    log::debug!("Registered web_layout primitive");
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
/// Phase 2a's render path stats four candidate files unconditionally; Phase
/// 2b broadcasts trigger that path on every state change, which can fire many
/// times per second. The cache bounds the common case to at most one scan per
/// TTL window while still picking up edits promptly.
static OVERRIDE_CACHE: Mutex<Option<OverrideCache>> = Mutex::new(None);

/// Current TTL for the cache, in milliseconds. Adjustable at runtime via
/// [`set_override_cache_ttl_millis`] for tests that need to observe
/// sequential file writes without waiting 500 ms between them.
static OVERRIDE_CACHE_TTL_MILLIS: AtomicU64 = AtomicU64::new(DEFAULT_OVERRIDE_CACHE_TTL_MILLIS);

fn override_cache_ttl() -> Duration {
    Duration::from_millis(OVERRIDE_CACHE_TTL_MILLIS.load(Ordering::Relaxed))
}

/// Snapshot of the override-chain resolution, guarded by `OVERRIDE_CACHE`.
///
/// `valid_until` is the wall-clock deadline after which the cache MUST be
/// refreshed; `winning` records which candidate (if any) won and the mtime it
/// had at scan time. A matching mtime on refresh lets us skip the disk read
/// and reuse the cached `content`. A mismatch (or a newly-present higher-
/// priority override) invalidates the entry.
#[derive(Clone)]
struct OverrideCache {
    valid_until: Instant,
    winning: Option<CachedOverride>,
}

/// Cached payload for one override file: source text plus the mtime observed
/// when the text was read. Stored as `String` (not `Table`) because Lua tables
/// can't outlive their parent `Lua`; re-evaluating a cached string on the
/// caller's Lua VM is cheap compared to a fresh disk read.
#[derive(Clone)]
struct CachedOverride {
    path: PathBuf,
    mtime: SystemTime,
    content: String,
}

/// Walk the resolution chain and return the first layout table that loads
/// successfully. Falls back to the embedded default via `require`.
///
/// Hot path:
/// - within `OVERRIDE_CACHE_TTL` of the last successful scan, the cache is
///   reused verbatim (zero stats, cached content re-loaded into `lua`);
/// - otherwise, each candidate is stat-ed to check for mtime drift. A cached
///   winning override whose mtime is unchanged short-circuits the read.
fn resolve_layout_table(lua: &Lua) -> Result<Table> {
    if let Some(cached) = cache_hit() {
        match cached.winning {
            Some(entry) => return load_override_from_str(lua, &entry.path, &entry.content),
            None => return load_embedded(lua),
        }
    }
    scan_and_load(lua)
}

/// Return the cached entry if it is still within its TTL window. Expiry is
/// resolved eagerly: the returned value is an owned snapshot so the lock is
/// released before we touch Lua.
fn cache_hit() -> Option<OverrideCache> {
    let guard = OVERRIDE_CACHE.lock().ok()?;
    let cached = guard.as_ref()?;
    if Instant::now() >= cached.valid_until {
        return None;
    }
    Some(cached.clone())
}

/// Perform a full override-chain scan, update the cache, and load the winning
/// candidate (or the embedded default) into `lua`.
fn scan_and_load(lua: &Lua) -> Result<Table> {
    let candidates = override_candidates();

    // Seed the fresh scan with whatever we have cached for each candidate
    // path. If a candidate's mtime still matches the cache we skip re-reading
    // its content; otherwise we pull fresh bytes off disk.
    let prior_by_path = prior_cache_by_path();

    for candidate in candidates {
        match candidate_mtime(&candidate) {
            None => continue, // not a file — skip.
            Some(mtime) => {
                let entry = match prior_by_path.get(&candidate) {
                    Some(prev) if prev.mtime == mtime => CachedOverride {
                        path: candidate.clone(),
                        mtime,
                        content: prev.content.clone(),
                    },
                    _ => {
                        let content = std::fs::read_to_string(&candidate).map_err(|e| {
                            anyhow!("failed to read {}: {e}", candidate.display())
                        })?;
                        CachedOverride {
                            path: candidate.clone(),
                            mtime,
                            content,
                        }
                    }
                };
                let table = load_override_from_str(lua, &entry.path, &entry.content)?;
                store_cache(Some(entry));
                return Ok(table);
            }
        }
    }

    // No override won — cache the negative result so subsequent renders in
    // this TTL window skip stat-ing the same four paths.
    store_cache(None);
    load_embedded(lua)
}

/// Build a fast lookup of the prior cache keyed by path. Used so a partial
/// rescan (e.g. a newly-added higher-priority override) can still reuse
/// already-read bytes for unchanged lower-priority files.
fn prior_cache_by_path() -> std::collections::HashMap<PathBuf, CachedOverride> {
    let guard = match OVERRIDE_CACHE.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut out = std::collections::HashMap::new();
    if let Some(cache) = guard.as_ref() {
        if let Some(entry) = &cache.winning {
            out.insert(entry.path.clone(), entry.clone());
        }
    }
    out
}

/// Persist the resolution snapshot and set the next TTL deadline.
fn store_cache(winning: Option<CachedOverride>) {
    let new_cache = OverrideCache {
        valid_until: Instant::now() + override_cache_ttl(),
        winning,
    };
    match OVERRIDE_CACHE.lock() {
        Ok(mut guard) => *guard = Some(new_cache),
        Err(poisoned) => *poisoned.into_inner() = Some(new_cache),
    }
}

/// Adjust the override-chain cache TTL. Tests that exercise sequential file
/// edits set this to 0 so each render re-scans the filesystem; production
/// code never needs to call it. Returns the previous value so test harnesses
/// can restore it after use.
#[doc(hidden)]
pub fn set_override_cache_ttl_millis(ttl_millis: u64) -> u64 {
    OVERRIDE_CACHE_TTL_MILLIS.swap(ttl_millis, Ordering::Relaxed)
}

/// Stat helper — returns `Some(mtime)` iff `path` is a regular file. Any I/O
/// error (including "not found") yields `None` so the caller can skip.
fn candidate_mtime(path: &Path) -> Option<SystemTime> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    meta.modified().ok()
}

/// Invalidate the process-wide override cache. Tests use this between runs;
/// production never needs it because each render's TTL window expires on its
/// own. Exposed `pub` only because `tests/` and integration test binaries
/// live outside the crate root.
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
