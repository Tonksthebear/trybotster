//! Agent context resolution command.
//!
//! Merges agent-level identity (env vars) with worktree-level metadata
//! (`.botster/context.json`) into a flat namespace. Used by session init
//! scripts to access task context without manual JSON parsing.
//!
//! # Sources (higher priority wins)
//!
//! 1. Worktree `.botster/context.json` — repo, branch_name, prompt, metadata.*
//! 2. Agent env vars — agent_key, hub_id, worktree_path
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
    ("worktree_path", "BOTSTER_WORKTREE_PATH"),
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
    let mut ctx = BTreeMap::new();

    // 1. Load worktree-level context (lower priority, loaded first)
    if let Some(worktree_ctx) = load_context_json() {
        // Top-level string fields
        for field in &["repo", "branch_name", "prompt"] {
            if let Some(val) = worktree_ctx.get(*field).and_then(|v| v.as_str()) {
                if !val.is_empty() {
                    ctx.insert((*field).to_string(), val.to_string());
                }
            }
        }

        // Flatten metadata into top-level namespace
        if let Some(metadata) = worktree_ctx.get("metadata").and_then(|v| v.as_object()) {
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

/// Load `.botster/context.json` from the current directory.
///
/// Returns `None` if the file doesn't exist or can't be parsed.
fn load_context_json() -> Option<serde_json::Value> {
    let path = std::env::current_dir().ok()?.join(".botster/context.json");
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}
