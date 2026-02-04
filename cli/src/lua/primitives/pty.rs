//! PTY primitives for Lua scripts.
//!
//! Exposes PTY terminal handling to Lua, allowing scripts to create forwarders,
//! send input, resize terminals, and optionally intercept PTY output via hooks.
//!
//! # Design Principle: "Lua controls. Rust streams."
//!
//! For high-frequency PTY output:
//! - **Default (fast path)**: Rust streams directly to WebRTC, no Lua in data path
//! - **Optional (slow path)**: If "pty_output" hooks are registered, call them
//!
//! # Usage in Lua
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

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use mlua::prelude::*;

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

/// PTY operations queued from Lua.
///
/// These are processed by Hub in its event loop after Lua callbacks return.
#[derive(Debug)]
pub enum PtyRequest {
    /// Create a new PTY forwarder for streaming to WebRTC.
    CreateForwarder(CreateForwarderRequest),

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
/// - `webrtc.create_pty_forwarder(opts)` - Create a PTY-to-WebRTC forwarder
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
}
