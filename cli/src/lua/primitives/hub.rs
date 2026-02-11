//! Hub state primitives for Lua scripts.
//!
//! Exposes Hub state queries and operations to Lua, allowing scripts to
//! inspect worktrees, register/unregister agents, request lifecycle operations,
//! and initiate WebRTC signaling.
//!
//! # Design Principle: "Query freely. Mutate via queue."
//!
//! - **State queries** (`get_worktrees`, `server_id`, `detect_repo`)
//!   read directly from shared state or environment
//! - **Registration** (`register_agent`, `unregister_agent`) manages PTY handles
//! - **Operations** (`quit`, `handle_webrtc_offer`, `handle_ice_candidate`)
//!   queue requests for Hub to process asynchronously
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Get available worktrees
//! local worktrees = hub.get_worktrees()
//!
//! -- Register agent PTY handles
//! local index = hub.register_agent("owner-repo-42", sessions)
//!
//! -- Get server-assigned hub ID
//! local id = hub.server_id()
//!
//! -- Detect current repo (owner/name format)
//! local repo = hub.detect_repo()
//!
//! -- Handle WebRTC signaling
//! hub.handle_webrtc_offer(browser_identity, sdp)
//! hub.handle_ice_candidate(browser_identity, candidate_data)
//!
//! -- Request Hub shutdown
//! hub.quit()
//! ```

use std::sync::{Arc, Mutex, RwLock};

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use crate::hub::handle_cache::HandleCache;
use crate::hub::state::HubState;

/// Hub operation requests queued from Lua.
///
/// These are processed by Hub in its event loop after Lua callbacks return.
#[derive(Debug, Clone)]
pub enum HubRequest {
    /// Request Hub shutdown.
    Quit,
    /// Handle an incoming WebRTC SDP offer from a browser.
    HandleWebrtcOffer {
        /// Browser identity key (e.g., `identityKey:tabId`).
        browser_identity: String,
        /// SDP offer string.
        sdp: String,
    },
    /// Add an ICE candidate to an existing WebRTC peer connection.
    HandleIceCandidate {
        /// Browser identity key (e.g., `identityKey:tabId`).
        browser_identity: String,
        /// ICE candidate data as JSON value.
        candidate: serde_json::Value,
    },
}

/// Shared request queue for Hub operations from Lua.
pub type HubRequestQueue = Arc<Mutex<Vec<HubRequest>>>;

/// Server-assigned hub ID, shared between Hub and Lua primitives.
pub type SharedServerId = Arc<Mutex<Option<String>>>;

/// Create a new Hub request queue.
#[must_use]
pub fn new_request_queue() -> HubRequestQueue {
    Arc::new(Mutex::new(Vec::new()))
}

