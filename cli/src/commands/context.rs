//! Agent context resolution command.
//!
//! Derives all agent identity from `BOTSTER_SESSION_UUID`. The session manifest
//! in the workspace store (`~/.botster{-dev}/workspaces/*/sessions/{uuid}/manifest.json`)
//! is the single source of truth.
//!
//! # Examples
//!
//! ```bash
//! botster context agent_key       # prints agent key
//! botster context hub_id          # prints hub ID
//! botster context                 # dumps all context as JSON
//! ```

use anyhow::Result;
use std::collections::BTreeMap;

/// Run the context command.
///
/// If `key` is `Some`, prints the value for that key (or empty line if not found).
/// If `key` is `None`, dumps all available context as JSON.
pub fn run(key: Option<&str>) -> Result<()> {
    if std::env::var("BOTSTER_SESSION_UUID")
        .ok()
        .filter(|s| !s.is_empty())
        .is_none()
    {
        anyhow::bail!("BOTSTER_SESSION_UUID is required for botster context");
    }

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

/// Build the context map from the session manifest.
///
/// Requires `BOTSTER_SESSION_UUID` to be set. Loads the session manifest from
/// the workspace store and extracts agent identity fields.
///
/// Public so `mcp_gateway` can pass the full context at subscribe time.
pub fn build() -> BTreeMap<String, String> {
    let Some(session_uuid) = std::env::var("BOTSTER_SESSION_UUID")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        log::warn!("[context] BOTSTER_SESSION_UUID not set");
        return BTreeMap::new();
    };

    let Some(manifest_path) = crate::env::session_manifest_path(&session_uuid) else {
        log::warn!("[context] Session manifest not found for {session_uuid}");
        return BTreeMap::new();
    };

    let Ok(content) = std::fs::read_to_string(&manifest_path) else {
        log::warn!("[context] Failed to read session manifest at {}", manifest_path.display());
        return BTreeMap::new();
    };

    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
        log::warn!("[context] Failed to parse session manifest at {}", manifest_path.display());
        return BTreeMap::new();
    };

    let mut ctx = BTreeMap::new();

    // Extract well-known fields from the session manifest
    let fields = [
        ("session_uuid", "uuid"),
        ("agent_key", "agent_key"),
        ("hub_id", "hub_id"),
        ("hub_manifest_path", "hub_manifest_path"),
        ("repo", "repo"),
        ("branch_name", "branch"),
        ("worktree_path", "worktree_path"),
        ("agent_name", "agent_name"),
        ("workspace_id", "workspace_id"),
        ("prompt", "prompt"),
    ];

    for (ctx_key, manifest_key) in &fields {
        if let Some(val) = manifest.get(*manifest_key).and_then(|v| v.as_str()) {
            if !val.is_empty() {
                ctx.insert((*ctx_key).to_string(), val.to_string());
            }
        }
    }

    // Flatten metadata keys into context so plugins can store arbitrary
    // values (e.g. issue_number, invocation_url) accessible via
    // `botster context <key>`. Metadata values don't override well-known fields.
    if let Some(metadata) = manifest.get("metadata").and_then(|v| v.as_object()) {
        for (key, value) in metadata {
            if !ctx.contains_key(key) {
                if let Some(s) = value.as_str() {
                    if !s.is_empty() {
                        ctx.insert(key.clone(), s.to_string());
                    }
                } else if let Some(n) = value.as_i64() {
                    ctx.insert(key.clone(), n.to_string());
                } else if let Some(n) = value.as_u64() {
                    ctx.insert(key.clone(), n.to_string());
                }
            }
        }
    }

    ctx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_returns_empty_when_session_uuid_not_set() {
        // BOTSTER_SESSION_UUID should not be set in test env
        std::env::remove_var("BOTSTER_SESSION_UUID");
        let ctx = build();
        assert!(ctx.is_empty());
    }
}
