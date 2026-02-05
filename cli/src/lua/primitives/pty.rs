//! PTY primitives for Lua scripts.
//!
//! Exposes PTY terminal handling to Lua, allowing scripts to create forwarders,
//! spawn PTY sessions, send input, resize terminals, and optionally intercept
//! PTY output via hooks.
//!
//! # Design Principle: "Lua controls. Rust streams."
//!
//! For high-frequency PTY output:
//! - **Default (fast path)**: Rust streams directly to WebRTC, no Lua in data path
//! - **Optional (slow path)**: If "pty_output" hooks are registered, call them
//!
//! # PTY Session Handles
//!
//! Lua can spawn PTY sessions directly via `pty.spawn()`, receiving a
//! `PtySessionHandle` userdata that provides full control over the PTY:
//!
//! ```lua
//! local session = pty.spawn({
//!     worktree_path = "/path/to/worktree",
//!     command = "bash",
//!     rows = 24,
//!     cols = 80,
//!     detect_notifications = true,
//! })
//!
//! -- Write input to the PTY
//! session:write("ls -la\n")
//!
//! -- Resize the terminal
//! session:resize(40, 120)
//!
//! -- Get current dimensions
//! local rows, cols = session:dimensions()
//!
//! -- Read scrollback buffer
//! local scrollback = session:get_scrollback()
//!
//! -- Check forwarding port
//! local port = session:port()  -- number or nil
//!
//! -- Poll for OSC notifications
//! local notifications = session:poll_notifications()
//!
//! -- Kill the session
//! session:kill()
//! ```
//!
//! # PTY Forwarders
//!
//! ```lua
//! -- Create a PTY forwarder (Rust handles the streaming)
//! local forwarder = webrtc.create_pty_forwarder({
//!     peer_id = "browser-123",
//!     agent_index = 0,
//!     pty_index = 0,
//!     prefix = "\x01",  -- Optional: prefix for raw terminal data
//! })
//!
//! -- Check forwarder status
//! print(forwarder:id())         -- "browser-123:0:0"
//! print(forwarder:is_active())  -- true
//!
//! -- Stop forwarder (forwarder is also stopped automatically on cleanup)
//! forwarder:stop()
//!
//! -- Direct PTY operations
//! hub.write_pty(0, 0, "ls -la\n")      -- Send input to PTY
//! hub.resize_pty(0, 0, 24, 80)         -- Resize PTY
//! local key = hub.get_scrollback(0, 0) -- Request scrollback (async)
//! ```
//!
//! # Hook Integration
//!
//! If hooks are registered for "pty_output", Rust will call them for each output:
//!
//! ```lua
//! hooks.register("pty_output", function(ctx, data)
//!     -- ctx contains: agent_index, pty_index, peer_id
//!     -- data is the raw output bytes
//!     -- Return transformed data, or nil to drop
//!     return data
//! end)
//! ```

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::agent::notification::AgentNotification;
use crate::agent::pty::{PtySession, SharedPtyState};
use crate::agent::spawn::PtySpawnConfig;
use tokio::sync::broadcast;

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use crate::agent::pty::events::PtyEvent;

// =============================================================================
// PtySessionHandle - Lua-facing handle to a spawned PtySession
// =============================================================================

/// Lua-facing handle to a spawned PTY session.
///
/// Wraps the thread-safe components of a [`PtySession`], allowing Lua to
/// interact with the PTY (write input, resize, read scrollback, poll
/// notifications, etc.) without holding a direct reference to the session.
///
/// The `_session` field keeps the `PtySession` alive via `Arc` -- dropping
/// the last reference triggers `PtySession::drop()` which kills the child
/// process and aborts the command processor task.
///
/// # Thread Safety
///
/// All fields are `Send + Sync` as required by [`LuaUserData`]. The
/// `notification_rx` field wraps `std::sync::mpsc::Receiver` (which is
/// `!Sync`) in `Arc<Mutex<>>` to satisfy this constraint.
///
/// # Example (Lua)
///
/// ```lua
/// local session = pty.spawn({
///     worktree_path = "/tmp/work",
///     command = "bash",
///     rows = 24,
///     cols = 80,
/// })
/// session:write("echo hello\n")
/// local rows, cols = session:dimensions()
/// session:kill()
/// ```
pub struct PtySessionHandle {
    /// Keep `PtySession` alive -- its `Drop` impl kills the child process
    /// and aborts the command processor task.
    _session: Arc<Mutex<PtySession>>,

    /// Shared state for direct write/resize operations.
    shared_state: Arc<Mutex<SharedPtyState>>,

    /// Scrollback buffer for session replay.
    scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,

    /// Event broadcast sender for subscribing to PTY output.
    #[allow(dead_code)]
    event_tx: broadcast::Sender<PtyEvent>,

    /// Forwarding port (if configured).
    port: Option<u16>,

    /// Whether notifications are enabled on this session.
    has_notifications: bool,

    /// Notification receiver (moved out of `PtySession`, owned by handle).
    ///
    /// Wrapped in `Arc<Mutex<>>` to satisfy `LuaUserData`'s `Send + Sync`
    /// requirements (`std::sync::mpsc::Receiver` is `!Sync`).
    notification_rx: Option<Arc<Mutex<std::sync::mpsc::Receiver<AgentNotification>>>>,
}

impl std::fmt::Debug for PtySessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySessionHandle")
            .field("port", &self.port)
            .field("has_notifications", &self.has_notifications)
            .field("has_notification_rx", &self.notification_rx.is_some())
            .finish()
    }
}

impl PtySessionHandle {
    /// Create a `PtyHandle` from this session handle.
    ///
    /// Used to register Lua-created PTY sessions with `HandleCache` for
    /// access by Rust-side PTY operations (forwarders, write, resize).
    #[must_use]
    pub fn to_pty_handle(&self) -> crate::hub::agent_handle::PtyHandle {
        crate::hub::agent_handle::PtyHandle::new(
            self.event_tx.clone(),
            Arc::clone(&self.shared_state),
            Arc::clone(&self.scrollback_buffer),
            self.port,
        )
    }
}

impl LuaUserData for PtySessionHandle {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        // session:write(data) - Write bytes/string to the PTY.
        methods.add_method("write", |_, this, data: LuaString| {
            let bytes = data.as_bytes().to_vec();
            let mut state = this
                .shared_state
                .lock()
                .expect("PtySessionHandle shared_state lock poisoned");
            if let Some(writer) = &mut state.writer {
                writer
                    .write_all(&bytes)
                    .map_err(|e| LuaError::runtime(format!("Failed to write to PTY: {e}")))?;
                writer
                    .flush()
                    .map_err(|e| LuaError::runtime(format!("Failed to flush PTY writer: {e}")))?;
            }
            Ok(())
        });

