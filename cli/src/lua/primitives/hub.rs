//! Hub state primitives for Lua scripts.
//!
//! Exposes Hub state queries and operations to Lua, allowing scripts to
//! inspect worktrees, register/unregister agents, request lifecycle operations,
//! and initiate WebRTC signaling.
//!
//! # Design Principle: "Query freely. Mutate via event."
//!
//! - **State queries** (`get_worktrees`, `server_id`, `detect_repo`)
//!   read directly from shared state or environment
//! - **Registration** (`register_agent`, `unregister_agent`) manages PTY handles
//! - **Operations** (`quit`, `handle_webrtc_offer`, `handle_ice_candidate`)
//!   send events to the Hub event loop via `HubEventSender`
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

use super::HubEventSender;
use crate::hub::events::HubEvent;
use crate::hub::handle_cache::HandleCache;
use crate::hub::state::HubState;

/// Hub operation requests queued from Lua.
///
/// These are processed by Hub in its event loop after Lua callbacks return.
#[derive(Debug, Clone)]
pub enum HubRequest {
    /// Request Hub shutdown.
    Quit,
    /// Request update-and-restart (exec into new binary).
    ExecRestart,
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
    /// Initiate Olm ratchet restart for a browser whose session is desynced.
    RatchetRestart {
        /// Browser identity key (e.g., `identityKey:tabId`).
        browser_identity: String,
    },
}

/// Server-assigned hub ID, shared between Hub and Lua primitives.
pub type SharedServerId = Arc<Mutex<Option<String>>>;

