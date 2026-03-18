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
        log::warn!(
            "[context] Failed to read session manifest at {}",
            manifest_path.display()
        );
        return BTreeMap::new();
    };

    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&content) else {
        log::warn!(
            "[context] Failed to parse session manifest at {}",
            manifest_path.display()
        );
        return BTreeMap::new();
    };

    let mut ctx = BTreeMap::new();

    // Extract well-known fields from the session manifest
    let fields = [
        ("session_uuid", "session_uuid"),
        ("agent_key", "id"),
        ("hub_id", "hub_id"),
        ("hub_manifest_path", "hub_manifest_path"),
        ("repo", "repo"),
        ("branch_name", "branch_name"),
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
    use std::sync::Mutex;

    /// Serialize all context tests — they mutate process-global env vars
    /// (BOTSTER_SESSION_UUID, BOTSTER_CONFIG_DIR) which race when tests
    /// run in parallel.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn build_returns_empty_when_session_uuid_not_set() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("BOTSTER_SESSION_UUID");
        std::env::remove_var("BOTSTER_CONFIG_DIR");
        let ctx = build();
        assert!(ctx.is_empty());
    }

    // ── Manifest fault injection tests ────────────────────────────────────

    /// Helper: create a temp workspace store with a session manifest.
    /// Returns (temp_dir, session_uuid) so BOTSTER_CONFIG_DIR can be pointed
    /// at the temp dir.
    fn create_manifest(content: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::TempDir::new().unwrap();
        let uuid = "sess-test-0001-deadbeef";
        let ws_id = "ws-test-001";
        let manifest_dir = dir
            .path()
            .join("workspaces")
            .join(ws_id)
            .join("sessions")
            .join(uuid);
        std::fs::create_dir_all(&manifest_dir).unwrap();
        std::fs::write(manifest_dir.join("manifest.json"), content).unwrap();
        (dir, uuid.to_string())
    }

    /// Helper: set env vars and build context, holding the env lock.
    fn build_with_manifest(content: &str) -> (BTreeMap<String, String>, tempfile::TempDir) {
        let (dir, uuid) = create_manifest(content);
        std::env::set_var("BOTSTER_SESSION_UUID", &uuid);
        std::env::set_var("BOTSTER_CONFIG_DIR", dir.path());
        let ctx = build();
        (ctx, dir) // return dir to keep TempDir alive
    }

    /// A well-formed manifest should populate all context fields.
    #[test]
    fn build_extracts_fields_from_valid_manifest() {
        let _lock = ENV_LOCK.lock().unwrap();
        let (ctx, _dir) = build_with_manifest(
            r#"{
            "session_uuid": "sess-test-0001-deadbeef",
            "id": "my-agent",
            "hub_id": "hub-abc",
            "hub_manifest_path": "/data/hubs/abc/manifest.json",
            "repo": "owner/repo",
            "branch_name": "feature-x",
            "worktree_path": "/tmp/wt",
            "agent_name": "Claude",
            "workspace_id": "ws-test-001"
        }"#,
        );

        assert_eq!(
            ctx.get("session_uuid").map(String::as_str),
            Some("sess-test-0001-deadbeef")
        );
        assert_eq!(ctx.get("agent_key").map(String::as_str), Some("my-agent"));
        assert_eq!(ctx.get("hub_id").map(String::as_str), Some("hub-abc"));
        assert_eq!(ctx.get("repo").map(String::as_str), Some("owner/repo"));
        assert_eq!(
            ctx.get("branch_name").map(String::as_str),
            Some("feature-x")
        );
    }

    /// A manifest with missing fields should produce a partial context,
    /// not an error. Stale manifests from older versions may lack fields
    /// that were added later.
    #[test]
    fn build_handles_stale_manifest_missing_fields() {
        let _lock = ENV_LOCK.lock().unwrap();
        let (ctx, _dir) = build_with_manifest(
            r#"{
            "session_uuid": "sess-test-0001-deadbeef",
            "id": "old-agent"
        }"#,
        );

        // Present field should be extracted.
        assert_eq!(ctx.get("agent_key").map(String::as_str), Some("old-agent"));
        // Missing fields should just be absent, not error.
        assert!(ctx.get("hub_id").is_none());
        assert!(ctx.get("repo").is_none());
        assert!(ctx.get("branch_name").is_none());
    }

    /// A manifest with unknown extra fields (from a newer version) must
    /// not cause a parse failure. Forward compatibility.
    #[test]
    fn build_handles_manifest_with_unknown_fields() {
        let _lock = ENV_LOCK.lock().unwrap();
        let (ctx, _dir) = build_with_manifest(
            r#"{
            "session_uuid": "sess-test-0001-deadbeef",
            "id": "future-agent",
            "hub_id": "hub-xyz",
            "future_field_v99": "some value",
            "another_new_thing": 42,
            "nested_new": {"deep": true}
        }"#,
        );

        assert_eq!(
            ctx.get("agent_key").map(String::as_str),
            Some("future-agent")
        );
        assert_eq!(ctx.get("hub_id").map(String::as_str), Some("hub-xyz"));
    }

    /// A completely corrupt (non-JSON) manifest must not panic.
    #[test]
    fn build_handles_corrupt_manifest_gracefully() {
        let _lock = ENV_LOCK.lock().unwrap();
        let (ctx, _dir) = build_with_manifest("this is not json at all {{{}}}");
        assert!(
            ctx.is_empty(),
            "corrupt manifest must produce empty context, not panic"
        );
    }

    /// An empty manifest file must not panic.
    #[test]
    fn build_handles_empty_manifest_file() {
        let _lock = ENV_LOCK.lock().unwrap();
        let (ctx, _dir) = build_with_manifest("");
        assert!(ctx.is_empty(), "empty manifest must produce empty context");
    }

    /// A manifest with empty string values should not include them in context.
    #[test]
    fn build_skips_empty_string_values() {
        let _lock = ENV_LOCK.lock().unwrap();
        let (ctx, _dir) = build_with_manifest(
            r#"{
            "session_uuid": "sess-test-0001-deadbeef",
            "id": "",
            "hub_id": "hub-ok",
            "repo": ""
        }"#,
        );

        assert!(
            ctx.get("agent_key").is_none(),
            "empty string should be skipped"
        );
        assert_eq!(ctx.get("hub_id").map(String::as_str), Some("hub-ok"));
        assert!(ctx.get("repo").is_none(), "empty string should be skipped");
    }

    /// Metadata fields should be flattened into context but not override
    /// well-known fields.
    #[test]
    fn build_flattens_metadata_without_overriding() {
        let _lock = ENV_LOCK.lock().unwrap();
        let (ctx, _dir) = build_with_manifest(
            r#"{
            "session_uuid": "sess-test-0001-deadbeef",
            "hub_id": "real-hub",
            "metadata": {
                "hub_id": "should-not-override",
                "issue_number": 42,
                "invocation_url": "https://github.com/owner/repo/issues/42"
            }
        }"#,
        );

        assert_eq!(ctx.get("hub_id").map(String::as_str), Some("real-hub"));
        assert_eq!(ctx.get("issue_number").map(String::as_str), Some("42"));
        assert_eq!(
            ctx.get("invocation_url").map(String::as_str),
            Some("https://github.com/owner/repo/issues/42")
        );
    }

    /// A session UUID that doesn't match any manifest should produce
    /// empty context, not an error.
    #[test]
    fn build_handles_nonexistent_session_uuid() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("workspaces").join("ws-1")).unwrap();
        std::env::set_var("BOTSTER_SESSION_UUID", "sess-does-not-exist");
        std::env::set_var("BOTSTER_CONFIG_DIR", dir.path());

        let ctx = build();
        assert!(ctx.is_empty());
    }
}