        // session:resize(rows, cols) - Resize the PTY.
        methods.add_method("resize", |_, this, (rows, cols): (u16, u16)| {
            let mut state = this
                .shared_state
                .lock()
                .expect("PtySessionHandle shared_state lock poisoned");

            state.dimensions = (rows, cols);

            if let Some(master_pty) = &state.master_pty {
                if let Err(e) = master_pty.resize(portable_pty::PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                }) {
                    return Err(LuaError::runtime(format!("Failed to resize PTY: {e}")));
                }
            }
            Ok(())
        });

        // session:dimensions() -> rows, cols
        methods.add_method("dimensions", |_, this, ()| {
            let state = this
                .shared_state
                .lock()
                .expect("PtySessionHandle shared_state lock poisoned");
            let (rows, cols) = state.dimensions;
            Ok((rows, cols))
        });

        // session:get_scrollback() -> string (raw bytes)
        methods.add_method("get_scrollback", |lua, this, ()| {
            let buffer = this
                .scrollback_buffer
                .lock()
                .expect("PtySessionHandle scrollback_buffer lock poisoned");
            let bytes: Vec<u8> = buffer.iter().copied().collect();
            lua.create_string(&bytes)
        });

        // session:port() -> number or nil
        methods.add_method("port", |_, this, ()| Ok(this.port));

        // session:is_alive() -> boolean
        //
        // Checks whether the PTY writer is still available, which indicates
        // the session has been spawned and hasn't been killed.
        methods.add_method("is_alive", |_, this, ()| {
            let state = this
                .shared_state
                .lock()
                .expect("PtySessionHandle shared_state lock poisoned");
            Ok(state.writer.is_some())
        });

        // session:poll_notifications() -> table of notifications
        //
        // Drains all pending notifications from the channel. Returns an
        // array-like table where each entry is a table with:
        //   { type = "osc9", message = "..." }
        //   { type = "osc777", title = "...", body = "..." }
        methods.add_method("poll_notifications", |lua, this, ()| {
            let table = lua.create_table()?;

            if !this.has_notifications {
                return Ok(table);
            }

            if let Some(ref rx_arc) = this.notification_rx {
                let rx = rx_arc
                    .lock()
                    .expect("PtySessionHandle notification_rx lock poisoned");
                let mut idx = 1;
                while let Ok(notif) = rx.try_recv() {
                    let entry = lua.create_table()?;
                    match notif {
                        AgentNotification::Osc9(msg) => {
                            entry.set("type", "osc9")?;
                            entry.set("message", msg)?;
                        }
                        AgentNotification::Osc777 { title, body } => {
                            entry.set("type", "osc777")?;
                            entry.set("title", title)?;
                            entry.set("body", body)?;
                        }
                    }
                    table.set(idx, entry)?;
                    idx += 1;
                }
            }

            Ok(table)
        });

        // session:kill() - Kill the child process.
        //
        // Locks the PtySession and calls kill_child(). After this call,
        // is_alive() will return false and write() will be a no-op.
        methods.add_method("kill", |_, this, ()| {
            let mut session = this
                ._session
                .lock()
                .expect("PtySessionHandle session lock poisoned");
            session.kill_child();
            Ok(())
        });
    }
}

/// Forwarder handle returned to Lua as userdata.
///
/// Represents an active PTY-to-WebRTC forwarder. Lua can check status
/// and stop the forwarder. The actual streaming is handled by Rust.
pub struct PtyForwarder {
    /// Unique forwarder identifier: "{peer_id}:{agent_index}:{pty_index}".
    pub id: String,
    /// Browser peer that receives the PTY output.
    pub peer_id: String,
    /// Agent index in Hub's agent list.
    pub agent_index: usize,
    /// PTY index within the agent (0=CLI, 1=Server).
    pub pty_index: usize,
    /// Whether this forwarder is still active.
    /// Set to false when stop() is called or Hub cleans up.
    pub active: Arc<Mutex<bool>>,
}

impl LuaUserData for PtyForwarder {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        // forwarder:stop() - Request forwarder shutdown
        methods.add_method("stop", |_, this, ()| {
            let mut active = this.active.lock()
                .expect("PTY forwarder active flag mutex poisoned");
            *active = false;
            Ok(())
        });

        // forwarder:is_active() - Check if forwarder is still running
        methods.add_method("is_active", |_, this, ()| {
            let active = this.active.lock()
                .expect("PTY forwarder active flag mutex poisoned");
            Ok(*active)
        });

        // forwarder:id() - Get forwarder identifier
        methods.add_method("id", |_, this, ()| Ok(this.id.clone()));

        // forwarder:peer_id() - Get the target peer ID
        methods.add_method("peer_id", |_, this, ()| Ok(this.peer_id.clone()));

        // forwarder:agent_index() - Get the agent index
        methods.add_method("agent_index", |_, this, ()| Ok(this.agent_index));

        // forwarder:pty_index() - Get the PTY index
        methods.add_method("pty_index", |_, this, ()| Ok(this.pty_index));
    }
}

/// Request to create a PTY forwarder (queued for Hub to process).
#[derive(Debug, Clone)]
pub struct CreateForwarderRequest {
    /// Browser peer that will receive the PTY output.
    pub peer_id: String,
    /// Agent index in Hub's agent list.
    pub agent_index: usize,
    /// PTY index within the agent (0=CLI, 1=Server).
    pub pty_index: usize,
    /// Optional prefix byte for raw terminal data (typically 0x01).
    pub prefix: Option<Vec<u8>>,
    /// Browser-generated subscription ID for message routing.
    ///
    /// The browser generates this ID when subscribing (e.g., "sub_2_1770164017").
    /// All messages sent back to the browser must include this exact ID so the
    /// browser's subscriptionHandlers map can route them correctly.
    pub subscription_id: String,
    /// Shared active flag for the forwarder handle.
    pub active_flag: Arc<Mutex<bool>>,
}

/// Request to create a TUI PTY forwarder (queued for Hub to process).
///
/// Unlike `CreateForwarderRequest`, this doesn't need a peer_id (single TUI)
/// and routes output through `tui_output_tx` instead of WebRTC.
#[derive(Debug, Clone)]
pub struct CreateTuiForwarderRequest {
    /// Agent index in Hub's agent list (legacy, for forwarder ID).
    pub agent_index: usize,
    /// PTY index within the agent (0=CLI, 1=Server).
    pub pty_index: usize,
    /// Subscription ID for tracking (Lua-generated).
    pub subscription_id: String,
    /// Shared active flag for the forwarder handle.
    pub active_flag: Arc<Mutex<bool>>,
}