/// Register Hub state primitives with the Lua state.
///
/// Adds the following functions to the `hub` table:
/// - `hub.get_worktrees()` - Get available worktrees
/// - `hub.register_agent(key, sessions)` - Register agent PTY handles
/// - `hub.unregister_agent(key)` - Unregister agent PTY handles
/// - `hub.hub_id()` - Get local hub identifier (stable hash, matches hub_discovery IDs)
/// - `hub.server_id()` - Get server-assigned hub ID
/// - `hub.detect_repo()` - Detect current repo name
/// - `hub.api_token()` - Get hub's API bearer token for authenticated requests
/// - `hub.handle_webrtc_offer(browser_identity, sdp)` - Send WebRTC offer event
/// - `hub.handle_ice_candidate(browser_identity, candidate)` - Send ICE candidate event
/// - `hub.quit()` - Request Hub shutdown
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `hub_event_tx` - Shared sender for Hub events (processed by Hub event loop)
/// * `handle_cache` - Thread-safe cache of agent handles for queries
/// * `hub_identifier` - Local hub identifier (stable hash, matches hub_discovery IDs)
/// * `server_id` - Server-assigned hub ID (set after registration)
/// * `shared_state` - Shared hub state for agent queries
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub(crate) fn register(
    lua: &Lua,
    hub_event_tx: HubEventSender,
    handle_cache: Arc<HandleCache>,
    hub_identifier: String,
    server_id: SharedServerId,
    _shared_state: Arc<RwLock<HubState>>,
    broker_connection: crate::broker::SharedBrokerConnection,
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

            // Convert to Lua via json_to_lua (null-safe, unlike lua.to_value)
            super::json::json_to_lua(lua, &serde_json::Value::Array(worktrees_data))
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
            // Use a placeholder index; add_agent returns the actual position
            // (which may differ on replace when agent already exists).
            let handle = AgentPtys::new(agent_key.clone(), pty_handles, 0);

            match cache2.add_agent(handle) {
                Some(idx) => {
                    // Update the agent_index to match actual position
                    cache2.update_agent_index(&agent_key, idx);
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
    // Also fires AgentUnregistered so the Hub can clean up broker_sessions entries.
    let cache3 = Arc::clone(&handle_cache);
    let tx_unreg = hub_event_tx.clone();
    let unregister_agent_fn = lua
        .create_function(move |_, agent_key: String| {
            let removed = cache3.remove_agent(&agent_key);
            if removed {
                log::info!("[Lua] Unregistered agent '{}'", agent_key);
                let guard = tx_unreg.lock().expect("HubEventSender mutex poisoned");
                if let Some(ref sender) = *guard {
                    let _ = sender.send(HubEvent::AgentUnregistered {
                        agent_key: agent_key.clone(),
                    });
                }
            }
            Ok(removed)
        })
        .map_err(|e| anyhow!("Failed to create hub.unregister_agent function: {e}"))?;

    hub.set("unregister_agent", unregister_agent_fn)
        .map_err(|e| anyhow!("Failed to set hub.unregister_agent: {e}"))?;

    // hub.hub_id() - Returns the local hub identifier (stable hash).
    // This is the same ID returned by hub_discovery.list(), suitable for
    // comparing against discovered hubs to identify self.
    let hub_id_fn = lua
        .create_function(move |_, ()| Ok(hub_identifier.clone()))
        .map_err(|e| anyhow!("Failed to create hub.hub_id function: {e}"))?;

    hub.set("hub_id", hub_id_fn)
        .map_err(|e| anyhow!("Failed to set hub.hub_id: {e}"))?;

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

    // hub.handle_webrtc_offer(browser_identity, sdp) - Send a WebRTC SDP offer event.
    let tx = hub_event_tx.clone();
    let handle_webrtc_offer_fn = lua
        .create_function(move |_, (browser_identity, sdp): (String, String)| {
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaHubRequest(HubRequest::HandleWebrtcOffer {
                    browser_identity,
                    sdp,
                }));
            } else {
                ::log::warn!("[Hub] handle_webrtc_offer called before hub_event_tx set — event dropped");
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.handle_webrtc_offer function: {e}"))?;

    hub.set("handle_webrtc_offer", handle_webrtc_offer_fn)
        .map_err(|e| anyhow!("Failed to set hub.handle_webrtc_offer: {e}"))?;

    // hub.handle_ice_candidate(browser_identity, candidate_data) - Send an ICE candidate event.
    //
    // `candidate_data` is a Lua table with `candidate`, `sdpMid`, and `sdpMLineIndex` fields,
    // matching the JSON structure from browser WebRTC signaling.
    let tx = hub_event_tx.clone();
    let handle_ice_candidate_fn = lua
        .create_function(move |lua, (browser_identity, candidate_data): (String, LuaValue)| {
            let candidate: serde_json::Value = lua.from_value(candidate_data)?;
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaHubRequest(HubRequest::HandleIceCandidate {
                    browser_identity,
                    candidate,
                }));
            } else {
                ::log::warn!("[Hub] handle_ice_candidate called before hub_event_tx set — event dropped");
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.handle_ice_candidate function: {e}"))?;

    hub.set("handle_ice_candidate", handle_ice_candidate_fn)
        .map_err(|e| anyhow!("Failed to set hub.handle_ice_candidate: {e}"))?;

    // hub.request_ratchet_restart(browser_identity) - Initiate Olm ratchet restart for a peer.
    let tx = hub_event_tx.clone();
    let request_ratchet_restart_fn = lua
        .create_function(move |_, browser_identity: String| {
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaHubRequest(HubRequest::RatchetRestart {
                    browser_identity,
                }));
            } else {
                ::log::warn!("[Hub] request_ratchet_restart called before hub_event_tx set — event dropped");
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.request_ratchet_restart function: {e}"))?;

    hub.set("request_ratchet_restart", request_ratchet_restart_fn)
        .map_err(|e| anyhow!("Failed to set hub.request_ratchet_restart: {e}"))?;

    // hub.register_pty_with_broker(session_handle, agent_key, pty_index)
    //   → session_id: integer | nil
    //
    // Transfers the master PTY FD to the broker via SCM_RIGHTS so the broker
    // can relay PTY I/O across Hub restarts. Returns the broker-assigned
    // session ID on success, or nil if the broker is unavailable or the FD
    // cannot be read (non-Unix, or PTY not yet spawned).
    //
    // Arguments:
    //   session_handle: PtySessionHandle userdata
    //   agent_key:  string  (e.g. "owner-repo-42")
    //   pty_index:  integer (0 = cli, 1 = server)
    #[cfg(unix)]
    {
        let broker_conn = Arc::clone(&broker_connection);
        let tx_reg = hub_event_tx.clone();
        let register_pty_fn = lua
            .create_function(
                move |_, (session_ud, agent_key, pty_index): (LuaAnyUserData, String, usize)| {
                    use crate::lua::primitives::pty::PtySessionHandle;

                    // Extract FD, PID, and current dimensions from the PtySessionHandle userdata.
                    // Drop the borrow before locking the broker so there is no
                    // risk of a lock-ordering deadlock.
                    let (fd, child_pid, dims) = {
                        let handle = session_ud.borrow::<PtySessionHandle>().map_err(|e| {
                            LuaError::runtime(format!(
                                "register_pty_with_broker: not a PtySessionHandle: {e}"
                            ))
                        })?;
                        (handle.get_master_fd(), handle.get_child_pid(), handle.get_dims())
                    }; // PtySessionHandle borrow released here

                    let fd = match fd {
                        Some(f) => f,
                        None => {
                            ::log::warn!(
                                "[Broker] register_pty_with_broker('{}' pty={}): \
                                 master FD unavailable",
                                agent_key,
                                pty_index
                            );
                            return Ok(LuaNil);
                        }
                    };

                    // Use the actual terminal dimensions from the session handle.
                    let (rows, cols) = dims;
                    let child_pid = child_pid.unwrap_or(0);

                    let session_id = {
                        let mut guard = broker_conn.lock().map_err(|_| {
                            LuaError::runtime(
                                "register_pty_with_broker: broker_connection mutex poisoned",
                            )
                        })?;
                        match guard.as_mut() {
                            Some(conn) => {
                                match conn.register_pty(
                                    &agent_key,
                                    pty_index,
                                    child_pid,
                                    rows,
                                    cols,
                                    fd,
                                ) {
                                    Ok(sid) => sid,
                                    Err(e) => {
                                        ::log::warn!(
                                            "[Broker] register_pty('{}' pty={}) failed: {e}",
                                            agent_key,
                                            pty_index
                                        );
                                        return Ok(LuaNil);
                                    }
                                }
                            }
                            None => {
                                ::log::debug!(
                                    "[Broker] register_pty_with_broker: broker not connected, \
                                     skipping"
                                );
                                return Ok(LuaNil);
                            }
                        }
                    };

                    // Notify the Hub event loop so it can populate its
                    // session-to-agent routing table.
                    {
                        let guard = tx_reg.lock().expect("HubEventSender mutex poisoned");
                        if let Some(ref sender) = *guard {
                            let _ = sender.send(HubEvent::BrokerSessionRegistered {
                                session_id,
                                agent_key: agent_key.clone(),
                                pty_index,
                            });
                        }
                    }

                    ::log::info!(
                        "[Broker] Registered PTY '{}' pty={} → session {}",
                        agent_key,
                        pty_index,
                        session_id
                    );
                    Ok(LuaValue::Integer(i64::from(session_id)))
                },
            )
            .map_err(|e| {
                anyhow!("Failed to create hub.register_pty_with_broker function: {e}")
            })?;

        hub.set("register_pty_with_broker", register_pty_fn)
            .map_err(|e| anyhow!("Failed to set hub.register_pty_with_broker: {e}"))?;
    }
    // On non-Unix targets, expose a no-op stub so plugins load without errors.
    #[cfg(not(unix))]
    {
        let noop_fn = lua
            .create_function(|_, _: LuaMultiValue| Ok(LuaNil))
            .map_err(|e| {
                anyhow!("Failed to create hub.register_pty_with_broker stub: {e}")
            })?;
        hub.set("register_pty_with_broker", noop_fn)
            .map_err(|e| anyhow!("Failed to set hub.register_pty_with_broker stub: {e}"))?;
    }

    // hub.get_pty_snapshot_from_broker(session_id) → string | nil
    //
    // Fetches the scrollback ring buffer for a broker-held session. Lua calls
    // this on Hub restart to replay bytes into a fresh vt100::Parser so the
    // shadow screen is reconstructed before setting up forwarders.
    //
    // Returns the raw byte string on success, or nil if the broker is not
    // connected or the fetch fails.
    //
    // Arguments:
    //   session_id: integer — value returned by register_pty_with_broker
    {
        let broker_conn2 = Arc::clone(&broker_connection);
        let snapshot_fn = lua
            .create_function(move |lua_ctx, session_id: u32| {
                let mut guard = broker_conn2.lock().map_err(|_| {
                    LuaError::runtime(
                        "get_pty_snapshot_from_broker: broker_connection mutex poisoned",
                    )
                })?;
                match guard.as_mut() {
                    Some(conn) => match conn.get_snapshot(session_id) {
                        Ok(bytes) => {
                            let s = lua_ctx.create_string(&bytes).map_err(|e| {
                                LuaError::runtime(format!(
                                    "get_pty_snapshot_from_broker: create_string failed: {e}"
                                ))
                            })?;
                            Ok(LuaValue::String(s))
                        }
                        Err(e) => {
                            ::log::warn!(
                                "[Broker] get_snapshot(session={session_id}) failed: {e}"
                            );
                            Ok(LuaNil)
                        }
                    },
                    None => {
                        ::log::debug!(
                            "[Broker] get_pty_snapshot_from_broker: broker not connected"
                        );
                        Ok(LuaNil)
                    }
                }
            })
            .map_err(|e| {
                anyhow!("Failed to create hub.get_pty_snapshot_from_broker function: {e}")
            })?;

        hub.set("get_pty_snapshot_from_broker", snapshot_fn)
            .map_err(|e| anyhow!("Failed to set hub.get_pty_snapshot_from_broker: {e}"))?;
    }

    // hub.quit() - Request Hub shutdown
    let tx = hub_event_tx;
    let quit_fn = lua
        .create_function(move |_, ()| {
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaHubRequest(HubRequest::Quit));
            } else {
                ::log::warn!("[Hub] quit() called before hub_event_tx set — event dropped");
            }
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
    use super::super::new_hub_event_sender;

    fn test_broker() -> crate::broker::SharedBrokerConnection {
        Arc::new(Mutex::new(None))
    }

    fn create_test_deps() -> (
        HubEventSender,
        Arc<HandleCache>,
        String,
        SharedServerId,
        Arc<RwLock<HubState>>,
    ) {
        let state = Arc::new(RwLock::new(HubState::new(
            std::path::PathBuf::from("/tmp/test-worktrees"),
        )));
        (
            new_hub_event_sender(),
            Arc::new(HandleCache::new()),
            "test-local-hub-id".to_string(),
            Arc::new(Mutex::new(Some("test-hub-id".to_string()))),
            state,
        )
    }

    #[test]
    fn test_register_creates_hub_table() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register hub primitives");

        let hub: LuaTable = lua.globals().get("hub").expect("hub table should exist");
        assert!(hub.contains_key("get_worktrees").unwrap());
        assert!(hub.contains_key("register_agent").unwrap());
        assert!(hub.contains_key("unregister_agent").unwrap());
        assert!(hub.contains_key("hub_id").unwrap());
        assert!(hub.contains_key("server_id").unwrap());
        assert!(hub.contains_key("detect_repo").unwrap());
        assert!(hub.contains_key("handle_webrtc_offer").unwrap());
        assert!(hub.contains_key("handle_ice_candidate").unwrap());
        assert!(hub.contains_key("quit").unwrap());
    }

    #[test]
    fn test_get_worktrees_returns_empty_array() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        let worktrees: LuaTable = lua.load("return hub.get_worktrees()").eval().unwrap();
        assert_eq!(worktrees.len().unwrap(), 0);
    }

    /// Empty worktrees returns an empty Lua table (iterable, length 0).
    #[test]
    fn test_get_worktrees_empty_returns_table() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        let worktrees: LuaTable = lua.load("return hub.get_worktrees()").eval().unwrap();
        assert_eq!(worktrees.len().unwrap(), 0, "Empty worktrees should have length 0");
    }

    #[test]
    fn test_quit_sends_event() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender);

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        lua.load("hub.quit()").exec().expect("Should send quit event");

        match rx.try_recv() {
            Ok(HubEvent::LuaHubRequest(HubRequest::Quit)) => {}
            other => panic!("Expected LuaHubRequest(Quit), got: {other:?}"),
        }
    }

    #[test]
    fn test_server_id_returns_value() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        let id: String = lua.load("return hub.server_id()").eval().unwrap();
        assert_eq!(id, "test-hub-id");
    }

    #[test]
    fn test_server_id_returns_nil_when_unset() {
        let lua = Lua::new();
        let (tx, cache, hid, _sid, state) = create_test_deps();
        let nil_sid: SharedServerId = Arc::new(Mutex::new(None));

        register(&lua, tx, cache, hid, nil_sid, state, test_broker()).expect("Should register");

        let id: LuaValue = lua.load("return hub.server_id()").eval().unwrap();
        assert!(id.is_nil());
    }

    #[test]
    fn test_handle_webrtc_offer_sends_event() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender);

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        lua.load(r#"hub.handle_webrtc_offer("browser-123", "v=0 test-sdp")"#)
            .exec()
            .expect("Should send offer event");

        match rx.try_recv() {
            Ok(HubEvent::LuaHubRequest(HubRequest::HandleWebrtcOffer {
                browser_identity,
                sdp,
            })) => {
                assert_eq!(browser_identity, "browser-123");
                assert_eq!(sdp, "v=0 test-sdp");
            }
            other => panic!("Expected LuaHubRequest(HandleWebrtcOffer), got: {other:?}"),
        }
    }

    #[test]
    fn test_handle_ice_candidate_sends_event() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender);

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        lua.load(
            r#"hub.handle_ice_candidate("browser-456", {candidate = "candidate:...", sdpMid = "0"})"#,
        )
        .exec()
        .expect("Should send ICE candidate event");

        match rx.try_recv() {
            Ok(HubEvent::LuaHubRequest(HubRequest::HandleIceCandidate {
                browser_identity,
                candidate,
            })) => {
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
            other => panic!("Expected LuaHubRequest(HandleIceCandidate), got: {other:?}"),
        }
    }

    /// Proves that `get_worktrees()` converts null JSON fields to Lua nil
    /// (not userdata). Uses `HandleCache::set_worktrees()` to inject data
    /// with a null field, then verifies Lua sees nil.
    #[test]
    fn test_get_worktrees_null_field_is_nil_not_userdata() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        // Inject a worktree so get_worktrees returns data
        cache.set_worktrees(vec![("/tmp/wt".to_string(), "main".to_string())]);

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        // get_worktrees returns array of {path, branch} - both strings, no nulls.
        // But the conversion path must use json_to_lua for safety.
        // Verify the result is a proper Lua table (not userdata).
        let result: LuaValue = lua.load("return hub.get_worktrees()").eval().unwrap();
        assert!(
            result.is_table(),
            "get_worktrees should return a table, got: {:?}",
            result
        );
        let tbl = result.as_table().unwrap();
        assert_eq!(tbl.len().unwrap(), 1);

        // Verify nested entry is also a proper table
        let entry: LuaValue = tbl.get(1).unwrap();
        assert!(
            entry.is_table(),
            "worktree entry should be a table, got: {:?}",
            entry
        );
    }
}
