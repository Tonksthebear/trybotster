//! Agent context resolution command.
//!
//! Merges agent-level identity (env vars) with worktree-level metadata
//! (`.botster/context.json`) into a flat namespace. Used by session init
//! scripts to access task context without manual JSON parsing.
//!
//! # Sources (higher priority wins)
//!
//! 1. Worktree `.botster/context.json` — repo, branch_name, prompt, metadata.*
//! 2. Agent env vars — agent_key, hub_id, hub_socket, hub_manifest_path, worktree_path
//!
//! # Examples
//!
//! ```bash
//! botster context agent_key       # prints agent key
//! botster context issue_number    # reads from metadata in context.json
//! botster context prompt          # reads prompt from context.json
//! botster context                 # dumps all context as JSON
//! ```

use anyhow::Result;
use std::collections::BTreeMap;

/// Well-known agent-level keys sourced from environment variables.
const AGENT_ENV_KEYS: &[(&str, &str)] = &[
    ("agent_key", "BOTSTER_AGENT_KEY"),
    ("hub_id", "BOTSTER_HUB_ID"),
    ("hub_socket", "BOTSTER_HUB_SOCKET"),
    ("hub_manifest_path", "BOTSTER_HUB_MANIFEST_PATH"),
    ("worktree_path", "BOTSTER_WORKTREE_PATH"),
    ("prompt", "BOTSTER_PROMPT"),
];

/// Run the context command.
///
/// If `key` is `Some`, prints the value for that key (or empty line if not found).
/// If `key` is `None`, dumps all available context as JSON.
pub fn run(key: Option<&str>) -> Result<()> {
    let context = build();

    match key {
        Some(k) => {
            if let Some(value) = context.get(k) {
                println!("{value}");
            }
        }
        None => {
            println!("{}", serde_json::to_string_pretty(&context)?);
        }
    }
    Ok(())
}

/// Build the merged context map.
///
/// Priority: agent-level (env vars) > worktree-level (context.json).
/// Metadata keys from context.json are flattened into the top-level namespace.
///
/// Public so `mcp_serve` can pass the full context at subscribe time.
pub fn build() -> BTreeMap<String, String> {
    build_from_json(load_context_json())
}

/// Core merge logic, separated from file I/O so tests can inject JSON directly.
fn build_from_json(worktree_ctx: Option<serde_json::Value>) -> BTreeMap<String, String> {
    let mut ctx = BTreeMap::new();

    // 1. Load worktree-level context (lower priority, loaded first)
    if let Some(ref json) = worktree_ctx {
        // Top-level string fields
        for field in &["repo", "branch_name", "prompt", "hub_socket", "hub_manifest_path"] {
            if let Some(val) = json.get(*field).and_then(|v| v.as_str()) {
                if !val.is_empty() {
                    ctx.insert((*field).to_string(), val.to_string());
                }
            }
        }

        // Flatten metadata into top-level namespace
        if let Some(metadata) = json.get("metadata").and_then(|v| v.as_object()) {
            for (k, v) in metadata {
                let str_val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Null => continue,
                    other => other.to_string(),
                };
                if !str_val.is_empty() {
                    ctx.insert(k.clone(), str_val);
                }
            }
        }
    }

    // 2. Overlay agent-level env vars (higher priority)
    for (key, env_var) in AGENT_ENV_KEYS {
        if let Ok(val) = std::env::var(env_var) {
            if !val.is_empty() {
                ctx.insert((*key).to_string(), val);
            }
        }
    }

    ctx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize env-var-touching tests to prevent cross-test races.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn json(s: &str) -> Option<serde_json::Value> {
        serde_json::from_str(s).ok()
    }

    #[test]
    fn hub_socket_from_context_json_when_env_absent() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("BOTSTER_HUB_SOCKET");

        let ctx = build_from_json(json(
            r#"{"hub_socket": "/tmp/botster-1000/abc123.sock"}"#,
        ));

        assert_eq!(
            ctx.get("hub_socket").map(String::as_str),
            Some("/tmp/botster-1000/abc123.sock")
        );
    }

    #[test]
    fn env_var_wins_over_context_json_hub_socket() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("BOTSTER_HUB_SOCKET", "/tmp/botster-1000/from-env.sock");

        let ctx = build_from_json(json(
            r#"{"hub_socket": "/tmp/botster-1000/from-file.sock"}"#,
        ));

        std::env::remove_var("BOTSTER_HUB_SOCKET");

        assert_eq!(
            ctx.get("hub_socket").map(String::as_str),
            Some("/tmp/botster-1000/from-env.sock")
        );
    }

    #[test]
    fn empty_hub_socket_values_are_skipped() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("BOTSTER_HUB_SOCKET");

        // Empty string in context.json must not insert the key
        let ctx_file_empty = build_from_json(json(r#"{"hub_socket": ""}"#));
        assert!(!ctx_file_empty.contains_key("hub_socket"));

        // Empty env var must not insert the key
        std::env::set_var("BOTSTER_HUB_SOCKET", "");
        let ctx_env_empty =
            build_from_json(json(r#"{"hub_socket": "/tmp/botster-1000/abc123.sock"}"#));
        std::env::remove_var("BOTSTER_HUB_SOCKET");

        // Empty env must not overwrite a valid context.json value
        assert_eq!(
            ctx_env_empty.get("hub_socket").map(String::as_str),
            Some("/tmp/botster-1000/abc123.sock")
        );
    }
}

/// Load `.botster/context.json` from the current directory.
///
/// Returns `None` if the file doesn't exist or can't be parsed.
fn load_context_json() -> Option<serde_json::Value> {
    let path = std::env::current_dir().ok()?.join(".botster/context.json");
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}