/// Request to create a TUI PTY forwarder with direct session access.
///
/// This variant takes the PTY components directly from Lua's PtySessionHandle,
/// avoiding the need to register agents with HandleCache.
#[derive(Clone)]
pub struct CreateTuiForwarderDirectRequest {
    /// Agent key for identification.
    pub agent_key: String,
    /// Session name (e.g., "cli", "server").
    pub session_name: String,
    /// Subscription ID for tracking.
    pub subscription_id: String,
    /// Shared active flag for the forwarder handle.
    pub active_flag: Arc<Mutex<bool>>,
    /// Event sender from the PTY session (for subscribing).
    pub event_tx: broadcast::Sender<PtyEvent>,
    /// Scrollback buffer for initial replay.
    pub scrollback_buffer: Arc<Mutex<VecDeque<u8>>>,
    /// HTTP port if this is a server session.
    pub port: Option<u16>,
}

impl std::fmt::Debug for CreateTuiForwarderDirectRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateTuiForwarderDirectRequest")
            .field("agent_key", &self.agent_key)
            .field("session_name", &self.session_name)
            .field("subscription_id", &self.subscription_id)
            .field("port", &self.port)
            .finish()
    }
}

/// PTY operations queued from Lua.
///
/// These are processed by Hub in its event loop after Lua callbacks return.
#[derive(Debug)]
pub enum PtyRequest {
    /// Create a new PTY forwarder for streaming to WebRTC.
    CreateForwarder(CreateForwarderRequest),

    /// Create a new PTY forwarder for streaming to TUI (legacy index-based).
    CreateTuiForwarder(CreateTuiForwarderRequest),

    /// Create a new PTY forwarder for streaming to TUI (direct session access).
    CreateTuiForwarderDirect(CreateTuiForwarderDirectRequest),

    /// Stop an existing PTY forwarder.
    StopForwarder {
        /// Forwarder identifier: "{peer_id}:{agent_index}:{pty_index}".
        forwarder_id: String,
    },

    /// Write input data to a PTY.
    WritePty {
        /// Agent index in Hub's agent list.
        agent_index: usize,
        /// PTY index within the agent.
        pty_index: usize,
        /// Input data to write.
        data: Vec<u8>,
    },

    /// Resize a PTY.
    ResizePty {
        /// Agent index in Hub's agent list.
        agent_index: usize,
        /// PTY index within the agent.
        pty_index: usize,
        /// New number of rows.
        rows: u16,
        /// New number of columns.
        cols: u16,
    },

    /// Request scrollback buffer (async - Hub will call back with data).
    GetScrollback {
        /// Agent index in Hub's agent list.
        agent_index: usize,
        /// PTY index within the agent.
        pty_index: usize,
        /// Key for storing the response in Lua registry.
        response_key: String,
    },
}

// Implement Clone for PtyRequest to satisfy the requirement
impl Clone for PtyRequest {
    fn clone(&self) -> Self {
        match self {
            Self::CreateForwarder(req) => Self::CreateForwarder(req.clone()),
            Self::CreateTuiForwarder(req) => Self::CreateTuiForwarder(req.clone()),
            Self::CreateTuiForwarderDirect(req) => Self::CreateTuiForwarderDirect(req.clone()),
            Self::StopForwarder { forwarder_id } => Self::StopForwarder {
                forwarder_id: forwarder_id.clone(),
            },
            Self::WritePty {
                agent_index,
                pty_index,
                data,
            } => Self::WritePty {
                agent_index: *agent_index,
                pty_index: *pty_index,
                data: data.clone(),
            },
            Self::ResizePty {
                agent_index,
                pty_index,
                rows,
                cols,
            } => Self::ResizePty {
                agent_index: *agent_index,
                pty_index: *pty_index,
                rows: *rows,
                cols: *cols,
            },
            Self::GetScrollback {
                agent_index,
                pty_index,
                response_key,
            } => Self::GetScrollback {
                agent_index: *agent_index,
                pty_index: *pty_index,
                response_key: response_key.clone(),
            },
        }
    }
}

/// Shared request queue for PTY operations from Lua.
pub type PtyRequestQueue = Arc<Mutex<Vec<PtyRequest>>>;

/// Create a new PTY request queue.
#[must_use]
pub fn new_request_queue() -> PtyRequestQueue {
    Arc::new(Mutex::new(Vec::new()))
}

