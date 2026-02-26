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
//! -- Get clean ANSI snapshot for reconnect
//! local snapshot = session:get_snapshot()
//!
//! -- Check forwarding port
//! local port = session:port()  -- number or nil
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
//! ```
//!
//! # Hook Integration
//!
//! Two hook types for PTY output:
//!
//! ```lua
//! -- OBSERVER: Async, safe, cannot block or transform
//! hooks.on("pty_output", "my_logger", function(ctx, data)
//!     -- ctx contains: agent_index, pty_index, peer_id
//!     -- data is the raw output bytes
//!     log.info("Got " .. #data .. " bytes from PTY")
//! end)
//!
//! -- INTERCEPTOR: Sync, blocking, can transform/drop (use sparingly!)
//! hooks.intercept("pty_output", "my_filter", function(ctx, data)
//!     -- Return transformed data, or nil to drop
//!     return data:gsub("secret", "***")
//! end, { timeout_ms = 10 })
//! ```

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex};

use crate::agent::pty::{PtySession, SharedPtyState};
use crate::agent::spawn::PtySpawnConfig;
use crate::hub::events::HubEvent;
use tokio::sync::broadcast;

use anyhow::{anyhow, Result};
use mlua::prelude::*;

use crate::agent::pty::events::PtyEvent;
use super::HubEventSender;

// =============================================================================
// PtySessionHandle - Lua-facing handle to a spawned PtySession
// =============================================================================

/// Lua-facing handle to a spawned PTY session.
///
/// Wraps the thread-safe components of a [`PtySession`], allowing Lua to
/// interact with the PTY (write input, resize, get snapshot, poll
/// notifications, etc.) without holding a direct reference to the session.
///
/// The `_session` field keeps the `PtySession` alive via `Arc` -- dropping
/// the last reference triggers `PtySession::drop()` which kills the child
/// process and aborts the command processor task.
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

    /// Shadow terminal for clean ANSI snapshots on reconnect.
    shadow_screen: Arc<Mutex<vt100::Parser>>,

    /// Event broadcast sender for subscribing to PTY output.
    event_tx: broadcast::Sender<PtyEvent>,

    /// Whether the inner PTY has kitty keyboard protocol active.
    kitty_enabled: Arc<AtomicBool>,

    /// Whether a resize happened without the application redrawing yet.
    resize_pending: Arc<AtomicBool>,

    /// Forwarding port (if configured).
    port: Option<u16>,

    /// Message delivery state (created lazily on first send_message).
    delivery: Arc<std::sync::OnceLock<Arc<crate::agent::message_delivery::MessageDeliveryState>>>,

    /// Hub event sender for delivery task notifications.
    hub_event_tx: HubEventSender,
}