/// Register Hub state primitives with the Lua state.
///
/// Adds the following functions to the `hub` table:
/// - `hub.get_worktrees()` - Get available worktrees
/// - `hub.register_agent(key, sessions)` - Register agent PTY handles
/// - `hub.unregister_agent(key)` - Unregister agent PTY handles
/// - `hub.server_id()` - Get server-assigned hub ID
/// - `hub.detect_repo()` - Detect current repo name
/// - `hub.api_token()` - Get hub's API bearer token for authenticated requests
/// - `hub.handle_webrtc_offer(browser_identity, sdp)` - Queue WebRTC offer
/// - `hub.handle_ice_candidate(browser_identity, candidate)` - Queue ICE candidate
/// - `hub.quit()` - Request Hub shutdown
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `request_queue` - Shared queue for Hub operations (processed by Hub)
/// * `handle_cache` - Thread-safe cache of agent handles for queries
/// * `server_id` - Server-assigned hub ID (set after registration)
/// * `shared_state` - Shared hub state for agent queries
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(
    lua: &Lua,
    request_queue: HubRequestQueue,
    handle_cache: Arc<HandleCache>,
    server_id: SharedServerId,
    _shared_state: Arc<RwLock<HubState>>,
) -> Result<()> {
    // Get or create the hub table
    let hub: LuaTable = lua
        .globals()
        .get("hub")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    // hub.get_worktrees() - Returns array of available worktrees
    // Uses serde serialization to ensure proper JSON array format
    let cache = Arc::clone(&handle_cache);
    let get_worktrees_fn = lua
        .create_function(move |lua, ()| {
            let worktrees = cache.get_worktrees();

            // Build as Vec for proper array serialization
            let worktrees_data: Vec<serde_json::Value> = worktrees
                .iter()
                .map(|(path, branch)| {
                    serde_json::json!({
                        "path": path,
                        "branch": branch
                    })
                })
                .collect();

            // Convert to Lua - Vec serializes as array
            lua.to_value(&worktrees_data)
        })
        .map_err(|e| anyhow!("Failed to create hub.get_worktrees function: {e}"))?;

    hub.set("get_worktrees", get_worktrees_fn)
        .map_err(|e| anyhow!("Failed to set hub.get_worktrees: {e}"))?;

    // hub.register_agent(agent_key, sessions) - Register agent PTY handles
    //
    // Called by Lua Agent class to register PTY session handles with
    // HandleCache, enabling Rust-side PTY operations (forwarders, write, resize).
    //
    // Arguments:
    //   agent_key: string - Agent key (e.g., "owner-repo-42")
    //   sessions: array - Ordered Lua array of PtySessionHandle userdata
    //                     Index order determines PTY index (agent=0, then alphabetical)
    let cache2 = Arc::clone(&handle_cache);
    let register_agent_fn = lua
        .create_function(move |_, (agent_key, sessions): (String, LuaTable)| {
            use crate::hub::agent_handle::{AgentPtys, PtyHandle};
            use crate::lua::primitives::pty::PtySessionHandle;

            let mut pty_handles: Vec<PtyHandle> = Vec::new();

            // Iterate ordered Lua array (1-based indices)
            for i in 1..=sessions.raw_len() {
                if let Ok(ud) = sessions.get::<LuaAnyUserData>(i) {
                    match ud.borrow::<PtySessionHandle>() {
                        Ok(handle) => {
                            pty_handles.push(handle.to_pty_handle());
                            log::debug!(
                                "[Lua] Extracted PTY handle at index {} for '{}'",
                                i - 1, agent_key
                            );
                        }
                        Err(e) => {
                            log::warn!(
                                "[Lua] Failed to borrow PTY session at index {} for '{}': {}",
                                i, agent_key, e
                            );
                        }
                    }
                }
            }

            if pty_handles.is_empty() {
                log::error!(
                    "[Lua] register_agent '{}' failed: no valid PTY sessions found in array",
                    agent_key
                );
                return Err(LuaError::runtime(
                    "register_agent requires at least one PTY session"
                ));
            }

            let pty_count = pty_handles.len();
            let agent_index = cache2.len();
            let handle = AgentPtys::new(agent_key.clone(), pty_handles, agent_index);

            match cache2.add_agent(handle) {
                Some(idx) => {
                    log::info!("[Lua] Registered agent '{}' at index {} with {} PTY(s)",
                        agent_key, idx, pty_count);
                    Ok(idx)
                }
                None => Err(LuaError::runtime("Failed to register agent with HandleCache")),
            }
        })
        .map_err(|e| anyhow!("Failed to create hub.register_agent function: {e}"))?;

    hub.set("register_agent", register_agent_fn)
        .map_err(|e| anyhow!("Failed to set hub.register_agent: {e}"))?;

    // hub.unregister_agent(agent_key) - Unregister agent PTY handles
    //
    // Called by Lua when an agent is closed to remove it from HandleCache.
    let cache3 = Arc::clone(&handle_cache);
    let unregister_agent_fn = lua
        .create_function(move |_, agent_key: String| {
            let removed = cache3.remove_agent(&agent_key);
            if removed {
                log::info!("[Lua] Unregistered agent '{}'", agent_key);
            }
            Ok(removed)
        })
        .map_err(|e| anyhow!("Failed to create hub.unregister_agent function: {e}"))?;

    hub.set("unregister_agent", unregister_agent_fn)
        .map_err(|e| anyhow!("Failed to set hub.unregister_agent: {e}"))?;

    // hub.server_id() - Returns the server-assigned hub ID, or nil if not yet registered.
    let sid = Arc::clone(&server_id);
    let server_id_fn = lua
        .create_function(move |_, ()| {
            let guard = sid.lock().expect("Server ID mutex poisoned");
            Ok(guard.clone())
        })
        .map_err(|e| anyhow!("Failed to create hub.server_id function: {e}"))?;

    hub.set("server_id", server_id_fn)
        .map_err(|e| anyhow!("Failed to set hub.server_id: {e}"))?;

    // hub.detect_repo() - Detects repo name from BOTSTER_REPO env var or git remote.
    //
    // Returns the repo name in "owner/name" format, or nil if detection fails.
    let detect_repo_fn = lua
        .create_function(move |_, ()| {
            // Check env var first (explicit override)
            if let Ok(repo) = std::env::var("BOTSTER_REPO") {
                return Ok(Some(repo));
            }
            // Fall back to git remote detection
            match crate::git::WorktreeManager::detect_current_repo() {
                Ok((_path, name)) => Ok(Some(name)),
                Err(_) => Ok(None),
            }
        })
        .map_err(|e| anyhow!("Failed to create hub.detect_repo function: {e}"))?;

    hub.set("detect_repo", detect_repo_fn)
        .map_err(|e| anyhow!("Failed to set hub.detect_repo: {e}"))?;

    // hub.api_token() - Returns the hub's API bearer token from the keyring.
    //
    // Plugins use this to make authenticated HTTP requests to the Rails server.
    // The token stays within the hub process — plugins should fetch scoped
    // tokens (e.g., MCP tokens) for passing to agents.
    let api_token_fn = lua
        .create_function(|_, ()| {
            let token = crate::keyring::Credentials::load()
                .ok()
                .and_then(|c| c.api_token().map(String::from));
            Ok(token)
        })
        .map_err(|e| anyhow!("Failed to create hub.api_token function: {e}"))?;

    hub.set("api_token", api_token_fn)
        .map_err(|e| anyhow!("Failed to set hub.api_token: {e}"))?;

    // hub.handle_webrtc_offer(browser_identity, sdp) - Queue a WebRTC SDP offer for processing.
    let queue_offer = Arc::clone(&request_queue);
    let handle_webrtc_offer_fn = lua
        .create_function(move |_, (browser_identity, sdp): (String, String)| {
            let mut q = queue_offer.lock().expect("Hub request queue mutex poisoned");
            q.push(HubRequest::HandleWebrtcOffer {
                browser_identity,
                sdp,
            });
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.handle_webrtc_offer function: {e}"))?;

    hub.set("handle_webrtc_offer", handle_webrtc_offer_fn)
        .map_err(|e| anyhow!("Failed to set hub.handle_webrtc_offer: {e}"))?;

    // hub.handle_ice_candidate(browser_identity, candidate_data) - Queue an ICE candidate.
    //
    // `candidate_data` is a Lua table with `candidate`, `sdpMid`, and `sdpMLineIndex` fields,
    // matching the JSON structure from browser WebRTC signaling.
    let queue_ice = Arc::clone(&request_queue);
    let handle_ice_candidate_fn = lua
        .create_function(move |lua, (browser_identity, candidate_data): (String, LuaValue)| {
            let candidate: serde_json::Value = lua.from_value(candidate_data)?;
            let mut q = queue_ice.lock().expect("Hub request queue mutex poisoned");
            q.push(HubRequest::HandleIceCandidate {
                browser_identity,
                candidate,
            });
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.handle_ice_candidate function: {e}"))?;

    hub.set("handle_ice_candidate", handle_ice_candidate_fn)
        .map_err(|e| anyhow!("Failed to set hub.handle_ice_candidate: {e}"))?;

    // hub.quit() - Request Hub shutdown
    let queue3 = request_queue;
    let quit_fn = lua
        .create_function(move |_, ()| {
            let mut q = queue3.lock()
                .expect("Hub request queue mutex poisoned");
            q.push(HubRequest::Quit);
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.quit function: {e}"))?;

    hub.set("quit", quit_fn)
        .map_err(|e| anyhow!("Failed to set hub.quit: {e}"))?;

    // Ensure hub table is globally registered
    lua.globals()
        .set("hub", hub)
        .map_err(|e| anyhow!("Failed to register hub table globally: {e}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_deps() -> (
        HubRequestQueue,
        Arc<HandleCache>,
        SharedServerId,
        Arc<RwLock<HubState>>,
    ) {
        let state = Arc::new(RwLock::new(HubState::new(
            std::path::PathBuf::from("/tmp/test-worktrees"),
        )));
        (
            new_request_queue(),
            Arc::new(HandleCache::new()),
            Arc::new(Mutex::new(Some("test-hub-id".to_string()))),
            state,
        )
    }

    #[test]
    fn test_register_creates_hub_table() {
        let lua = Lua::new();
        let (queue, cache, sid, state) = create_test_deps();

        register(&lua, queue, cache, sid, state).expect("Should register hub primitives");

        let hub: LuaTable = lua.globals().get("hub").expect("hub table should exist");
        assert!(hub.contains_key("get_worktrees").unwrap());
        assert!(hub.contains_key("register_agent").unwrap());
        assert!(hub.contains_key("unregister_agent").unwrap());
        assert!(hub.contains_key("server_id").unwrap());
        assert!(hub.contains_key("detect_repo").unwrap());
        assert!(hub.contains_key("handle_webrtc_offer").unwrap());
        assert!(hub.contains_key("handle_ice_candidate").unwrap());
        assert!(hub.contains_key("quit").unwrap());
    }

    #[test]
    fn test_get_worktrees_returns_empty_array() {
        let lua = Lua::new();
        let (queue, cache, sid, state) = create_test_deps();

        register(&lua, queue, cache, sid, state).expect("Should register");

        let worktrees: LuaTable = lua.load("return hub.get_worktrees()").eval().unwrap();
        assert_eq!(worktrees.len().unwrap(), 0);
    }

    #[test]
    fn test_get_worktrees_serializes_as_json_array() {
        let lua = Lua::new();
        let (queue, cache, sid, state) = create_test_deps();

        register(&lua, queue, cache, sid, state).expect("Should register");

        // Get worktrees and convert back to JSON to verify array format
        let worktrees: LuaValue = lua.load("return hub.get_worktrees()").eval().unwrap();
        let json: serde_json::Value = lua.from_value(worktrees).unwrap();

        // Empty worktrees should be an array [], not an object {}
        assert!(json.is_array(), "Empty worktrees should serialize as JSON array, got: {}", json);
        assert_eq!(json.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_quit_queues_request() {
        let lua = Lua::new();
        let (queue, cache, sid, state) = create_test_deps();

        register(&lua, Arc::clone(&queue), cache, sid, state).expect("Should register");

        lua.load("hub.quit()").exec().expect("Should queue quit");

        let requests = queue.lock()
            .expect("Hub request queue mutex poisoned");
        assert_eq!(requests.len(), 1);
        assert!(matches!(requests[0], HubRequest::Quit));
    }

    #[test]
    fn test_server_id_returns_value() {
        let lua = Lua::new();
        let (queue, cache, sid, state) = create_test_deps();

        register(&lua, queue, cache, sid, state).expect("Should register");

        let id: String = lua.load("return hub.server_id()").eval().unwrap();
        assert_eq!(id, "test-hub-id");
    }

    #[test]
    fn test_server_id_returns_nil_when_unset() {
        let lua = Lua::new();
        let (queue, cache, _sid, state) = create_test_deps();
        let nil_sid: SharedServerId = Arc::new(Mutex::new(None));

        register(&lua, queue, cache, nil_sid, state).expect("Should register");

        let id: LuaValue = lua.load("return hub.server_id()").eval().unwrap();
        assert!(id.is_nil());
    }

    #[test]
    fn test_handle_webrtc_offer_queues_request() {
        let lua = Lua::new();
        let (queue, cache, sid, state) = create_test_deps();

        register(&lua, Arc::clone(&queue), cache, sid, state).expect("Should register");

        lua.load(r#"hub.handle_webrtc_offer("browser-123", "v=0 test-sdp")"#)
            .exec()
            .expect("Should queue offer");

        let requests = queue.lock().expect("Hub request queue mutex poisoned");
        assert_eq!(requests.len(), 1);
        match &requests[0] {
            HubRequest::HandleWebrtcOffer {
                browser_identity,
                sdp,
            } => {
                assert_eq!(browser_identity, "browser-123");
                assert_eq!(sdp, "v=0 test-sdp");
            }
            other => panic!("Expected HandleWebrtcOffer, got: {other:?}"),
        }
    }

    #[test]
    fn test_handle_ice_candidate_queues_request() {
        let lua = Lua::new();
        let (queue, cache, sid, state) = create_test_deps();

        register(&lua, Arc::clone(&queue), cache, sid, state).expect("Should register");

        lua.load(
            r#"hub.handle_ice_candidate("browser-456", {candidate = "candidate:...", sdpMid = "0"})"#,
        )
        .exec()
        .expect("Should queue ICE candidate");

        let requests = queue.lock().expect("Hub request queue mutex poisoned");
        assert_eq!(requests.len(), 1);
        match &requests[0] {
            HubRequest::HandleIceCandidate {
                browser_identity,
                candidate,
            } => {
                assert_eq!(browser_identity, "browser-456");
                assert_eq!(
                    candidate.get("candidate").and_then(|v| v.as_str()),
                    Some("candidate:...")
                );
                assert_eq!(
                    candidate.get("sdpMid").and_then(|v| v.as_str()),
                    Some("0")
                );
            }
            other => panic!("Expected HandleIceCandidate, got: {other:?}"),
        }
    }
}