/// Register PTY primitives with the Lua state.
///
/// Adds the following functions:
/// - `pty.spawn(config)` - Spawn a PTY session, returns `PtySessionHandle` userdata
/// - `webrtc.create_pty_forwarder(opts)` - Create a PTY-to-WebRTC forwarder
/// - `tui.create_pty_forwarder(opts)` - Create a PTY-to-TUI forwarder
/// - `hub.write_pty(agent_index, pty_index, data)` - Write input to PTY
/// - `hub.resize_pty(agent_index, pty_index, rows, cols)` - Resize PTY
/// - `hub.get_scrollback(agent_index, pty_index)` - Get scrollback buffer
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `request_queue` - Shared queue for PTY operations (processed by Hub)
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub fn register(lua: &Lua, request_queue: PtyRequestQueue) -> Result<()> {
    // Get or create the webrtc table
    let webrtc: LuaTable = lua
        .globals()
        .get("webrtc")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    // webrtc.create_pty_forwarder({ peer_id, agent_index, pty_index, subscription_id, prefix? })
    let queue = request_queue.clone();
    let create_forwarder_fn = lua
        .create_function(move |_lua, opts: LuaTable| {
            let peer_id: String = opts
                .get("peer_id")
                .map_err(|_| LuaError::runtime("peer_id is required"))?;
            let agent_index: usize = opts
                .get("agent_index")
                .map_err(|_| LuaError::runtime("agent_index is required"))?;
            let pty_index: usize = opts
                .get("pty_index")
                .map_err(|_| LuaError::runtime("pty_index is required"))?;
            let subscription_id: String = opts
                .get("subscription_id")
                .map_err(|_| LuaError::runtime("subscription_id is required"))?;
            let prefix: Option<LuaString> = opts.get("prefix").ok();

            let forwarder_id = format!("{}:{}:{}", peer_id, agent_index, pty_index);
            let active_flag = Arc::new(Mutex::new(true));

            // Queue the request for Hub to process
            {
                let mut q = queue.lock()
                    .expect("PTY request queue mutex poisoned");
                q.push(PtyRequest::CreateForwarder(CreateForwarderRequest {
                    peer_id: peer_id.clone(),
                    agent_index,
                    pty_index,
                    prefix: prefix.map(|p| p.as_bytes().to_vec()),
                    subscription_id,
                    active_flag: Arc::clone(&active_flag),
                }));
            }

            // Return forwarder handle immediately
            // The actual forwarder task is spawned when Hub processes the request
            let forwarder = PtyForwarder {
                id: forwarder_id,
                peer_id,
                agent_index,
                pty_index,
                active: active_flag,
            };

            Ok(forwarder)
        })
        .map_err(|e| anyhow!("Failed to create webrtc.create_pty_forwarder function: {e}"))?;

    webrtc
        .set("create_pty_forwarder", create_forwarder_fn)
        .map_err(|e| anyhow!("Failed to set webrtc.create_pty_forwarder: {e}"))?;

    // Ensure webrtc table is globally registered
    lua.globals()
        .set("webrtc", webrtc)
        .map_err(|e| anyhow!("Failed to register webrtc table globally: {e}"))?;

    // Get or create the tui table (may already be created by tui.rs)
    let tui: LuaTable = lua
        .globals()
        .get("tui")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    // tui.create_pty_forwarder({ agent_index, pty_index, subscription_id })
    //
    // Like webrtc.create_pty_forwarder but routes output through TUI send queue.
    // No peer_id needed — there's only one TUI client.
    let queue_tui = request_queue.clone();
    let create_tui_forwarder_fn = lua
        .create_function(move |_lua, opts: LuaTable| {
            let agent_index: usize = opts
                .get("agent_index")
                .map_err(|_| LuaError::runtime("agent_index is required"))?;
            let pty_index: usize = opts
                .get("pty_index")
                .map_err(|_| LuaError::runtime("pty_index is required"))?;
            let subscription_id: String = opts
                .get("subscription_id")
                .map_err(|_| LuaError::runtime("subscription_id is required"))?;

            let forwarder_id = format!("tui:{}:{}", agent_index, pty_index);
            let active_flag = Arc::new(Mutex::new(true));

            // Queue the request for Hub to process
            {
                let mut q = queue_tui
                    .lock()
                    .expect("PTY request queue mutex poisoned");
                q.push(PtyRequest::CreateTuiForwarder(CreateTuiForwarderRequest {
                    agent_index,
                    pty_index,
                    subscription_id,
                    active_flag: Arc::clone(&active_flag),
                }));
            }

            // Return forwarder handle immediately
            let forwarder = PtyForwarder {
                id: forwarder_id,
                peer_id: "tui".to_string(),
                agent_index,
                pty_index,
                active: active_flag,
            };

            Ok(forwarder)
        })
        .map_err(|e| anyhow!("Failed to create tui.create_pty_forwarder function: {e}"))?;

    tui.set("create_pty_forwarder", create_tui_forwarder_fn)
        .map_err(|e| anyhow!("Failed to set tui.create_pty_forwarder: {e}"))?;

    // tui.forward_session({ agent_key, session_name, session, subscription_id })
    //
    // Create a PTY forwarder by passing the session handle directly.
    // This is the preferred method - no HandleCache registration needed.
    //
    // Arguments:
    //   agent_key: string - Agent key for identification
    //   session_name: string - Session name (e.g., "cli", "server")
    //   session: PtySessionHandle - The session userdata from pty.spawn()
    //   subscription_id: string - Subscription ID for message routing
    let queue_direct = request_queue.clone();
    let forward_session_fn = lua
        .create_function(move |_lua, opts: LuaTable| {
            let agent_key: String = opts
                .get("agent_key")
                .map_err(|_| LuaError::runtime("agent_key is required"))?;
            let session_name: String = opts
                .get("session_name")
                .map_err(|_| LuaError::runtime("session_name is required"))?;
            let subscription_id: String = opts
                .get("subscription_id")
                .map_err(|_| LuaError::runtime("subscription_id is required"))?;

            // Extract the PtySessionHandle userdata
            let session_ud: LuaAnyUserData = opts
                .get("session")
                .map_err(|_| LuaError::runtime("session is required"))?;
            let session_handle = session_ud
                .borrow::<PtySessionHandle>()
                .map_err(|e| LuaError::runtime(format!("session must be a PtySessionHandle: {e}")))?;

            let forwarder_id = format!("tui:{}:{}", agent_key, session_name);
            let active_flag = Arc::new(Mutex::new(true));

            // Queue the request with direct PTY access
            {
                let mut q = queue_direct
                    .lock()
                    .expect("PTY request queue mutex poisoned");
                q.push(PtyRequest::CreateTuiForwarderDirect(CreateTuiForwarderDirectRequest {
                    agent_key: agent_key.clone(),
                    session_name: session_name.clone(),
                    subscription_id,
                    active_flag: Arc::clone(&active_flag),
                    event_tx: session_handle.event_tx.clone(),
                    scrollback_buffer: Arc::clone(&session_handle.scrollback_buffer),
                    port: session_handle.port,
                }));
            }

            log::info!("[Lua] Queued TUI forwarder for {}:{}", agent_key, session_name);

            // Return forwarder handle immediately
            let forwarder = PtyForwarder {
                id: forwarder_id,
                peer_id: "tui".to_string(),
                agent_index: 0, // Not used for direct forwarders
                pty_index: 0,
                active: active_flag,
            };

            Ok(forwarder)
        })
        .map_err(|e| anyhow!("Failed to create tui.forward_session function: {e}"))?;

    tui.set("forward_session", forward_session_fn)
        .map_err(|e| anyhow!("Failed to set tui.forward_session: {e}"))?;

    // Ensure tui table is globally registered
    lua.globals()
        .set("tui", tui)
        .map_err(|e| anyhow!("Failed to register tui table globally: {e}"))?;

    // Get or create the hub table
    let hub: LuaTable = lua
        .globals()
        .get("hub")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    // hub.write_pty(agent_index, pty_index, data)
    let queue2 = request_queue.clone();
    let write_pty_fn = lua
        .create_function(
            move |_, (agent_index, pty_index, data): (usize, usize, LuaString)| {
                let mut q = queue2.lock()
                    .expect("PTY request queue mutex poisoned");
                q.push(PtyRequest::WritePty {
                    agent_index,
                    pty_index,
                    data: data.as_bytes().to_vec(),
                });
                Ok(())
            },
        )
        .map_err(|e| anyhow!("Failed to create hub.write_pty function: {e}"))?;

    hub.set("write_pty", write_pty_fn)
        .map_err(|e| anyhow!("Failed to set hub.write_pty: {e}"))?;

    // hub.resize_pty(agent_index, pty_index, rows, cols)
    let queue3 = request_queue.clone();
    let resize_pty_fn = lua
        .create_function(
            move |_, (agent_index, pty_index, rows, cols): (usize, usize, u16, u16)| {
                let mut q = queue3.lock()
                    .expect("PTY request queue mutex poisoned");
                q.push(PtyRequest::ResizePty {
                    agent_index,
                    pty_index,
                    rows,
                    cols,
                });
                Ok(())
            },
        )
        .map_err(|e| anyhow!("Failed to create hub.resize_pty function: {e}"))?;

    hub.set("resize_pty", resize_pty_fn)
        .map_err(|e| anyhow!("Failed to set hub.resize_pty: {e}"))?;

    // hub.get_scrollback(agent_index, pty_index) -> response_key
    let queue4 = request_queue;
    let get_scrollback_fn = lua
        .create_function(move |_, (agent_index, pty_index): (usize, usize)| {
            // Generate a unique response key for this request
            let response_key = format!("scrollback:{}:{}:{}", agent_index, pty_index, uuid::Uuid::new_v4());

            let mut q = queue4.lock()
                .expect("PTY request queue mutex poisoned");
            q.push(PtyRequest::GetScrollback {
                agent_index,
                pty_index,
                response_key: response_key.clone(),
            });

            // Return the key so Lua can retrieve the response later
            Ok(response_key)
        })
        .map_err(|e| anyhow!("Failed to create hub.get_scrollback function: {e}"))?;

    hub.set("get_scrollback", get_scrollback_fn)
        .map_err(|e| anyhow!("Failed to set hub.get_scrollback: {e}"))?;

    // Ensure hub table is globally registered
    lua.globals()
        .set("hub", hub)
        .map_err(|e| anyhow!("Failed to register hub table globally: {e}"))?;

    // =========================================================================
    // pty.spawn() - Spawn a PTY session, returns PtySessionHandle userdata
    // =========================================================================

    // Get or create the pty table
    let pty_table: LuaTable = lua
        .globals()
        .get("pty")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    // pty.spawn(config_table) -> PtySessionHandle
    //
    // config_table fields:
    //   worktree_path: string (required) - Working directory
    //   command: string (default "bash") - Command to run
    //   env: table<string, string> (default {}) - Environment variables
    //   init_commands: table<string> (default {}) - Commands to run after spawn
    //   detect_notifications: boolean (default false) - Enable OSC detection
    //   port: number (optional) - HTTP forwarding port
    //   context: string (default "") - Context written before init commands
    //   rows: number (default 24) - Initial rows
    //   cols: number (default 80) - Initial cols
    let spawn_fn = lua
        .create_function(|_lua, opts: LuaTable| {
            // Parse required fields
            let worktree_path: String = opts
                .get("worktree_path")
                .map_err(|_| LuaError::runtime("worktree_path is required"))?;

            // Parse optional fields with defaults
            let command: String = opts.get("command").unwrap_or_else(|_| "bash".to_string());
            let rows: u16 = opts.get("rows").unwrap_or(24);
            let cols: u16 = opts.get("cols").unwrap_or(80);
            let detect_notifications: bool =
                opts.get("detect_notifications").unwrap_or(false);
            let port: Option<u16> = opts.get("port").ok();
            let context: String = opts.get("context").unwrap_or_default();

            // Parse env table
            let mut env = HashMap::new();
            if let Ok(env_table) = opts.get::<LuaTable>("env") {
                for pair in env_table.pairs::<String, String>() {
                    if let Ok((k, v)) = pair {
                        env.insert(k, v);
                    }
                }
            }

            // Parse init_commands array
            let mut init_commands = Vec::new();
            if let Ok(cmds_table) = opts.get::<LuaTable>("init_commands") {
                for pair in cmds_table.pairs::<i64, String>() {
                    if let Ok((_, cmd)) = pair {
                        init_commands.push(cmd);
                    }
                }
            }

            // Build the spawn config
            let config = PtySpawnConfig {
                worktree_path: PathBuf::from(worktree_path),
                command,
                env,
                init_commands,
                detect_notifications,
                port,
                context,
            };

            // Create and spawn the PtySession
            let mut session = PtySession::new(rows, cols);
            session.spawn(config).map_err(|e| {
                LuaError::runtime(format!("Failed to spawn PTY session: {e}"))
            })?;

            // Extract direct access handles before wrapping in Arc
            let (shared_state, scrollback_buffer, event_tx) = session.get_direct_access();
            let session_port = session.port();
            let has_notifications = session.notification_tx.is_some();

            // Take the notification_rx from the session and wrap for thread safety
            let notification_rx = session.take_notification_rx().map(|rx| {
                Arc::new(Mutex::new(rx))
            });

            // Wrap session in Arc<Mutex<>> to keep it alive (Drop kills child)
            let session_arc = Arc::new(Mutex::new(session));

            let handle = PtySessionHandle {
                _session: session_arc,
                shared_state,
                scrollback_buffer,
                event_tx,
                port: session_port,
                has_notifications,
                notification_rx,
            };

            Ok(handle)
        })
        .map_err(|e| anyhow!("Failed to create pty.spawn function: {e}"))?;

    pty_table
        .set("spawn", spawn_fn)
        .map_err(|e| anyhow!("Failed to set pty.spawn: {e}"))?;

    // Ensure pty table is globally registered
    lua.globals()
        .set("pty", pty_table)
        .map_err(|e| anyhow!("Failed to register pty table globally: {e}"))?;

    Ok(())
}

