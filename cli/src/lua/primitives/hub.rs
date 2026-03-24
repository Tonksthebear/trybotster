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
//! local index = hub.register_session("sess-abc123", handle, {
//!   label = "owner-repo-42",
//!   broker_session_id = 123,
//! })
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
    /// Send a fresh signed bundle to a browser without ratchet-restart dedupe.
    SendFreshBundle {
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
/// - `hub.spawn_pty_with_broker(spawn_opts, session_uuid)` - Spawn and broker-register PTY
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
    //   metadata:     table - {
    //     session_type = "agent"|"accessory",
    //     label = "owner-repo-42",
    //     workspace_id = nil,
    //     broker_session_id = 123, -- required for all runtime sessions
    //   }
    let cache2 = Arc::clone(&handle_cache);
    let register_event_tx = Arc::clone(&hub_event_tx);
    let register_session_fn = lua
        .create_function(
            move |_, (session_uuid, session_ud, metadata): (String, LuaAnyUserData, LuaTable)| {
                use crate::hub::agent_handle::{PtyHandle, SessionHandle, SessionType};
                use crate::lua::primitives::pty::PtySessionHandle;

                // Grab event_tx + recovered flag + session_connection before consuming handle
                let (event_tx_clone, is_recovered, has_session_conn) = {
                    let handle = session_ud.borrow::<PtySessionHandle>().map_err(|e| {
                        LuaError::runtime(format!("register_session: not a PtySessionHandle: {e}"))
                    })?;
                    (
                        handle.event_tx(),
                        handle.is_recovered(),
                        handle.get_session_connection().is_some(),
                    )
                };

                let pty_handle: PtyHandle = {
                    let handle = session_ud.borrow::<PtySessionHandle>().map_err(|e| {
                        LuaError::runtime(format!("register_session: not a PtySessionHandle: {e}"))
                    })?;

                    if has_session_conn {
                        // Per-session process path
                        let conn = handle
                            .get_session_connection()
                            .expect("session_connection was Some above")
                            .clone();
                        handle.to_pty_handle_with_session(conn)
                    } else {
                        return Err(LuaError::runtime(
                            "register_session: session_connection required (broker path removed)",
                        ));
                    }
                };

                let label: String = metadata
                    .get("label")
                    .unwrap_or_else(|_| session_uuid.clone());

                let session_type_str: String = metadata
                    .get("session_type")
                    .unwrap_or_else(|_| "agent".to_string());
                let session_type = match session_type_str.as_str() {
                    "accessory" => SessionType::Accessory,
                    _ => SessionType::Agent,
                };

                let workspace_id: Option<String> = metadata.get("workspace_id").ok();

                let handle = SessionHandle::new(
                    session_uuid.clone(),
                    label.clone(),
                    session_type,
                    workspace_id,
                    pty_handle,
                );

                cache2.add_session(handle);
                let index = cache2.index_of(&session_uuid);
                log::info!(
                    "[Lua] Registered session '{}' (label='{}', type={}) at index {:?}",
                    session_uuid,
                    label,
                    session_type,
                    index
                );

                // Session-process-backed handles: install reader thread.
                // The reader feeds the shadow screen and broadcasts output directly.
                if has_session_conn {
                    let handle = session_ud.borrow::<PtySessionHandle>().map_err(|e| {
                        LuaError::runtime(format!("register_session: borrow for reader: {e}"))
                    })?;
                    if let Some(conn) = handle.get_session_connection() {
                        let session_handle = cache2.get_session(&session_uuid);
                        if let Some(sh) = session_handle {
                            let pty = sh.pty();
                            if let Ok(mut guard) = conn.lock() {
                                if let Some(ref mut session_conn) = *guard {
                                    if let Err(e) = session_conn.install_reader(
                                        session_uuid.clone(),
                                        pty.shadow_screen(),
                                        pty.event_tx_clone(),
                                        pty.kitty_enabled_arc(),
                                        pty.cursor_visible_arc(),
                                        pty.resize_pending_arc(),
                                        pty.last_output_at_atomic().clone(),
                                        session_type == SessionType::Agent,
                                        {
                                            let g = register_event_tx
                                                .lock()
                                                .expect("HubEventSender mutex poisoned");
                                            g.as_ref().cloned().ok_or_else(|| {
                                                LuaError::runtime("hub_event_tx not set")
                                            })?
                                        },
                                    ) {
                                        log::warn!(
                                            "[Lua] Failed to install session reader for '{}': {e}",
                                            session_uuid
                                        );
                                    } else {
                                        // Reader installed — request initial snapshot to populate shadow screen.
                                        // This gives the TUI immediate content on reconnect.
                                        match session_conn.get_snapshot() {
                                            Ok(snapshot) if !snapshot.is_empty() => {
                                                if let Ok(mut screen) = pty.shadow_screen().lock() {
                                                    screen.process(&snapshot);
                                                }
                                                log::info!(
                                                    "[Lua] Replayed {} bytes of session snapshot for '{}'",
                                                    snapshot.len(),
                                                    &session_uuid[..session_uuid.len().min(16)]
                                                );
                                            }
                                            Ok(_) => {
                                                log::debug!("[Lua] Empty snapshot for '{}'", &session_uuid[..session_uuid.len().min(16)]);
                                            }
                                            Err(e) => {
                                                log::warn!("[Lua] Snapshot replay failed for '{}': {e}", &session_uuid[..session_uuid.len().min(16)]);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // Recovered broker sessions skip the spawn path which normally creates a
                // notification watcher. Spawn one now so title/CWD/bell events
                // from the broker reach Lua hooks.
                if is_recovered && !has_session_conn {
                    let session_name: String = metadata
                        .get("session_name")
                        .unwrap_or_else(|_| session_type_str.clone());
                    let watcher_key = format!("{}:{}", session_uuid, session_name);
                    let guard = register_event_tx.lock().expect("HubEventSender mutex poisoned");
                    if let Some(ref sender) = *guard {
                        let _ = sender.send(HubEvent::LuaPtyRequest(
                            crate::lua::primitives::pty::PtyRequest::SpawnNotificationWatcher {
                                watcher_key,
                                session_uuid: session_uuid.clone(),
                                session_name,
                                event_tx: event_tx_clone,
                            },
                        ));
                    }
                }

                Ok(index.unwrap_or(0))
            },
        )
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

    // hub.exe_dir() — directory containing the running botster binary.
    // Used to prepend to child PATH so `botster` resolves to the same build.
    let exe_dir_fn = lua
        .create_function(|_, ()| {
            let dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.display().to_string()))
                .unwrap_or_default();
            Ok(dir)
        })
        .map_err(|e| anyhow!("Failed to create hub.exe_dir function: {e}"))?;
    hub.set("exe_dir", exe_dir_fn)
        .map_err(|e| anyhow!("Failed to set hub.exe_dir: {e}"))?;

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

    // hub.is_offline() - Returns true if the hub was started with --offline.
    //
    // Plugins should check this before making any network calls (HTTP, ActionCable,
    // WebSocket) to avoid connection errors in offline mode.
    let is_offline_fn = lua
        .create_function(move |_, ()| Ok(crate::env::is_offline()))
        .map_err(|e| anyhow!("Failed to create hub.is_offline function: {e}"))?;

    hub.set("is_offline", is_offline_fn)
        .map_err(|e| anyhow!("Failed to set hub.is_offline: {e}"))?;

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
                ::log::warn!(
                    "[Hub] handle_webrtc_offer called before hub_event_tx set — event dropped"
                );
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
        .create_function(
            move |lua, (browser_identity, candidate_data): (String, LuaValue)| {
                let candidate: serde_json::Value = lua.from_value(candidate_data)?;
                let guard = tx.lock().expect("HubEventSender mutex poisoned");
                if let Some(ref sender) = *guard {
                    let _ = sender.send(HubEvent::LuaHubRequest(HubRequest::HandleIceCandidate {
                        browser_identity,
                        candidate,
                    }));
                } else {
                    ::log::warn!(
                        "[Hub] handle_ice_candidate called before hub_event_tx set — event dropped"
                    );
                }
                Ok(())
            },
        )
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
                ::log::warn!(
                    "[Hub] request_ratchet_restart called before hub_event_tx set — event dropped"
                );
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.request_ratchet_restart function: {e}"))?;

    hub.set("request_ratchet_restart", request_ratchet_restart_fn)
        .map_err(|e| anyhow!("Failed to set hub.request_ratchet_restart: {e}"))?;

    // hub.send_fresh_bundle(browser_identity) - Push a fresh signed bundle for a new session.
    let tx = hub_event_tx.clone();
    let send_fresh_bundle_fn = lua
        .create_function(move |_, browser_identity: String| {
            let guard = tx.lock().expect("HubEventSender mutex poisoned");
            if let Some(ref sender) = *guard {
                let _ = sender.send(HubEvent::LuaHubRequest(HubRequest::SendFreshBundle {
                    browser_identity,
                }));
            } else {
                ::log::warn!(
                    "[Hub] send_fresh_bundle called before hub_event_tx set — event dropped"
                );
            }
            Ok(())
        })
        .map_err(|e| anyhow!("Failed to create hub.send_fresh_bundle function: {e}"))?;

    hub.set("send_fresh_bundle", send_fresh_bundle_fn)
        .map_err(|e| anyhow!("Failed to set hub.send_fresh_bundle: {e}"))?;

    // hub.spawn_session(opts, session_uuid) → PtySessionHandle
    //
    // Spawns a per-session process that creates its own PTY and binds a Unix
    // socket. The hub connects to the socket and installs a reader thread.
    // No broker, no FD transfer, no multiplexing.
    //
    // The session process is a separate OS process (`botster session`) that
    // survives hub restarts. Recovery reconnects via socket directory scan.
    //
    // Arguments:
    //   opts: table {
    //     worktree_path: string     — working directory for the child
    //     command: string?           — command to run (default "bash")
    //     rows: integer?             — terminal rows (default 24)
    //     cols: integer?             — terminal cols (default 80)
    //     detect_notifications: bool? — enable OSC notification detection
    //     port: integer?             — HTTP forwarding port
    //     env: table?                — environment variables {KEY=VAL, ...}
    //     init_commands: table?       — commands to write after spawn
    //     tee_path: string?          — log file path
    //     tee_cap: integer?          — log rotation cap (default 10MB)
    //   }
    //   session_uuid: string — stable session UUID
    #[cfg(unix)]
    {
        let tx_spawn = hub_event_tx.clone();
        let spawn_session_fn = lua
            .create_function(
                move |_lua_ctx, (opts, session_uuid): (LuaTable, String)| {
                    use crate::session::connection::SessionConnection;
                    use crate::session::protocol;
                    use crate::session::SpawnConfig;

                    // Parse opts
                    let worktree_path: String = opts
                        .get("worktree_path")
                        .map_err(|_| LuaError::runtime("worktree_path is required"))?;
                    let command: String =
                        opts.get("command").unwrap_or_else(|_| "bash".to_string());
                    let rows: u16 = opts.get("rows").unwrap_or(24);
                    let cols: u16 = opts.get("cols").unwrap_or(80);
                    let detect_notifications: bool =
                        opts.get("detect_notifications").unwrap_or(false);
                    let port: Option<u16> = opts.get("port").ok();
                    let tee_path: Option<String> = opts.get("tee_path").ok();
                    let tee_cap: u64 =
                        opts.get("tee_cap").unwrap_or(10 * 1024 * 1024);

                    // Parse env table
                    let mut env_pairs = Vec::new();
                    if let Ok(env_table) = opts.get::<LuaTable>("env") {
                        for pair in env_table.pairs::<String, String>() {
                            if let Ok((k, v)) = pair {
                                env_pairs.push((k, v));
                            }
                        }
                    }

                    // Parse init_commands (written to PTY stdin after child spawns)
                    let mut init_commands = Vec::new();
                    if let Ok(cmds_table) = opts.get::<LuaTable>("init_commands") {
                        for pair in cmds_table.pairs::<i64, String>() {
                            if let Ok((_, cmd)) = pair {
                                init_commands.push(cmd);
                            }
                        }
                    }

                    // Determine socket path
                    let socket_path = crate::session::session_socket_path(&session_uuid)
                        .map_err(|e| {
                            LuaError::runtime(format!(
                                "spawn_session: socket path error: {e}"
                            ))
                        })?;

                    // Fork session process
                    let exe = std::env::current_exe().map_err(|e| {
                        LuaError::runtime(format!("spawn_session: current_exe: {e}"))
                    })?;
                    let socket_str = socket_path.display().to_string();
                    match std::process::Command::new(&exe)
                        .args([
                            "session",
                            "--uuid",
                            &session_uuid,
                            "--socket",
                            &socket_str,
                            "--timeout",
                            "120",
                        ])
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                    {
                        Ok(child) => {
                            ::log::info!(
                                "[Session] spawned session process (pid {})",
                                child.id()
                            );
                            std::mem::forget(child); // detach
                        }
                        Err(e) => {
                            return Err(LuaError::runtime(format!(
                                "spawn_session: failed to fork: {e}"
                            )));
                        }
                    }

                    // Wait for socket to appear
                    let deadline = std::time::Instant::now()
                        + std::time::Duration::from_millis(2000);
                    while !socket_path.exists() {
                        if std::time::Instant::now() >= deadline {
                            return Err(LuaError::runtime(
                                "spawn_session: session process socket did not appear within 2s",
                            ));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    // Small settle time for the listener to be ready
                    std::thread::sleep(std::time::Duration::from_millis(50));

                    // Connect
                    let mut conn =
                        SessionConnection::connect(&socket_path).map_err(|e| {
                            LuaError::runtime(format!(
                                "spawn_session: connect failed: {e}"
                            ))
                        })?;

                    // Send spawn config
                    let spawn_config = SpawnConfig {
                        command,
                        args: Vec::new(),
                        env: env_pairs,
                        cwd: Some(worktree_path),
                        rows,
                        cols,
                        init_commands,
                        tee_path,
                        tee_cap,
                    };
                    conn.send_spawn_config(&spawn_config).map_err(|e| {
                        LuaError::runtime(format!(
                            "spawn_session: send config failed: {e}"
                        ))
                    })?;

                    // Wrap in shared connection
                    let shared_conn: crate::session::connection::SharedSessionConnection =
                        std::sync::Arc::new(std::sync::Mutex::new(Some(conn)));

                    // Create a PtySessionHandle that wraps the session connection.
                    // This handle is returned to Lua and later registered via
                    // hub.register_session() which creates the PtyHandle and
                    // installs the reader thread.
                    use crate::lua::primitives::pty::PtySessionHandle;
                    let handle = PtySessionHandle::new_minimal(
                        rows,
                        cols,
                        std::sync::Arc::clone(&tx_spawn),
                    );

                    // Store the session connection on the handle so register_session
                    // can retrieve it later.
                    handle.set_session_connection(shared_conn);

                    ::log::info!(
                        "[Session] connected to session process for '{}'",
                        &session_uuid[..session_uuid.len().min(16)]
                    );

                    Ok(handle)
                },
            )
            .map_err(|e| anyhow!("Failed to create hub.spawn_session function: {e}"))?;

        hub.set("spawn_session", spawn_session_fn)
            .map_err(|e| anyhow!("Failed to set hub.spawn_session: {e}"))?;
    }

    // hub.connect_session(session_uuid, socket_path) → PtySessionHandle
    //
    // Connects to an existing session process socket for recovery.
    // Returns a PtySessionHandle with the session connection attached.
    // The caller then passes this to hub.register_session() which installs
    // the reader thread and creates the PtyHandle.
    {
        let tx_connect = hub_event_tx.clone();
        let connect_session_fn = lua
            .create_function(
                move |_, (session_uuid, socket_path): (String, String)| {
                    use crate::session::connection::SessionConnection;

                    let path = std::path::Path::new(&socket_path);
                    let conn = SessionConnection::connect(path).map_err(|e| {
                        LuaError::runtime(format!(
                            "connect_session('{}'): {e}",
                            &session_uuid[..session_uuid.len().min(16)]
                        ))
                    })?;

                    let rows = conn.metadata.rows;
                    let cols = conn.metadata.cols;

                    let shared_conn: crate::session::connection::SharedSessionConnection =
                        std::sync::Arc::new(std::sync::Mutex::new(Some(conn)));

                    use crate::lua::primitives::pty::PtySessionHandle;
                    let handle = PtySessionHandle::new_minimal(
                        rows,
                        cols,
                        std::sync::Arc::clone(&tx_connect),
                    );
                    handle.set_session_connection(shared_conn);

                    ::log::info!(
                        "[Session] recovery connect for '{}'",
                        &session_uuid[..session_uuid.len().min(16)]
                    );

                    Ok(handle)
                },
            )
            .map_err(|e| anyhow!("Failed to create hub.connect_session function: {e}"))?;

        hub.set("connect_session", connect_session_fn)
            .map_err(|e| anyhow!("Failed to set hub.connect_session: {e}"))?;
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
    // hub.pty_tee is now a no-op stub — session processes handle tee via spawn config.
    {
        let pty_tee_fn = lua
            .create_function(
                move |_, (_session_id, _log_path, _cap_bytes): (u32, String, u64)| {
                    ::log::debug!("[Hub] pty_tee is a no-op — session processes handle tee via spawn config");
                    Ok(LuaNil)
                },
            )
            .map_err(|e| anyhow!("Failed to create hub.pty_tee function: {e}"))?;

        hub.set("pty_tee", pty_tee_fn)
            .map_err(|e| anyhow!("Failed to set hub.pty_tee: {e}"))?;
    }

    // hub.quit() - Request Hub shutdown.
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
    // window, the new Hub recovers agents on startup, and the user sees
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
                ::log::warn!("[Hub] exec_restart() called before hub_event_tx set — event dropped");
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
                ::log::warn!("[Hub] dev_rebuild() called before hub_event_tx set — event dropped");
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::new_hub_event_sender;
    use super::*;

    fn create_test_deps() -> (
        HubEventSender,
        Arc<HandleCache>,
        String,
        SharedServerId,
        Arc<RwLock<HubState>>,
    ) {
        let state = Arc::new(RwLock::new(HubState::new(std::path::PathBuf::from(
            "/tmp/test-worktrees",
        ))));
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

        register(&lua, tx, cache, hid, sid, state)
            .expect("Should register hub primitives");

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

    // Broker-specific register_session tests removed — broker module deleted.
    // Registration now requires a session_connection.

    /// `hub.pty_tee` is now a no-op stub (returns nil always).
    #[test]
    fn test_pty_tee_rejects_unsafe_path() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

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

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

        // Path has "workspaces" component but not "sessions".
        let result: LuaValue = lua
            .load(r#"return hub.pty_tee(1, "/data/workspaces/my-agent/pty-0.log", 0)"#)
            .eval()
            .unwrap();
        assert!(
            result.is_nil(),
            "path without sessions component must return nil"
        );
    }

    /// A crafted path that contains "workspaces" as a substring of a directory name
    /// (e.g. "evil-workspaces") must be rejected — component-level check required.
    #[test]
    fn test_pty_tee_rejects_workspaces_as_substring() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

        // "evil-workspaces" satisfies a naive contains("workspaces/") check but is
        // not the exact "workspaces" path component — must be rejected.
        let result: LuaValue = lua
            .load(r#"return hub.pty_tee(1, "/tmp/evil-workspaces/x/sessions/0/pty-0.log", 0)"#)
            .eval()
            .unwrap();
        assert!(
            result.is_nil(),
            "substring match on component name must be rejected"
        );
    }

    /// A path with ".." traversal components must be rejected.
    #[test]
    fn test_pty_tee_rejects_path_traversal() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

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

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

        let result: LuaValue = lua
            .load(r#"return hub.pty_tee(1, "workspaces/agent/sessions/0/pty-0.log", 0)"#)
            .eval()
            .unwrap();
        assert!(result.is_nil(), "relative path must be rejected");
    }

    /// `hub.pty_tee` is a no-op stub (always returns nil).
    #[test]
    fn test_pty_tee_returns_nil_stub() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

        let result: LuaValue = lua
            .load(r#"return hub.pty_tee(1, "/data/workspaces/key/sessions/0/pty-0.log", 0)"#)
            .eval()
            .unwrap();
        assert!(result.is_nil(), "pty_tee stub must return nil");
    }

    #[test]
    fn test_get_worktrees_returns_empty_array() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

        let worktrees: LuaTable = lua.load("return hub.get_worktrees()").eval().unwrap();
        assert_eq!(worktrees.len().unwrap(), 0);
    }

    /// Empty worktrees returns an empty Lua table (iterable, length 0).
    #[test]
    fn test_get_worktrees_empty_returns_table() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

        let worktrees: LuaTable = lua.load("return hub.get_worktrees()").eval().unwrap();
        assert_eq!(
            worktrees.len().unwrap(),
            0,
            "Empty worktrees should have length 0"
        );
    }

    #[test]
    fn test_quit_sends_event() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender.into());

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

        lua.load("hub.quit()")
            .exec()
            .expect("Should send quit event");

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

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

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

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

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

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

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

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

        let id: String = lua.load("return hub.server_id()").eval().unwrap();
        assert_eq!(id, "test-hub-id");
    }

    #[test]
    fn test_server_id_returns_nil_when_unset() {
        let lua = Lua::new();
        let (tx, cache, hid, _sid, state) = create_test_deps();
        let nil_sid: SharedServerId = Arc::new(Mutex::new(None));

        register(&lua, tx, cache, hid, nil_sid, state).expect("Should register");

        let id: LuaValue = lua.load("return hub.server_id()").eval().unwrap();
        assert!(id.is_nil());
    }

    #[test]
    fn test_handle_webrtc_offer_sends_event() {
        let lua = Lua::new();
        let (tx, cache, hid, sid, state) = create_test_deps();

        let (sender, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender.into());

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

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

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

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
                assert_eq!(candidate.get("sdpMid").and_then(|v| v.as_str()), Some("0"));
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

        register(&lua, tx, cache, hid, sid, state).expect("Should register");

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