impl std::fmt::Debug for PtySessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySessionHandle")
            .field("port", &self.port)
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
            Arc::clone(&self.shadow_screen),
            Arc::clone(&self.kitty_enabled),
            Arc::clone(&self.resize_pending),
            self.port,
        )
    }

    // =========================================================================
    // Broker integration — FD and PID extraction
    // =========================================================================

    /// Return the raw file descriptor of the master PTY end, if available.
    ///
    /// Returns `None` if the PTY session mutex is poisoned or the master FD
    /// has already been transferred (post-`exec` replacement).
    ///
    /// Used by `hub.register_pty_with_broker()` to pass the FD to the broker
    /// process via `SCM_RIGHTS`.
    #[cfg(unix)]
    #[must_use]
    pub fn get_master_fd(&self) -> Option<std::os::unix::io::RawFd> {
        self._session.lock().ok()?.get_master_fd()
    }

    /// Return the OS process ID of the child process, if available.
    ///
    /// Returns `None` if the PTY session mutex is poisoned or the child PID
    /// is not yet set (spawn not yet completed).
    ///
    /// Used by `hub.register_pty_with_broker()` to pass the PID to the broker
    /// so it can monitor the child process lifetime.
    #[must_use]
    pub fn get_child_pid(&self) -> Option<u32> {
        self._session.lock().ok()?.get_child_pid()
    }

    /// Return the current PTY dimensions `(rows, cols)`.
    ///
    /// Reads from `SharedPtyState` so the value reflects the most recent
    /// `resize()` call. Falls back to `(24, 80)` if the mutex is poisoned.
    ///
    /// Used by `hub.register_pty_with_broker()` to pass accurate terminal
    /// dimensions to the broker at registration time instead of hard-coding
    /// the VT100 defaults.
    #[must_use]
    pub fn get_dims(&self) -> (u16, u16) {
        self.shared_state
            .lock()
            .map(|s| s.dimensions)
            .unwrap_or((24, 80))
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

        // session:cursor_visible() -> boolean
        // Returns true when the PTY's cursor is visible (free-text input expected),
        // false when hidden (generation, selection UI, or no input expected).
        // Reads directly from the vt100 shadow screen state.
        methods.add_method("cursor_visible", |_, this, ()| {
            let parser = this
                .shadow_screen
                .lock()
                .expect("PtySessionHandle shadow_screen lock poisoned");
            Ok(!parser.screen().hide_cursor())
        });

        // session:send_message(text) - Queue a message for probe-based delivery.
        // The message is delivered when the PTY is accepting free-text input.
        // Returns immediately; delivery happens asynchronously.
        methods.add_method("send_message", |_, this, text: LuaString| {
            use crate::agent::message_delivery::{MessageDeliveryState, spawn_delivery_task};

            let text_str = text.to_str()
                .map_err(|e| LuaError::runtime(format!("Invalid UTF-8 in message: {e}")))?
                .to_string();

            log::info!("[PtySessionHandle] send_message called ({} bytes)", text_str.len());

            let delivery = this.delivery.get_or_init(|| {
                log::info!("[PtySessionHandle] Spawning delivery task");
                let state = Arc::new(MessageDeliveryState::new());
                let hub_tx = this.hub_event_tx
                    .lock()
                    .expect("hub_event_tx lock poisoned")
                    .clone();
                let _handle = spawn_delivery_task(
                    Arc::clone(&state),
                    Arc::clone(&this.shared_state),
                    this.event_tx.clone(),
                    hub_tx,
                    Arc::clone(&this.kitty_enabled),
                );
                state
            });

            delivery.enqueue(text_str);
            Ok(())
        });

        // session:get_snapshot() -> string (clean ANSI bytes)
        // Also aliased as get_scrollback for backwards compatibility.
        methods.add_method("get_snapshot", |lua, this, ()| {
            let mut parser = this
                .shadow_screen
                .lock()
                .expect("PtySessionHandle shadow_screen lock poisoned");
            let skip_visible = this.resize_pending.swap(false, Ordering::AcqRel);
            let output = crate::agent::pty::snapshot_with_scrollback(parser.screen_mut(), skip_visible);
            lua.create_string(&output)
        });

        // session:get_screen() -> string (plain text, visible screen only)
        //
        // Returns the current visible terminal contents as plain text with no
        // ANSI escape sequences. Intended for agent/LLM consumption where escape
        // codes add noise. Unlike get_snapshot(), this does not include scrollback
        // and does not affect resize_pending state.
        methods.add_method("get_screen", |lua, this, ()| {
            let parser = this
                .shadow_screen
                .lock()
                .expect("PtySessionHandle shadow_screen lock poisoned");
            let text = parser.screen().contents();
            lua.create_string(text.as_bytes())
        });

        // Backwards-compatible alias
        methods.add_method("get_scrollback", |lua, this, ()| {
            let mut parser = this
                .shadow_screen
                .lock()
                .expect("PtySessionHandle shadow_screen lock poisoned");
            let skip_visible = this.resize_pending.swap(false, Ordering::AcqRel);
            let output = crate::agent::pty::snapshot_with_scrollback(parser.screen_mut(), skip_visible);
            lua.create_string(&output)
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

        // session:kitty_enabled() -> boolean
        //
        // Returns true when the PTY's child process has activated kitty
        // keyboard protocol. Used by notification writers to send the
        // correct Enter key encoding (CSI 13 u vs raw \r).
        methods.add_method("kitty_enabled", |_, this, ()| {
            Ok(this.kitty_enabled.load(std::sync::atomic::Ordering::Relaxed))
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
#[derive(Debug)]
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

/// Request to create a socket PTY forwarder (queued for Hub to process).
///
/// Streams PTY output as `Frame::PtyOutput` over a Unix domain socket connection.
#[derive(Debug, Clone)]
pub struct CreateSocketForwarderRequest {
    /// Socket client identifier (e.g., "socket:0137b").
    pub client_id: String,
    /// Agent index in Hub's agent list.
    pub agent_index: usize,
    /// PTY index within the agent (0=CLI, 1=Server).
    pub pty_index: usize,
    /// Subscription ID for tracking (Lua-generated).
    pub subscription_id: String,
    /// Shared active flag for the forwarder handle.
    pub active_flag: Arc<Mutex<bool>>,
}

/// PTY operations queued from Lua.
///
/// These are processed by Hub in its event loop after Lua callbacks return.
#[derive(Debug)]
pub enum PtyRequest {
    /// Create a new PTY forwarder for streaming to WebRTC.
    CreateForwarder(CreateForwarderRequest),

    /// Create a new PTY forwarder for streaming to TUI (index-based).
    CreateTuiForwarder(CreateTuiForwarderRequest),

    /// Create a new PTY forwarder for streaming to a socket client.
    CreateSocketForwarder(CreateSocketForwarderRequest),

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

    /// Spawn a notification watcher task that subscribes to PTY events
    /// and queues `PtyEvent::Notification` events for the Hub tick loop.
    SpawnNotificationWatcher {
        /// Unique key: "{agent_key}:{session_name}".
        watcher_key: String,
        /// Agent key for the Lua hook context.
        agent_key: String,
        /// Session name (e.g., "cli", "server").
        session_name: String,
        /// Event sender to subscribe to PTY events.
        event_tx: broadcast::Sender<PtyEvent>,
    },
}

// Implement Clone for PtyRequest to satisfy the requirement
impl Clone for PtyRequest {
    fn clone(&self) -> Self {
        match self {
            Self::CreateForwarder(req) => Self::CreateForwarder(req.clone()),
            Self::CreateTuiForwarder(req) => Self::CreateTuiForwarder(req.clone()),
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
            Self::SpawnNotificationWatcher {
                watcher_key,
                agent_key,
                session_name,
                event_tx,
            } => Self::SpawnNotificationWatcher {
                watcher_key: watcher_key.clone(),
                agent_key: agent_key.clone(),
                session_name: session_name.clone(),
                event_tx: event_tx.clone(),
            },
            Self::CreateSocketForwarder(req) => Self::CreateSocketForwarder(req.clone()),
        }
    }
}

/// Helper to send a PTY request through the shared event sender.
///
/// Silently drops the event with a warning if the sender isn't wired up yet.
fn send_pty_event(tx: &HubEventSender, request: PtyRequest) {
    let guard = tx.lock().expect("HubEventSender mutex poisoned");
    if let Some(ref sender) = *guard {
        let _ = sender.send(HubEvent::LuaPtyRequest(request));
    } else {
        ::log::warn!("[PTY] Request sent before hub_event_tx set — event dropped");
    }
}

/// Register PTY primitives with the Lua state.
///
/// Adds the following functions:
/// - `pty.spawn(config)` - Spawn a PTY session, returns `PtySessionHandle` userdata
/// - `webrtc.create_pty_forwarder(opts)` - Create a PTY-to-WebRTC forwarder
/// - `tui.create_pty_forwarder(opts)` - Create a PTY-to-TUI forwarder
/// - `hub.write_pty(agent_index, pty_index, data)` - Write input to PTY
/// - `hub.resize_pty(agent_index, pty_index, rows, cols)` - Resize PTY
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `hub_event_tx` - Shared sender for Hub events (filled in later by `set_hub_event_tx`)
///
/// # Errors
///
/// Returns an error if Lua table or function creation fails.
pub(crate) fn register(lua: &Lua, hub_event_tx: HubEventSender) -> Result<()> {
    // Get or create the webrtc table
    let webrtc: LuaTable = lua
        .globals()
        .get("webrtc")
        .unwrap_or_else(|_| lua.create_table().unwrap());

    // webrtc.create_pty_forwarder({ peer_id, agent_index, pty_index, subscription_id, prefix? })
    let tx = hub_event_tx.clone();
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

            // Send the request to Hub via event channel
            send_pty_event(&tx, PtyRequest::CreateForwarder(CreateForwarderRequest {
                peer_id: peer_id.clone(),
                agent_index,
                pty_index,
                prefix: prefix.map(|p| p.as_bytes().to_vec()),
                subscription_id,
                active_flag: Arc::clone(&active_flag),
            }));

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
    let tx_tui = hub_event_tx.clone();
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

            // Send the request to Hub via event channel
            send_pty_event(&tx_tui, PtyRequest::CreateTuiForwarder(CreateTuiForwarderRequest {
                agent_index,
                pty_index,
                subscription_id,
                active_flag: Arc::clone(&active_flag),
            }));

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
    let tx2 = hub_event_tx.clone();
    let write_pty_fn = lua
        .create_function(
            move |_, (agent_index, pty_index, data): (usize, usize, LuaString)| {
                send_pty_event(&tx2, PtyRequest::WritePty {
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
    let tx3 = hub_event_tx.clone();
    let resize_pty_fn = lua
        .create_function(
            move |_, (agent_index, pty_index, rows, cols): (usize, usize, u16, u16)| {
                send_pty_event(&tx3, PtyRequest::ResizePty {
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
    //   agent_key: string (optional) - Agent key for notification watcher
    //   session_name: string (optional) - Session name for notification watcher
    //   cols: number (default 80) - Initial cols
    let tx_spawn = hub_event_tx.clone();
    let spawn_fn = lua
        .create_function(move |_lua, opts: LuaTable| {
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
            let agent_key: Option<String> = opts.get("agent_key").ok();
            let session_name: Option<String> = opts.get("session_name").ok();

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
            let (shared_state, shadow_screen, event_tx, kitty_enabled, resize_pending) = session.get_direct_access();
            let session_port = session.port();

            // Always spawn the event watcher when agent identity is provided.
            // The watcher forwards PtyEvent variants (notifications, title, CWD,
            // prompt marks) to Lua hooks. Notification detection in the reader
            // thread is still gated by `detect_notifications`, but OSC metadata
            // events (title/CWD/prompt) are emitted unconditionally.
            if let (Some(ak), Some(sn)) = (&agent_key, &session_name) {
                let watcher_key = format!("{ak}:{sn}");
                send_pty_event(&tx_spawn, PtyRequest::SpawnNotificationWatcher {
                    watcher_key,
                    agent_key: ak.clone(),
                    session_name: sn.clone(),
                    event_tx: event_tx.clone(),
                });
            }

            // Wrap session in Arc<Mutex<>> to keep it alive (Drop kills child)
            let session_arc = Arc::new(Mutex::new(session));

            let handle = PtySessionHandle {
                _session: session_arc,
                shared_state,
                shadow_screen,
                event_tx,
                kitty_enabled,
                resize_pending,
                port: session_port,
                delivery: Arc::new(std::sync::OnceLock::new()),
                hub_event_tx: tx_spawn.clone(),
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
    use super::super::new_hub_event_sender;

    /// Create a wired-up sender with a channel for tests that need to check events.
    fn setup_with_channel() -> (HubEventSender, tokio::sync::mpsc::UnboundedReceiver<HubEvent>) {
        let tx = new_hub_event_sender();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        *tx.lock().unwrap() = Some(sender);
        (tx, receiver)
    }

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
    fn test_create_pty_forwarder_sends_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();

        super::register(&lua, tx).expect("Should register PTY primitives");

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

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::LuaPtyRequest(PtyRequest::CreateForwarder(req)) => {
                assert_eq!(req.peer_id, "browser-123");
                assert_eq!(req.agent_index, 0);
                assert_eq!(req.pty_index, 1);
                assert_eq!(req.subscription_id, "sub_1_1234567890");
                assert!(req.prefix.is_none());
            }
            _ => panic!("Expected LuaPtyRequest CreateForwarder event"),
        }

        let id: String = lua.load("return forwarder:id()").eval().unwrap();
        assert_eq!(id, "browser-123:0:1");
    }

    #[test]
    fn test_create_pty_forwarder_with_prefix() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();

        super::register(&lua, tx).expect("Should register PTY primitives");

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

        match rx.try_recv().unwrap() {
            HubEvent::LuaPtyRequest(PtyRequest::CreateForwarder(req)) => {
                assert_eq!(req.prefix, Some(vec![0x01]));
                assert_eq!(req.subscription_id, "sub_2_9876543210");
            }
            _ => panic!("Expected CreateForwarder event"),
        }
    }

    #[test]
    fn test_write_pty_sends_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();

        super::register(&lua, tx).expect("Should register PTY primitives");

        lua.load(r#"hub.write_pty(0, 1, "ls -la\n")"#)
            .exec()
            .expect("Should write PTY");

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::LuaPtyRequest(PtyRequest::WritePty {
                agent_index,
                pty_index,
                data,
            }) => {
                assert_eq!(agent_index, 0);
                assert_eq!(pty_index, 1);
                assert_eq!(data, b"ls -la\n");
            }
            _ => panic!("Expected LuaPtyRequest WritePty event"),
        }
    }

    #[test]
    fn test_resize_pty_sends_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();

        super::register(&lua, tx).expect("Should register PTY primitives");

        lua.load(r#"hub.resize_pty(0, 0, 40, 120)"#)
            .exec()
            .expect("Should resize PTY");

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::LuaPtyRequest(PtyRequest::ResizePty {
                agent_index,
                pty_index,
                rows,
                cols,
            }) => {
                assert_eq!(agent_index, 0);
                assert_eq!(pty_index, 0);
                assert_eq!(rows, 40);
                assert_eq!(cols, 120);
            }
            _ => panic!("Expected LuaPtyRequest ResizePty event"),
        }
    }

    #[test]
    fn test_multiple_requests_send_in_order() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();

        super::register(&lua, tx).expect("Should register PTY primitives");

        lua.load(
            r#"
            webrtc.create_pty_forwarder({ peer_id = "p1", agent_index = 0, pty_index = 0, subscription_id = "sub_1" })
            hub.write_pty(0, 0, "test")
            hub.resize_pty(0, 0, 24, 80)
        "#,
        )
        .exec()
        .expect("Should send multiple events");

        let e1 = rx.try_recv().expect("Should have first event");
        let e2 = rx.try_recv().expect("Should have second event");
        let e3 = rx.try_recv().expect("Should have third event");

        assert!(matches!(e1, HubEvent::LuaPtyRequest(PtyRequest::CreateForwarder(_))));
        assert!(matches!(e2, HubEvent::LuaPtyRequest(PtyRequest::WritePty { .. })));
        assert!(matches!(e3, HubEvent::LuaPtyRequest(PtyRequest::ResizePty { .. })));
    }

    #[test]
    fn test_create_forwarder_requires_peer_id() {
        let lua = Lua::new();
        let tx = new_hub_event_sender();

        super::register(&lua, tx.clone()).expect("Should register PTY primitives");

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
        let tx = new_hub_event_sender();

        super::register(&lua, tx.clone()).expect("Should register PTY primitives");

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
        let tx = new_hub_event_sender();

        super::register(&lua, tx.clone()).expect("Should register PTY primitives");

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
        let tx = new_hub_event_sender();

        // Register TUI table stub first (pty.rs appends to it)
        lua.globals()
            .set("tui", lua.create_table().unwrap())
            .unwrap();

        super::register(&lua, tx.clone()).expect("Should register PTY primitives");

        let tui: mlua::Table = lua.globals().get("tui").expect("tui should exist");
        let _: mlua::Function = tui
            .get("create_pty_forwarder")
            .expect("tui.create_pty_forwarder should exist");
    }

    #[test]
    fn test_tui_create_pty_forwarder_sends_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();

        lua.globals()
            .set("tui", lua.create_table().unwrap())
            .unwrap();

        super::register(&lua, tx).expect("Should register PTY primitives");

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

        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::LuaPtyRequest(PtyRequest::CreateTuiForwarder(req)) => {
                assert_eq!(req.agent_index, 0);
                assert_eq!(req.pty_index, 1);
                assert_eq!(req.subscription_id, "tui_term_1");
            }
            _ => panic!("Expected LuaPtyRequest CreateTuiForwarder event"),
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
        let tx = new_hub_event_sender();

        lua.globals()
            .set("tui", lua.create_table().unwrap())
            .unwrap();

        super::register(&lua, tx.clone()).expect("Should register PTY primitives");

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
        let tx = new_hub_event_sender();

        lua.globals()
            .set("tui", lua.create_table().unwrap())
            .unwrap();

        super::register(&lua, tx.clone()).expect("Should register PTY primitives");

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
    fn create_test_session_handle() -> PtySessionHandle {
        let session = PtySession::new(24, 80);
        let (shared_state, shadow_screen, event_tx, kitty_enabled, resize_pending) = session.get_direct_access();
        let session_arc = Arc::new(Mutex::new(session));

        PtySessionHandle {
            _session: session_arc,
            shared_state,
            shadow_screen,
            event_tx,
            kitty_enabled,
            resize_pending,
            port: None,
            delivery: Arc::new(std::sync::OnceLock::new()),
            hub_event_tx: crate::lua::primitives::new_hub_event_sender(),
        }
    }

    #[test]
    fn test_pty_session_handle_dimensions() {
        let lua = Lua::new();
        let handle = create_test_session_handle();

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
        let handle = create_test_session_handle();

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
    fn test_pty_session_handle_cursor_visible_default() {
        let lua = Lua::new();
        let handle = create_test_session_handle();

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // Default state: cursor should be visible
        let result: bool = lua
            .load("return session:cursor_visible()")
            .eval()
            .expect("cursor_visible should work");
        assert!(result, "Cursor should be visible by default");
    }

    #[test]
    fn test_pty_session_handle_cursor_hidden() {
        let lua = Lua::new();
        let handle = create_test_session_handle();

        // Send DECTCEM hide cursor sequence to shadow screen
        handle
            .shadow_screen
            .lock()
            .expect("shadow_screen lock")
            .process(b"\x1b[?25l");

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        let result: bool = lua
            .load("return session:cursor_visible()")
            .eval()
            .expect("cursor_visible should work");
        assert!(!result, "Cursor should be hidden after DECTCEM hide");
    }

    #[test]
    fn test_pty_session_handle_cursor_show_after_hide() {
        let lua = Lua::new();
        let handle = create_test_session_handle();

        // Hide then show cursor
        handle
            .shadow_screen
            .lock()
            .expect("shadow_screen lock")
            .process(b"\x1b[?25l\x1b[?25h");

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        let result: bool = lua
            .load("return session:cursor_visible()")
            .eval()
            .expect("cursor_visible should work");
        assert!(result, "Cursor should be visible after show sequence");
    }

    #[test]
    fn test_pty_session_handle_get_snapshot_empty() {
        let lua = Lua::new();
        let handle = create_test_session_handle();

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // Empty shadow screen returns reset + cursor position
        let result: LuaString = lua
            .load("return session:get_snapshot()")
            .eval()
            .expect("get_snapshot should work");
        // Should at least contain the reset sequence
        let bytes = result.as_bytes();
        assert!(bytes.starts_with(b"\x1b[H\x1b[2J\x1b[0m"));
    }

    #[test]
    fn test_pty_session_handle_get_snapshot_with_data() {
        let lua = Lua::new();
        let handle = create_test_session_handle();

        // Feed data to shadow screen directly
        handle
            .shadow_screen
            .lock()
            .expect("shadow_screen lock")
            .process(b"hello world");

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        let result: LuaString = lua
            .load("return session:get_snapshot()")
            .eval()
            .expect("get_snapshot should work");
        let bytes = result.as_bytes();
        let result_str = String::from_utf8_lossy(&bytes);
        assert!(result_str.contains("hello world"));
    }

    #[test]
    fn test_pty_session_handle_get_scrollback_alias() {
        let lua = Lua::new();
        let handle = create_test_session_handle();

        handle
            .shadow_screen
            .lock()
            .expect("shadow_screen lock")
            .process(b"alias test");

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // get_scrollback should work as an alias for get_snapshot
        let result: LuaString = lua
            .load("return session:get_scrollback()")
            .eval()
            .expect("get_scrollback alias should work");
        let bytes = result.as_bytes();
        let result_str = String::from_utf8_lossy(&bytes);
        assert!(result_str.contains("alias test"));
    }

    #[test]
    fn test_pty_session_handle_port_nil() {
        let lua = Lua::new();
        let handle = create_test_session_handle();

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
        let mut handle = create_test_session_handle();
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
        let handle = create_test_session_handle();

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
        let handle = create_test_session_handle();

        lua.globals()
            .set("session", handle)
            .expect("Failed to set session");

        // Write with no writer should not error (just no-op)
        lua.load(r#"session:write("test")"#)
            .exec()
            .expect("write with no writer should not error");
    }

    #[test]
    fn test_pty_session_handle_kill() {
        let lua = Lua::new();
        let handle = create_test_session_handle();

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
        let tx = new_hub_event_sender();

        super::register(&lua, tx.clone()).expect("Should register PTY primitives");

        let pty: mlua::Table = lua.globals().get("pty").expect("pty table should exist");
        let _: mlua::Function = pty
            .get("spawn")
            .expect("pty.spawn should exist");
    }

    #[tokio::test]
    async fn test_pty_spawn_requires_worktree_path() {
        let lua = Lua::new();
        let tx = new_hub_event_sender();

        super::register(&lua, tx.clone()).expect("Should register PTY primitives");

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
        let tx = new_hub_event_sender();

        super::register(&lua, tx.clone()).expect("Should register PTY primitives");

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
        let tx = new_hub_event_sender();

        super::register(&lua, tx.clone()).expect("Should register PTY primitives");

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
    async fn test_pty_spawn_with_notifications_sends_watcher_event() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();

        super::register(&lua, tx).expect("Should register PTY primitives");

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
                    agent_key = "agent-1",
                    session_name = "cli",
                })
            "#,
        )
        .exec()
        .expect("pty.spawn with notifications should work");

        // Should have sent a SpawnNotificationWatcher event
        let event = rx.try_recv().expect("Should have received event");
        match event {
            HubEvent::LuaPtyRequest(PtyRequest::SpawnNotificationWatcher {
                watcher_key,
                agent_key,
                session_name,
                ..
            }) => {
                assert_eq!(watcher_key, "agent-1:cli");
                assert_eq!(agent_key, "agent-1");
                assert_eq!(session_name, "cli");
            }
            other => panic!("Expected LuaPtyRequest SpawnNotificationWatcher, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_pty_spawn_notifications_without_identity_no_watcher() {
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();

        super::register(&lua, tx).expect("Should register PTY primitives");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let temp_path = temp_dir.path().to_string_lossy().to_string();

        lua.globals()
            .set("temp_path", temp_path)
            .expect("Failed to set temp_path");

        // detect_notifications=true but no agent_key/session_name -> no event sent
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
        .expect("pty.spawn should work");

        assert!(rx.try_recv().is_err(), "No event should be sent without identity");
    }

    #[tokio::test]
    async fn test_pty_spawn_watcher_spawned_without_detect_notifications() {
        // Watcher should spawn even with detect_notifications=false when identity is provided,
        // because OSC metadata events (title, CWD, prompt) need forwarding for all sessions.
        let lua = Lua::new();
        let (tx, mut rx) = setup_with_channel();

        super::register(&lua, tx).expect("Should register PTY primitives");

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
                    detect_notifications = false,
                    agent_key = "agent-1",
                    session_name = "server",
                })
            "#,
        )
        .exec()
        .expect("pty.spawn should work");

        // Should have sent a SpawnNotificationWatcher event even without detect_notifications
        let event = rx.try_recv().expect("Should have received watcher event");
        match event {
            HubEvent::LuaPtyRequest(PtyRequest::SpawnNotificationWatcher {
                watcher_key,
                agent_key,
                session_name,
                ..
            }) => {
                assert_eq!(watcher_key, "agent-1:server");
                assert_eq!(agent_key, "agent-1");
                assert_eq!(session_name, "server");
            }
            other => panic!("Expected SpawnNotificationWatcher, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_pty_spawn_write_and_snapshot() {
        let lua = Lua::new();
        let tx = new_hub_event_sender();

        super::register(&lua, tx.clone()).expect("Should register PTY primitives");

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

        // Snapshot should have some data (at least the shell prompt + reset sequence)
        let snapshot: LuaString = lua
            .load("return session:get_snapshot()")
            .eval()
            .expect("get_snapshot should work");
        assert!(
            snapshot.as_bytes().len() > 10,
            "Snapshot should have data after writing (got {} bytes)",
            snapshot.as_bytes().len()
        );

        // Kill the session
        lua.load("session:kill()")
            .exec()
            .expect("kill should work");
    }
}
