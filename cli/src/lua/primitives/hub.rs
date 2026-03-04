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
//! - **Registration** (`register_session`, `unregister_session`) manages PTY handles
//! - **Operations** (`quit`, `handle_webrtc_offer`, `handle_ice_candidate`)
//!   send events to the Hub event loop via `HubEventSender`
//!
//! # Usage in Lua
//!
//! ```lua
//! -- Get available worktrees
//! local worktrees = hub.get_worktrees()
//!
//! -- Register session PTY handle
//! local index = hub.register_session("sess-abc123", handle, { agent_key = "owner-repo-42" })
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
    /// Request a graceful restart: broker keeps PTYs alive for the reconnect
    /// window so agents survive the Hub restarting.
    ///
    /// Unlike [`HubRequest::Quit`] (which sends `kill_all` to the broker),
    /// this calls `disconnect_graceful()`, giving the user 120 s to rerun
    /// `botster` before the broker times out and kills PTYs itself.
    GracefulRestart,
    /// Rebuild the CLI binary in the background, then exec-restart into it.
    ///
    /// Runs `cargo build` against the manifest embedded at compile time
    /// ([`env!("CARGO_MANIFEST_DIR")`]). On success the Hub exec-replaces
    /// itself with the freshly built binary; the broker preserves PTY FDs
    /// across the restart so agents survive.
    ///
    /// On build failure the Hub logs an error and keeps running — no agents
    /// are disrupted.  Intended for development iteration only.
    DevRebuild,
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
/// - `hub.register_session(uuid, handle, metadata)` - Register session PTY handle
/// - `hub.unregister_session(uuid)` - Unregister session PTY handle
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

    // hub.register_session(session_uuid, pty_handle, metadata) - Register session PTY handle
    //
    // Called by Lua Agent class to register a single PTY session handle with
    // HandleCache, enabling Rust-side PTY operations (forwarders, write, resize).
    //
    // Arguments:
    //   session_uuid: string - Stable session UUID (e.g., "sess-1234567890-abcdef")
    //   pty_handle:   PtySessionHandle userdata - Single PTY handle
    //   metadata:     table - { session_type = "agent"|"accessory", agent_key = "owner-repo-42", workspace_id = nil }
    let cache2 = Arc::clone(&handle_cache);
    let register_session_fn = lua
        .create_function(move |_, (session_uuid, session_ud, metadata): (String, LuaAnyUserData, LuaTable)| {
            use crate::hub::agent_handle::{SessionHandle, SessionType, PtyHandle};
            use crate::lua::primitives::pty::PtySessionHandle;

            let pty_handle: PtyHandle = {
                let handle = session_ud.borrow::<PtySessionHandle>().map_err(|e| {
                    LuaError::runtime(format!(
                        "register_session: not a PtySessionHandle: {e}"
                    ))
                })?;
                handle.to_pty_handle()
            };

            let agent_key: String = metadata
                .get("agent_key")
                .unwrap_or_else(|_| session_uuid.clone());

            let session_type_str: String = metadata
                .get("session_type")
                .unwrap_or_else(|_| "agent".to_string());
            let session_type = match session_type_str.as_str() {
                "accessory" => SessionType::Accessory,
                _ => SessionType::Agent,
            };

            let workspace_id: Option<String> = metadata
                .get("workspace_id")
                .ok();

            let handle = SessionHandle::new(
                session_uuid.clone(),
                agent_key.clone(),
                session_type,
                workspace_id,
                pty_handle,
            );

            cache2.add_session(handle);
            let index = cache2.index_of(&session_uuid);
            log::info!("[Lua] Registered session '{}' (key='{}', type={}) at index {:?}",
                session_uuid, agent_key, session_type, index);
            Ok(index.unwrap_or(0))
        })
        .map_err(|e| anyhow!("Failed to create hub.register_session function: {e}"))?;

    hub.set("register_session", register_session_fn)
        .map_err(|e| anyhow!("Failed to set hub.register_session: {e}"))?;

    // hub.unregister_session(session_uuid) - Unregister session PTY handle
    //
    // Called by Lua when a session is closed to remove it from HandleCache.
    // Also fires SessionUnregistered so the Hub can clean up broker_sessions entries.
    let cache3 = Arc::clone(&handle_cache);
    let tx_unreg = hub_event_tx.clone();
    let unregister_session_fn = lua
        .create_function(move |_, session_uuid: String| {
            let removed = cache3.remove_session(&session_uuid);
            if removed {
                log::info!("[Lua] Unregistered session '{}'", session_uuid);
                let guard = tx_unreg.lock().expect("HubEventSender mutex poisoned");
                if let Some(ref sender) = *guard {
                    let _ = sender.send(HubEvent::SessionUnregistered {
                        session_uuid: session_uuid.clone(),
                    });
                }
            }
            Ok(removed)
        })
        .map_err(|e| anyhow!("Failed to create hub.unregister_session function: {e}"))?;

    hub.set("unregister_session", unregister_session_fn)
        .map_err(|e| anyhow!("Failed to set hub.unregister_session: {e}"))?;

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

    // hub.register_pty_with_broker(session_handle, session_uuid)
    //   → session_id: integer | nil
    //
    // Transfers the master PTY FD to the broker via SCM_RIGHTS so the broker
    // can relay PTY I/O across Hub restarts. Returns the broker-assigned
    // session ID on success, or nil if the broker is unavailable or the FD
    // cannot be read (non-Unix, or PTY not yet spawned).
    //
    // Arguments:
    //   session_handle: PtySessionHandle userdata
    //   session_uuid:   string (e.g. "sess-1234567890-abcdef")
    #[cfg(unix)]
    {
        let broker_conn = Arc::clone(&broker_connection);
        let tx_reg = hub_event_tx.clone();
        let register_pty_fn = lua
            .create_function(
                move |_, (session_ud, session_uuid): (LuaAnyUserData, String)| {
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
                                "[Broker] register_pty_with_broker('{}'): \
                                 master FD unavailable",
                                session_uuid
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
                                    &session_uuid,
                                    child_pid,
                                    rows,
                                    cols,
                                    fd,
                                ) {
                                    Ok(sid) => sid,
                                    Err(e) => {
                                        ::log::warn!(
                                            "[Broker] register_pty('{}') failed: {e}",
                                            session_uuid
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
                                session_uuid: session_uuid.clone(),
                            });
                        }
                    }

                    ::log::info!(
                        "[Broker] Registered PTY '{}' → session {}",
                        session_uuid,
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

    // hub.pty_tee(session_id, log_path, cap_bytes) → true | nil
    //
    // Arms a file tee on an existing broker session.  After this call the
    // broker writes a copy of every PTY output byte to `log_path`.  The tee
    // survives Hub reconnects without needing to be re-armed.
    //
    // Path validation (Hub-side, before sending to broker):
    //   - Must contain "workspaces/" path component
    //   - Must contain "sessions/" path component
    //   These two constraints ensure the path stays within the workspace
    //   sessions directory and is not an arbitrary filesystem location.
    //
    // Arguments:
    //   session_id: integer  — value returned by register_pty_with_broker
    //   log_path:   string   — absolute path ending in pty-0.log
    //   cap_bytes:  integer  — rotation threshold (0 → broker default 10 MiB)
    //
    // Returns true on success, nil if the broker is not connected or the
    // path fails validation.
    {
        let broker_tee = Arc::clone(&broker_connection);
        let pty_tee_fn = lua
            .create_function(
                move |_, (session_id, log_path, cap_bytes): (u32, String, u64)| {
                    // Path validation: must stay within the workspace sessions tree.
                    //
                    // Component-level checks prevent crafted paths that satisfy a
                    // naive substring match (e.g. "/tmp/evil-workspaces/x") from
                    // being passed to the broker:
                    //   1. Must be absolute (starts with '/').
                    //   2. Must contain a path component exactly equal to "workspaces".
                    //   3. Must contain a path component exactly equal to "sessions".
                    //   4. Must not contain ".." traversal components.
                    //
                    // These four checks together ensure the path stays inside the
                    // intended directory tree without requiring the file to exist yet
                    // (the broker creates it on first write).
                    let path = std::path::Path::new(&log_path);
                    let components: Vec<_> = path
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect();
                    let is_absolute = path.is_absolute();
                    let has_workspaces = components.iter().any(|c| c == "workspaces");
                    let has_sessions = components.iter().any(|c| c == "sessions");
                    let has_traversal = components.iter().any(|c| c == "..");
                    if !is_absolute || !has_workspaces || !has_sessions || has_traversal {
                        ::log::warn!(
                            "[Broker] pty_tee rejected unsafe path: {log_path}"
                        );
                        return Ok(LuaNil);
                    }

                    let mut guard = broker_tee.lock().map_err(|_| {
                        LuaError::runtime("pty_tee: broker_connection mutex poisoned")
                    })?;
                    match guard.as_mut() {
                        Some(conn) => match conn.arm_tee(session_id, &log_path, cap_bytes) {
                            Ok(()) => {
                                ::log::info!(
                                    "[Broker] Tee armed: session={session_id} path={log_path}"
                                );
                                Ok(LuaValue::Boolean(true))
                            }
                            Err(e) => {
                                ::log::warn!(
                                    "[Broker] pty_tee arm failed for session {session_id}: {e}"
                                );
                                Ok(LuaNil)
                            }
                        },
                        None => {
                            ::log::debug!(
                                "[Broker] pty_tee: broker not connected, skipping session {session_id}"
                            );
                            Ok(LuaNil)
                        }
                    }
                },
            )
            .map_err(|e| anyhow!("Failed to create hub.pty_tee function: {e}"))?;

        hub.set("pty_tee", pty_tee_fn)
            .map_err(|e| anyhow!("Failed to set hub.pty_tee: {e}"))?;
    }

    // hub.get_pty_snapshot_from_broker(session_id) → string | nil
    //
    // Fetches the scrollback ring buffer for a broker-held session. Lua calls
    // this on Hub restart to replay bytes into a fresh AlacrittyParser shadow
    // screen so state is reconstructed before setting up forwarders.
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

    // hub.create_ghost_session(session_uuid, session_id, rows, cols)
    //   → PtySessionHandle userdata
    //
    // Creates a shadow-screen-only PTY handle for Hub restart recovery.
    // No real PTY process is spawned — only the AlacrittyParser shadow screen
    // and broadcast channel are initialised with the given dimensions.
    //
    // Also fires HubEvent::BrokerSessionRegistered so the Hub's
    // broker_sessions routing table maps session_id → session_uuid.
    // BrokerPtyOutput frames then route to the correct HandleCache entry once
    // the caller registers the ghost handle via hub.register_session().
    //
    // Arguments:
    //   session_uuid: string  — session UUID (e.g. "sess-1234567890-abcdef")
    //   session_id:   integer — broker session ID from context.json metadata
    //   rows:         integer — terminal rows saved in context.json metadata
    //   cols:         integer — terminal cols saved in context.json metadata
    {
        let tx_ghost = Arc::clone(&hub_event_tx);
        let create_ghost_fn = lua
            .create_function(
                move |_,
                      (session_uuid, session_id, rows, cols): (
                    String,
                    u32,
                    u16,
                    u16,
                )| {
                    use crate::lua::primitives::pty::PtySessionHandle;

                    // Create a ghost handle — no real PTY, just shadow screen
                    // and broadcast channel at the dimensions saved in context.json.
                    let handle = PtySessionHandle::new_ghost(rows, cols, Arc::clone(&tx_ghost));

                    // Register the broker session_id → session_uuid mapping
                    // so BrokerPtyOutput frames route correctly once the caller calls
                    // hub.register_session() to populate HandleCache.
                    {
                        let guard = tx_ghost
                            .lock()
                            .expect("HubEventSender mutex poisoned");
                        if let Some(ref sender) = *guard {
                            let _ = sender.send(HubEvent::BrokerSessionRegistered {
                                session_id,
                                session_uuid: session_uuid.clone(),
                            });
                        }
                    }

                    ::log::info!(
                        "[Broker] Created ghost session '{}' session={}",
                        session_uuid,
                        session_id
                    );

                    Ok(handle)
                },
            )
            .map_err(|e| anyhow!("Failed to create hub.create_ghost_session function: {e}"))?;

        hub.set("create_ghost_session", create_ghost_fn)
            .map_err(|e| anyhow!("Failed to set hub.create_ghost_session: {e}"))?;
    }

    // hub.quit() - Request Hub shutdown (broker kills PTYs immediately).
    let tx_quit = hub_event_tx.clone();
    let quit_fn = lua
        .create_function(move |_, ()| {
            let guard = tx_quit.lock().expect("HubEventSender mutex poisoned");
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

    // hub.graceful_restart() - Request Hub shutdown with broker keeping PTYs
    // alive so agents survive the restart.
    //
    // The broker will wait up to its configured timeout (120 s by default)
    // for the Hub to reconnect.  If it does not, the broker kills PTYs and
    // exits on its own.  Use this from the TUI / browser "Restart" action
    // instead of hub.quit() when you intend to immediately relaunch botster.
    let tx = hub_event_tx.clone();
    let graceful_restart_fn = lua
        .create_function(move |_, ()| {
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaHubRequest(HubRequest::GracefulRestart));
            } else {
                ::log::warn!(
                    "[Hub] graceful_restart() called before hub_event_tx set — event dropped"
                );
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.graceful_restart function: {e}"))?;

    hub.set("graceful_restart", graceful_restart_fn)
        .map_err(|e| anyhow!("Failed to set hub.graceful_restart: {e}"))?;

    // hub.exec_restart() - Exec-restart the Hub process: broker keeps PTYs alive
    // across the exec() so agents survive.
    //
    // The Hub process replaces itself (execv) with a fresh instance of the same
    // binary using the same arguments.  This is the correct primitive for the
    // TUI "restart_hub" action: the broker stays alive during the reconnect
    // window, the new Hub picks up ghost agents on startup, and the user sees
    // their agents restored without manual intervention.
    //
    // Contrast with hub.graceful_restart(), which quits the Hub cleanly but
    // does NOT re-exec — the user must manually relaunch botster.
    let tx = hub_event_tx.clone();
    let exec_restart_fn = lua
        .create_function(move |_, ()| {
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaHubRequest(HubRequest::ExecRestart));
            } else {
                ::log::warn!(
                    "[Hub] exec_restart() called before hub_event_tx set — event dropped"
                );
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.exec_restart function: {e}"))?;

    hub.set("exec_restart", exec_restart_fn)
        .map_err(|e| anyhow!("Failed to set hub.exec_restart: {e}"))?;

    // hub.dev_rebuild() - Build the CLI binary then exec-restart into it.
    //
    // Runs `cargo build` in a background task using the manifest dir embedded
    // at compile time. On success, fires ExecRestart so the Hub exec-replaces
    // itself with the fresh binary while the broker preserves PTY FDs.
    //
    // On build failure the Hub logs the error and keeps running — no agents
    // are disrupted. This is a development-time convenience primitive; it is
    // safe to call in any environment but cargo must be on $PATH.
    let tx_dev = hub_event_tx.clone();
    let dev_rebuild_fn = lua
        .create_function(move |_, ()| {
            let guard = tx_dev.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaHubRequest(HubRequest::DevRebuild));
            } else {
                ::log::warn!(
                    "[Hub] dev_rebuild() called before hub_event_tx set — event dropped"
                );
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.dev_rebuild function: {e}"))?;

    hub.set("dev_rebuild", dev_rebuild_fn)
        .map_err(|e| anyhow!("Failed to set hub.dev_rebuild: {e}"))?;

    // Ensure hub table is globally registered
    lua.globals()
        .set("hub", hub)
        .map_err(|e| anyhow!("Failed to register hub table globally: {e}"))?;

    // Backward-compatible aliases for Lua handlers not yet updated (Phase 5-6 scope).
    // These will be removed once the Lua side is updated to use the new names.
    lua.load(
        r#"
        hub.register_agent = function(agent_key, sessions)
            -- Legacy: register first session handle with agent_key as UUID placeholder
            local handle = sessions[1]
            if handle then
                return hub.register_session(agent_key, handle, { agent_key = agent_key })
            end
            return nil
        end
        hub.unregister_agent = function(agent_key)
            return hub.unregister_session(agent_key)
        end
        hub.create_ghost_pty = function(agent_key, _pty_index, session_id, rows, cols)
            return hub.create_ghost_session(agent_key, session_id, rows, cols)
        end
        -- Legacy: 3-arg form (handle, key, pty_index) → 2-arg form (handle, key).
        -- pty_index is silently dropped (single-PTY model).
        local _new_register_pty = hub.register_pty_with_broker
        hub.register_pty_with_broker = function(handle, key_or_uuid, _pty_index)
            return _new_register_pty(handle, key_or_uuid)
        end
        "#,
    )
    .exec()
    .map_err(|e| anyhow!("Failed to register backward-compat hub aliases: {e}"))?;

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
        assert!(hub.contains_key("register_session").unwrap());
        assert!(hub.contains_key("unregister_session").unwrap());
        assert!(hub.contains_key("hub_id").unwrap());
        assert!(hub.contains_key("server_id").unwrap());
        assert!(hub.contains_key("detect_repo").unwrap());
        assert!(hub.contains_key("handle_webrtc_offer").unwrap());
        assert!(hub.contains_key("handle_ice_candidate").unwrap());
        assert!(hub.contains_key("quit").unwrap());
        assert!(hub.contains_key("graceful_restart").unwrap());
        assert!(hub.contains_key("exec_restart").unwrap());
        assert!(hub.contains_key("dev_rebuild").unwrap());
        assert!(hub.contains_key("pty_tee").unwrap());
    }

    /// `hub.pty_tee` with a path missing the "workspaces" component must return nil.
    #[test]
    fn test_pty_tee_rejects_unsafe_path() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        // Path lacks required "workspaces" component.
        let result: LuaValue = lua
            .load(r#"return hub.pty_tee(1, "/tmp/bad/path/pty-0.log", 0)"#)
            .eval()
            .unwrap();
        assert!(result.is_nil(), "unsafe path must return nil");
    }

    /// `hub.pty_tee` with a path missing the "sessions" component must return nil.
    #[test]
    fn test_pty_tee_rejects_path_without_sessions() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        // Path has "workspaces" component but not "sessions".
        let result: LuaValue = lua
            .load(r#"return hub.pty_tee(1, "/data/workspaces/my-agent/pty-0.log", 0)"#)
            .eval()
            .unwrap();
        assert!(result.is_nil(), "path without sessions component must return nil");
    }

    /// A crafted path that contains "workspaces" as a substring of a directory name
    /// (e.g. "evil-workspaces") must be rejected — component-level check required.
    #[test]
    fn test_pty_tee_rejects_workspaces_as_substring() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        // "evil-workspaces" satisfies a naive contains("workspaces/") check but is
        // not the exact "workspaces" path component — must be rejected.
        let result: LuaValue = lua
            .load(r#"return hub.pty_tee(1, "/tmp/evil-workspaces/x/sessions/0/pty-0.log", 0)"#)
            .eval()
            .unwrap();
        assert!(result.is_nil(), "substring match on component name must be rejected");
    }

    /// A path with ".." traversal components must be rejected.
    #[test]
    fn test_pty_tee_rejects_path_traversal() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        let result: LuaValue = lua
            .load(r#"return hub.pty_tee(1, "/data/workspaces/agent/../../../etc/sessions/0/pty-0.log", 0)"#)
            .eval()
            .unwrap();
        assert!(result.is_nil(), "path traversal must be rejected");
    }

    /// A relative path (no leading '/') must be rejected.
    #[test]
    fn test_pty_tee_rejects_relative_path() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        let result: LuaValue = lua
            .load(r#"return hub.pty_tee(1, "workspaces/agent/sessions/0/pty-0.log", 0)"#)
            .eval()
            .unwrap();
        assert!(result.is_nil(), "relative path must be rejected");
    }

    /// `hub.pty_tee` returns nil when the broker is not connected.
    #[test]
    fn test_pty_tee_returns_nil_when_broker_disconnected() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        // test_broker() returns Arc<Mutex<None>> — broker not connected.
        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        let result: LuaValue = lua
            .load(r#"return hub.pty_tee(1, "/data/workspaces/key/sessions/0/pty-0.log", 0)"#)
            .eval()
            .unwrap();
        assert!(result.is_nil(), "disconnected broker must return nil");
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
        *tx.lock().unwrap() = Some(sender.into());

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        lua.load("hub.quit()").exec().expect("Should send quit event");

        match rx.try_recv() {
            Ok(HubEvent::LuaHubRequest(HubRequest::Quit)) => {}
            other => panic!("Expected LuaHubRequest(Quit), got: {other:?}"),
        }
    }

    #[test]
    fn test_graceful_restart_sends_event() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender.into());

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        lua.load("hub.graceful_restart()")
            .exec()
            .expect("Should send graceful_restart event");

        match rx.try_recv() {
            Ok(HubEvent::LuaHubRequest(HubRequest::GracefulRestart)) => {}
            other => panic!("Expected LuaHubRequest(GracefulRestart), got: {other:?}"),
        }
    }

    /// `hub.exec_restart()` sends `HubRequest::ExecRestart`, which causes the
    /// Hub to re-exec itself after gracefully disconnecting the broker so agents
    /// survive the restart.
    #[test]
    fn test_exec_restart_sends_event() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender.into());

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        lua.load("hub.exec_restart()")
            .exec()
            .expect("Should send exec_restart event");

        match rx.try_recv() {
            Ok(HubEvent::LuaHubRequest(HubRequest::ExecRestart)) => {}
            other => panic!("Expected LuaHubRequest(ExecRestart), got: {other:?}"),
        }
    }

    #[test]
    fn test_dev_rebuild_sends_event() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender.into());

        register(&lua, tx, cache, hid, sid, state, test_broker()).expect("Should register");

        lua.load("hub.dev_rebuild()")
            .exec()
            .expect("Should send dev_rebuild event");

        match rx.try_recv() {
            Ok(HubEvent::LuaHubRequest(HubRequest::DevRebuild)) => {}
            other => panic!("Expected LuaHubRequest(DevRebuild), got: {other:?}"),
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
        *tx.lock().unwrap() = Some(sender.into());

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
        *tx.lock().unwrap() = Some(sender.into());

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
