//! Lua primitive functions exposed to scripts.
//!
//! This module provides the built-in functions available to Lua scripts.
//! Primitives are registered when the Lua runtime is created.
//!
//! # Available Primitives
//!
//! - `log` - Logging functions (info, warn, error, debug)
//! - `webrtc` - WebRTC peer connection and messaging
//! - `pty` - PTY terminal operations (forwarders, input, resize)
//! - `fs` - File system operations (read, write, copy, exists)
//! - `hub` - Hub state queries and operations (agents, worktrees)
//! - `tui` - TUI terminal connection and messaging
//! - `worktree` - Git worktree queries and operations (list, find, create, delete)
//! - `events` - Event subscription system for agent lifecycle events
//!
//! # Adding New Primitives
//!
//! 1. Create a new module (e.g., `foo.rs`)
//! 2. Implement a `register(lua: &Lua) -> Result<()>` function
//! 3. Add `pub mod foo;` here
//! 4. Call `foo::register(lua)?;` in `register_all`

pub mod connection;
pub mod events;
pub mod fs;
pub mod hub;
pub mod log;
pub mod pty;
pub mod tui;
pub mod webrtc;
pub mod worktree;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use mlua::Lua;

use crate::hub::handle_cache::HandleCache;

pub use events::{
    new_event_callbacks, EventCallbackId, EventCallbacks, SharedEventCallbacks,
};
pub use connection::{
    new_request_queue as new_connection_queue, ConnectionRequest, ConnectionRequestQueue,
};
pub use hub::{new_request_queue as new_hub_queue, HubRequest, HubRequestQueue};
pub use pty::{
    new_request_queue as new_pty_queue, CreateForwarderRequest, CreateTuiForwarderRequest,
    CreateTuiForwarderDirectRequest, PtyForwarder, PtyOutputContext, PtyRequest, PtyRequestQueue,
    PtySessionHandle,
};
pub use tui::{new_send_queue as new_tui_queue, TuiSendQueue, TuiSendRequest};
pub use webrtc::{new_send_queue, WebRtcSendQueue, WebRtcSendRequest};
pub use worktree::{
    new_request_queue as new_worktree_queue, WorktreeRequest, WorktreeRequestQueue,
};

/// Register all primitive functions with the Lua state.
///
/// Called during `LuaRuntime::new()` to set up the runtime environment.
/// Note: WebRTC and PTY primitives are registered separately via
/// `register_webrtc()` and `register_pty()` because they require queue references.
///
/// # Errors
///
/// Returns an error if any primitive registration fails.
pub fn register_all(lua: &Lua) -> Result<()> {
    fs::register(lua)?;
    log::register(lua)?;
    Ok(())
}

/// Register WebRTC primitives with a send queue.
///
/// Call this after `register_all()` to set up WebRTC message handling.
/// The send queue is drained by Hub after Lua callbacks return.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `send_queue` - Shared queue for outgoing WebRTC messages
///
/// # Errors
///
/// Returns an error if registration fails.
pub fn register_webrtc(lua: &Lua, send_queue: WebRtcSendQueue) -> Result<()> {
    webrtc::register(lua, send_queue)?;
    Ok(())
}

/// Register TUI primitives with a send queue.
///
/// Call this after `register_all()` to set up TUI message handling.
/// The send queue is drained by Hub after Lua callbacks return.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `send_queue` - Shared queue for outgoing TUI messages
///
/// # Errors
///
/// Returns an error if registration fails.
pub fn register_tui(lua: &Lua, send_queue: TuiSendQueue) -> Result<()> {
    tui::register(lua, send_queue)?;
    Ok(())
}

/// Register PTY primitives with a request queue.
///
/// Call this after `register_all()` to set up PTY operations.
/// The request queue is drained by Hub after Lua callbacks return.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `request_queue` - Shared queue for PTY operations
///
/// # Errors
///
/// Returns an error if registration fails.
pub fn register_pty(lua: &Lua, request_queue: PtyRequestQueue) -> Result<()> {
    pty::register(lua, request_queue)?;
    Ok(())
}

/// Register Hub state primitives with a request queue and handle cache.
///
/// Call this after `register_all()` to set up Hub state queries and operations.
/// The request queue is drained by Hub after Lua callbacks return.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `request_queue` - Shared queue for Hub operations
/// * `handle_cache` - Thread-safe cache of agent handles for queries
///
/// # Errors
///
/// Returns an error if registration fails.
pub fn register_hub(
    lua: &Lua,
    request_queue: HubRequestQueue,
    handle_cache: Arc<HandleCache>,
) -> Result<()> {
    hub::register(lua, request_queue, handle_cache)?;
    Ok(())
}

/// Register connection primitives with a request queue and handle cache.
///
/// Call this after `register_all()` to set up connection URL queries and
/// code regeneration. The request queue is drained by Hub after Lua callbacks return.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `request_queue` - Shared queue for connection operations
/// * `handle_cache` - Thread-safe cache for connection URL queries
///
/// # Errors
///
/// Returns an error if registration fails.
pub fn register_connection(
    lua: &Lua,
    request_queue: ConnectionRequestQueue,
    handle_cache: Arc<HandleCache>,
) -> Result<()> {
    connection::register(lua, request_queue, handle_cache)?;
    Ok(())
}

/// Register worktree primitives with a request queue and handle cache.
///
/// Call this after `register_all()` to set up worktree queries and operations.
/// The request queue is drained by Hub after Lua callbacks return.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `request_queue` - Shared queue for worktree operations
/// * `handle_cache` - Thread-safe cache for worktree queries
/// * `worktree_base` - Base directory for worktree storage
///
/// # Errors
///
/// Returns an error if registration fails.
pub fn register_worktree(
    lua: &Lua,
    request_queue: WorktreeRequestQueue,
    handle_cache: Arc<HandleCache>,
    worktree_base: PathBuf,
) -> Result<()> {
    worktree::register(lua, request_queue, handle_cache, worktree_base)?;
    Ok(())
}

/// Register event system primitives.
///
/// Call this after `register_all()` to set up the event subscription system.
/// The callback storage is used by LuaRuntime to fire events.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `callbacks` - Shared callback storage
///
/// # Errors
///
/// Returns an error if registration fails.
pub fn register_events(lua: &Lua, callbacks: SharedEventCallbacks) -> Result<()> {
    events::register(lua, callbacks)?;
    Ok(())
}