/// Context passed to PTY output hooks.
#[derive(Debug, Clone)]
pub struct PtyOutputContext {
    /// Agent index in Hub's agent list.
    pub agent_index: usize,
    /// PTY index within the agent.
    pub pty_index: usize,
    /// Browser peer receiving this output.
    pub peer_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pty_forwarder_userdata() {
        let lua = Lua::new();

        let forwarder = PtyForwarder {
            id: "test-peer:0:0".to_string(),
            peer_id: "test-peer".to_string(),
            agent_index: 0,
            pty_index: 0,
            active: Arc::new(Mutex::new(true)),
        };

        lua.globals().set("forwarder", forwarder).expect("Failed to set forwarder");

        // Test id() method
        let id: String = lua.load("return forwarder:id()").eval().expect("Failed to get id");
        assert_eq!(id, "test-peer:0:0");

        // Test is_active() method
        let active: bool = lua.load("return forwarder:is_active()").eval().expect("Failed to check is_active");
        assert!(active);

        // Test stop() method
        lua.load("forwarder:stop()").exec().expect("Failed to call stop");
        let active: bool = lua.load("return forwarder:is_active()").eval().expect("Failed to check is_active after stop");
        assert!(!active);

        // Test other accessors
        let peer_id: String = lua.load("return forwarder:peer_id()").eval().unwrap();
        assert_eq!(peer_id, "test-peer");

        let agent_idx: usize = lua.load("return forwarder:agent_index()").eval().unwrap();
        assert_eq!(agent_idx, 0);

        let pty_idx: usize = lua.load("return forwarder:pty_index()").eval().unwrap();
        assert_eq!(pty_idx, 0);
    }

    #[test]
    fn test_create_pty_forwarder_queues_request() {
        let lua = Lua::new();
        let queue = new_request_queue();

        // Register primitives
        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        // Create forwarder
        lua.load(
            r#"
            forwarder = webrtc.create_pty_forwarder({
                peer_id = "browser-123",
                agent_index = 0,
                pty_index = 1,
                subscription_id = "sub_1_1234567890",
            })
        "#,
        )
        .exec()
        .expect("Should create forwarder");

        // Check queue has the request
        let requests = queue.lock()
            .expect("PTY request queue mutex poisoned");
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            PtyRequest::CreateForwarder(req) => {
                assert_eq!(req.peer_id, "browser-123");
                assert_eq!(req.agent_index, 0);
                assert_eq!(req.pty_index, 1);
                assert_eq!(req.subscription_id, "sub_1_1234567890");
                assert!(req.prefix.is_none());
            }
            _ => panic!("Expected CreateForwarder request"),
        }

        // Check forwarder handle is returned
        let id: String = lua.load("return forwarder:id()").eval().unwrap();
        assert_eq!(id, "browser-123:0:1");
    }

    #[test]
    fn test_create_pty_forwarder_with_prefix() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        lua.load(
            r#"
            webrtc.create_pty_forwarder({
                peer_id = "browser-456",
                agent_index = 1,
                pty_index = 0,
                subscription_id = "sub_2_9876543210",
                prefix = "\x01",
            })
        "#,
        )
        .exec()
        .expect("Should create forwarder with prefix");

        let requests = queue.lock()
            .expect("PTY request queue mutex poisoned");
        match &requests[0] {
            PtyRequest::CreateForwarder(req) => {
                assert_eq!(req.prefix, Some(vec![0x01]));
                assert_eq!(req.subscription_id, "sub_2_9876543210");
            }
            _ => panic!("Expected CreateForwarder request"),
        }
    }

    #[test]
    fn test_write_pty_queues_request() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        lua.load(r#"hub.write_pty(0, 1, "ls -la\n")"#)
            .exec()
            .expect("Should write PTY");

        let requests = queue.lock()
            .expect("PTY request queue mutex poisoned");
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            PtyRequest::WritePty {
                agent_index,
                pty_index,
                data,
            } => {
                assert_eq!(*agent_index, 0);
                assert_eq!(*pty_index, 1);
                assert_eq!(data, b"ls -la\n");
            }
            _ => panic!("Expected WritePty request"),
        }
    }

    #[test]
    fn test_resize_pty_queues_request() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        lua.load(r#"hub.resize_pty(0, 0, 40, 120)"#)
            .exec()
            .expect("Should resize PTY");

        let requests = queue.lock()
            .expect("PTY request queue mutex poisoned");
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            PtyRequest::ResizePty {
                agent_index,
                pty_index,
                rows,
                cols,
            } => {
                assert_eq!(*agent_index, 0);
                assert_eq!(*pty_index, 0);
                assert_eq!(*rows, 40);
                assert_eq!(*cols, 120);
            }
            _ => panic!("Expected ResizePty request"),
        }
    }

    #[test]
    fn test_get_scrollback_queues_request_and_returns_key() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let key: String = lua
            .load(r#"return hub.get_scrollback(1, 0)"#)
            .eval()
            .expect("Should get scrollback");

        // Key should start with the expected prefix
        assert!(key.starts_with("scrollback:1:0:"), "Key should have correct prefix");

        let requests = queue.lock()
            .expect("PTY request queue mutex poisoned");
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            PtyRequest::GetScrollback {
                agent_index,
                pty_index,
                response_key,
            } => {
                assert_eq!(*agent_index, 1);
                assert_eq!(*pty_index, 0);
                assert_eq!(response_key, &key);
            }
            _ => panic!("Expected GetScrollback request"),
        }
    }

    #[test]
    fn test_multiple_requests_queue_in_order() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        lua.load(
            r#"
            webrtc.create_pty_forwarder({ peer_id = "p1", agent_index = 0, pty_index = 0, subscription_id = "sub_1" })
            hub.write_pty(0, 0, "test")
            hub.resize_pty(0, 0, 24, 80)
        "#,
        )
        .exec()
        .expect("Should queue multiple requests");

        let requests = queue.lock()
            .expect("PTY request queue mutex poisoned");
        assert_eq!(requests.len(), 3);

        assert!(matches!(requests[0], PtyRequest::CreateForwarder(_)));
        assert!(matches!(requests[1], PtyRequest::WritePty { .. }));
        assert!(matches!(requests[2], PtyRequest::ResizePty { .. }));
    }

    #[test]
    fn test_create_forwarder_requires_peer_id() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let result: mlua::Result<()> = lua
            .load(
                r#"
            webrtc.create_pty_forwarder({ agent_index = 0, pty_index = 0 })
        "#,
            )
            .exec();

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("peer_id"), "Error should mention peer_id: {}", err_msg);
    }

    #[test]
    fn test_create_forwarder_requires_agent_index() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let result: mlua::Result<()> = lua
            .load(
                r#"
            webrtc.create_pty_forwarder({ peer_id = "test", pty_index = 0, subscription_id = "sub_1" })
        "#,
            )
            .exec();

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("agent_index"),
            "Error should mention agent_index: {}",
            err_msg
        );
    }

    #[test]
    fn test_create_forwarder_requires_subscription_id() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let result: mlua::Result<()> = lua
            .load(
                r#"
            webrtc.create_pty_forwarder({ peer_id = "test", agent_index = 0, pty_index = 0 })
        "#,
            )
            .exec();

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("subscription_id"),
            "Error should mention subscription_id: {}",
            err_msg
        );
    }

    // =========================================================================
    // TUI PTY Forwarder Tests
    // =========================================================================

    #[test]
    fn test_tui_create_pty_forwarder_exists() {
        let lua = Lua::new();
        let queue = new_request_queue();

        // Register TUI table stub first (pty.rs appends to it)
        lua.globals()
            .set("tui", lua.create_table().unwrap())
            .unwrap();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let tui: mlua::Table = lua.globals().get("tui").expect("tui should exist");
        let _: mlua::Function = tui
            .get("create_pty_forwarder")
            .expect("tui.create_pty_forwarder should exist");
    }

    #[test]
    fn test_tui_create_pty_forwarder_queues_request() {
        let lua = Lua::new();
        let queue = new_request_queue();

        lua.globals()
            .set("tui", lua.create_table().unwrap())
            .unwrap();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        lua.load(
            r#"
            forwarder = tui.create_pty_forwarder({
                agent_index = 0,
                pty_index = 1,
                subscription_id = "tui_term_1",
            })
        "#,
        )
        .exec()
        .expect("Should create TUI forwarder");

        let requests = queue
            .lock()
            .expect("PTY request queue mutex poisoned");
        assert_eq!(requests.len(), 1);

        match &requests[0] {
            PtyRequest::CreateTuiForwarder(req) => {
                assert_eq!(req.agent_index, 0);
                assert_eq!(req.pty_index, 1);
                assert_eq!(req.subscription_id, "tui_term_1");
            }
            _ => panic!("Expected CreateTuiForwarder request"),
        }

        // Verify forwarder handle
        let id: String = lua.load("return forwarder:id()").eval().unwrap();
        assert_eq!(id, "tui:0:1");

        let peer: String = lua.load("return forwarder:peer_id()").eval().unwrap();
        assert_eq!(peer, "tui");

        let active: bool = lua.load("return forwarder:is_active()").eval().unwrap();
        assert!(active);
    }

    #[test]
    fn test_tui_create_pty_forwarder_requires_agent_index() {
        let lua = Lua::new();
        let queue = new_request_queue();

        lua.globals()
            .set("tui", lua.create_table().unwrap())
            .unwrap();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let result: mlua::Result<()> = lua
            .load(
                r#"
            tui.create_pty_forwarder({ pty_index = 0, subscription_id = "sub_1" })
        "#,
            )
            .exec();

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("agent_index"),
            "Error should mention agent_index: {}",
            err_msg
        );
    }

    #[test]
    fn test_tui_forwarder_stop_sets_inactive() {
        let lua = Lua::new();
        let queue = new_request_queue();

        lua.globals()
            .set("tui", lua.create_table().unwrap())
            .unwrap();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        lua.load(
            r#"
            fwd = tui.create_pty_forwarder({
                agent_index = 0,
                pty_index = 0,
                subscription_id = "sub_tui",
            })
        "#,
        )
        .exec()
        .unwrap();

        let active: bool = lua.load("return fwd:is_active()").eval().unwrap();
        assert!(active);

        lua.load("fwd:stop()").exec().unwrap();

        let active: bool = lua.load("return fwd:is_active()").eval().unwrap();
        assert!(!active);
    }

    // =========================================================================
    // PtySessionHandle Tests
    // =========================================================================

    /// Helper to create a PtySessionHandle for testing without spawning a real
    /// process. Uses the direct PtySession constructor and sets up shared state
    /// manually.
    fn create_test_session_handle(with_notifications: bool) -> PtySessionHandle {
        let session = PtySession::new(24, 80);
        let (shared_state, scrollback_buffer, event_tx) = session.get_direct_access();
        let session_arc = Arc::new(Mutex::new(session));

        let notification_rx = if with_notifications {
            let (tx, rx) = std::sync::mpsc::channel();
            // Send a test notification
            tx.send(AgentNotification::Osc9(Some("test".to_string())))
                .unwrap();
            drop(tx);
            Some(Arc::new(Mutex::new(rx)))
        } else {
            None
        };

        PtySessionHandle {
            _session: session_arc,
            shared_state,
            scrollback_buffer,
            event_tx,
            port: None,
            has_notifications: with_notifications,
            notification_rx,
        }
    }

    #[test]
    fn test_pty_session_handle_dimensions() {
        let lua = Lua::new();
        let handle = create_test_session_handle(false);

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // Test dimensions method
        let result: (u16, u16) = lua
            .load("return session:dimensions()")
            .eval()
            .expect("dimensions should work");
        assert_eq!(result, (24, 80));
    }

    #[test]
    fn test_pty_session_handle_resize() {
        let lua = Lua::new();
        let handle = create_test_session_handle(false);

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // Resize and verify dimensions change
        lua.load("session:resize(40, 120)")
            .exec()
            .expect("resize should work");

        let result: (u16, u16) = lua
            .load("return session:dimensions()")
            .eval()
            .expect("dimensions should work after resize");
        assert_eq!(result, (40, 120));
    }

    #[test]
    fn test_pty_session_handle_get_scrollback_empty() {
        let lua = Lua::new();
        let handle = create_test_session_handle(false);

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // Empty scrollback returns empty string
        let result: LuaString = lua
            .load("return session:get_scrollback()")
            .eval()
            .expect("get_scrollback should work");
        assert_eq!(result.as_bytes(), b"");
    }

    #[test]
    fn test_pty_session_handle_get_scrollback_with_data() {
        let lua = Lua::new();
        let handle = create_test_session_handle(false);

        // Add data to scrollback buffer directly
        {
            let mut buffer = handle
                .scrollback_buffer
                .lock()
                .expect("scrollback lock");
            buffer.extend(b"hello world".iter());
        }

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        let result: LuaString = lua
            .load("return session:get_scrollback()")
            .eval()
            .expect("get_scrollback should work");
        assert_eq!(result.as_bytes(), b"hello world");
    }

    #[test]
    fn test_pty_session_handle_port_nil() {
        let lua = Lua::new();
        let handle = create_test_session_handle(false);

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // No port configured -> nil
        let result: LuaValue = lua
            .load("return session:port()")
            .eval()
            .expect("port should work");
        assert!(result.is_nil());
    }

    #[test]
    fn test_pty_session_handle_port_with_value() {
        let lua = Lua::new();
        let mut handle = create_test_session_handle(false);
        handle.port = Some(8080);

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        let result: u16 = lua
            .load("return session:port()")
            .eval()
            .expect("port should return number");
        assert_eq!(result, 8080);
    }

    #[test]
    fn test_pty_session_handle_is_alive_no_writer() {
        let lua = Lua::new();
        let handle = create_test_session_handle(false);

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // No writer set -> not alive
        let result: bool = lua
            .load("return session:is_alive()")
            .eval()
            .expect("is_alive should work");
        assert!(!result, "Session without writer should not be alive");
    }

    #[test]
    fn test_pty_session_handle_write_no_writer_is_noop() {
        let lua = Lua::new();
        let handle = create_test_session_handle(false);

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // Write with no writer should not error (just no-op)
        lua.load(r#"session:write("test")"#)
            .exec()
            .expect("write with no writer should not error");
    }

    #[test]
    fn test_pty_session_handle_poll_notifications_empty() {
        let lua = Lua::new();
        let handle = create_test_session_handle(false);

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // No notifications configured -> empty table
        let result: LuaTable = lua
            .load("return session:poll_notifications()")
            .eval()
            .expect("poll_notifications should work");
        assert_eq!(result.len().unwrap(), 0);
    }

    #[test]
    fn test_pty_session_handle_poll_notifications_with_data() {
        let lua = Lua::new();
        let handle = create_test_session_handle(true);

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // Should have one notification (from create_test_session_handle)
        let count: i64 = lua
            .load(
                r#"
                local notifs = session:poll_notifications()
                return #notifs
            "#,
            )
            .eval()
            .expect("poll_notifications should work");
        assert_eq!(count, 1);

        // Second poll should be empty (drained)
        let count2: i64 = lua
            .load(
                r#"
                local notifs = session:poll_notifications()
                return #notifs
            "#,
            )
            .eval()
            .expect("second poll should work");
        assert_eq!(count2, 0);
    }

    #[test]
    fn test_pty_session_handle_poll_notifications_osc9_fields() {
        let lua = Lua::new();
        let handle = create_test_session_handle(true);

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // Verify notification fields
        let (notif_type, message): (String, String) = lua
            .load(
                r#"
                local notifs = session:poll_notifications()
                return notifs[1].type, notifs[1].message
            "#,
            )
            .eval()
            .expect("notification fields should work");
        assert_eq!(notif_type, "osc9");
        assert_eq!(message, "test");
    }

    #[test]
    fn test_pty_session_handle_poll_notifications_osc777() {
        let lua = Lua::new();

        // Create handle with osc777 notification
        let session = PtySession::new(24, 80);
        let (shared_state, scrollback_buffer, event_tx) = session.get_direct_access();
        let session_arc = Arc::new(Mutex::new(session));

        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(AgentNotification::Osc777 {
            title: "Build".to_string(),
            body: "Complete".to_string(),
        })
        .unwrap();
        drop(tx);

        let handle = PtySessionHandle {
            _session: session_arc,
            shared_state,
            scrollback_buffer,
            event_tx,
            port: None,
            has_notifications: true,
            notification_rx: Some(Arc::new(Mutex::new(rx))),
        };

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        let (notif_type, title, body): (String, String, String) = lua
            .load(
                r#"
                local notifs = session:poll_notifications()
                return notifs[1].type, notifs[1].title, notifs[1].body
            "#,
            )
            .eval()
            .expect("osc777 fields should work");
        assert_eq!(notif_type, "osc777");
        assert_eq!(title, "Build");
        assert_eq!(body, "Complete");
    }

    #[test]
    fn test_pty_session_handle_kill() {
        let lua = Lua::new();
        let handle = create_test_session_handle(false);

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // Kill should not panic even with no child process
        lua.load("session:kill()")
            .exec()
            .expect("kill should not error");
    }

    #[tokio::test]
    async fn test_pty_spawn_function_exists() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let pty: mlua::Table = lua.globals().get("pty").expect("pty table should exist");
        let _: mlua::Function = pty
            .get("spawn")
            .expect("pty.spawn should exist");
    }

    #[tokio::test]
    async fn test_pty_spawn_requires_worktree_path() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let result: mlua::Result<()> = lua
            .load(
                r#"
                pty.spawn({ command = "echo hello" })
            "#,
            )
            .exec();

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("worktree_path"),
            "Error should mention worktree_path: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_pty_spawn_basic() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let temp_path = temp_dir.path().to_string_lossy().to_string();

        lua.globals()
            .set("temp_path", temp_path)
            .expect("Failed to set temp_path");

        // Spawn a session and verify basic methods work
        lua.load(
            r#"
                session = pty.spawn({
                    worktree_path = temp_path,
                    command = "echo hello",
                    rows = 30,
                    cols = 100,
                })
            "#,
        )
        .exec()
        .expect("pty.spawn should work");

        // Verify dimensions
        let (rows, cols): (u16, u16) = lua
            .load("return session:dimensions()")
            .eval()
            .expect("dimensions should work");
        assert_eq!(rows, 30);
        assert_eq!(cols, 100);

        // Verify is_alive (should be true since we spawned a process)
        let alive: bool = lua
            .load("return session:is_alive()")
            .eval()
            .expect("is_alive should work");
        assert!(alive, "Spawned session should be alive");

        // Verify port is nil (not configured)
        let port: LuaValue = lua
            .load("return session:port()")
            .eval()
            .expect("port should work");
        assert!(port.is_nil());
    }

    #[tokio::test]
    async fn test_pty_spawn_with_port() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let temp_path = temp_dir.path().to_string_lossy().to_string();

        lua.globals()
            .set("temp_path", temp_path)
            .expect("Failed to set temp_path");

        lua.load(
            r#"
                session = pty.spawn({
                    worktree_path = temp_path,
                    command = "echo hello",
                    port = 9090,
                })
            "#,
        )
        .exec()
        .expect("pty.spawn with port should work");

        let port: u16 = lua
            .load("return session:port()")
            .eval()
            .expect("port should return number");
        assert_eq!(port, 9090);
    }

    #[tokio::test]
    async fn test_pty_spawn_with_notifications() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let temp_path = temp_dir.path().to_string_lossy().to_string();

        lua.globals()
            .set("temp_path", temp_path)
            .expect("Failed to set temp_path");

        lua.load(
            r#"
                session = pty.spawn({
                    worktree_path = temp_path,
                    command = "echo hello",
                    detect_notifications = true,
                })
            "#,
        )
        .exec()
        .expect("pty.spawn with notifications should work");

        // poll_notifications should work (returns empty table, no OSC in output)
        let count: i64 = lua
            .load(
                r#"
                local notifs = session:poll_notifications()
                return #notifs
            "#,
            )
            .eval()
            .expect("poll_notifications should work");
        // May be 0 or more depending on timing, just verify no crash
        assert!(count >= 0);
    }

    #[tokio::test]
    async fn test_pty_spawn_write_and_scrollback() {
        let lua = Lua::new();
        let queue = new_request_queue();

        super::register(&lua, Arc::clone(&queue)).expect("Should register PTY primitives");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let temp_path = temp_dir.path().to_string_lossy().to_string();

        lua.globals()
            .set("temp_path", temp_path)
            .expect("Failed to set temp_path");

        lua.load(
            r#"
                session = pty.spawn({
                    worktree_path = temp_path,
                    command = "bash",
                })
            "#,
        )
        .exec()
        .expect("pty.spawn should work");

        // Write input should not error
        lua.load(r#"session:write("echo test\n")"#)
            .exec()
            .expect("write should work");

        // Give the PTY a moment to process
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Scrollback should have some data (at least the shell prompt)
        let scrollback: LuaString = lua
            .load("return session:get_scrollback()")
            .eval()
            .expect("get_scrollback should work");
        assert!(
            !scrollback.as_bytes().is_empty(),
            "Scrollback should have data after writing"
        );

        // Kill the session
        lua.load("session:kill()")
            .exec()
            .expect("kill should work");
    }
}
