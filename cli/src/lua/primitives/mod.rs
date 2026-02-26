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
//! - `fs` - File system operations (read, write, copy, exists, listdir, is_dir)
//! - `hub` - Hub state queries and operations (agents, worktrees)
//! - `hub_discovery` - Discover running hubs on this machine (list, is_running, socket_path)
//! - `tui` - TUI terminal connection and messaging
//! - `worktree` - Git worktree queries and operations (list, find, create, delete)
//! - `events` - Event subscription system for agent lifecycle events
//! - `json` - JSON encode/decode (explicit serialization)
//! - `http` - HTTP client (GET, POST, PUT, DELETE)
//! - `timer` - One-shot and repeating timers
//! - `config` - Hub configuration and environment access
//! - `secrets` - Plugin-scoped encrypted secret storage (AES-GCM files, no keyring access)
//! - `action_cable` - ActionCable WebSocket connections (subscribe, perform, callbacks)
//! - `hub_client` - Outgoing hub-to-hub Unix socket connections (connect, send, callbacks)
//! - `websocket` - WebSocket client (persistent connections with callbacks)
//!
//! # Adding New Primitives
//!
//! 1. Create a new module (e.g., `foo.rs`)
//! 2. Implement a `register(lua: &Lua) -> Result<()>` function
//! 3. Add `pub mod foo;` here
//! 4. Call `foo::register(lua)?;` in `register_all`

pub mod action_cable;
pub mod config;
pub mod connection;
pub mod events;
pub mod fs;
pub mod http;
pub mod hub;
pub mod hub_client;
pub mod hub_discovery;
pub mod json;
pub mod log;
pub mod push;
pub mod pty;
pub mod secrets;
pub mod socket;
pub mod timer;
pub mod tui;
pub mod update;
pub mod watch;
pub mod webrtc;
pub mod websocket;
pub mod worktree;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use mlua::Lua;

use crate::hub::handle_cache::HandleCache;

/// Shared sender for Lua primitives to deliver events to the Hub event loop.
///
/// Lua closures capture a clone of this Arc. Initially `None` (primitives are
/// registered before the event channel exists). Filled in by
/// `LuaRuntime::set_hub_event_tx()` before any Lua plugins execute.
///
/// If a Lua closure fires before the sender is set (shouldn't happen in
/// practice), the event is dropped with a warning log.
pub(crate) type HubEventSender = Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<crate::hub::events::HubEvent>>>>;

/// Create a new `HubEventSender` (initially `None`).
#[must_use]
pub(crate) fn new_hub_event_sender() -> HubEventSender {
    Arc::new(Mutex::new(None))
}

pub use events::{
    new_event_callbacks, EventCallbackId, EventCallbacks, SharedEventCallbacks,
};
pub use connection::ConnectionRequest;
pub use hub::{HubRequest, SharedServerId};
pub use pty::{
    CreateForwarderRequest, CreateSocketForwarderRequest, CreateTuiForwarderRequest,
    PtyForwarder, PtyOutputContext, PtyRequest, PtySessionHandle,
};
pub use socket::SocketSendRequest;
pub use tui::TuiSendRequest;
pub use webrtc::WebRtcSendRequest;
pub use http::{new_http_registry, HttpAsyncRegistry};
pub use timer::{new_timer_registry, TimerRegistry};
pub use watch::{new_watcher_registry, WatcherRegistry};
pub use action_cable::{
    ActionCableCallbackRegistry, ActionCableRequest, LuaAcChannel, LuaAcConnection,
    new_callback_registry as new_ac_callback_registry,
};
pub use websocket::{new_websocket_registry, WebSocketRegistry};
pub use hub_client::{
    HubClientCallbackRegistry, HubClientFrameSenders, HubClientPendingRequests,
    HubClientRequest, LuaHubClientConn,
    new_hub_client_callback_registry, new_hub_client_frame_senders, new_hub_client_pending_requests,
};
pub use worktree::{
    WorktreeCreateResult, WorktreeRequest, WorktreeResultReceiver,
    WorktreeResultSender,
};

/// Register all primitive functions with the Lua state.
///
/// Called during `LuaRuntime::new()` to set up the runtime environment.
/// Note: WebRTC, TUI, PTY, Hub, connection, and worktree primitives are
/// registered separately because they require a `HubEventSender` reference.
///
/// # Errors
///
/// Returns an error if any primitive registration fails.
pub fn register_all(lua: &Lua) -> Result<()> {
    fs::register(lua)?;
    log::register(lua)?;
    json::register(lua)?;
    config::register(lua)?;
    hub_discovery::register(lua)?;
    secrets::register(lua)?;
    Ok(())
}

/// Register self-update primitives with a shared event sender.
///
/// Call this after `register_all()` to set up update checking and installation.
/// The install function sends `HubEvent::LuaHubRequest(ExecRestart)` on success.
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_update(lua: &Lua, hub_event_tx: HubEventSender) -> Result<()> {
    update::register(lua, hub_event_tx)?;
    Ok(())
}

/// Register web push notification primitives with a shared event sender.
///
/// Call this after `register_all()` to set up push notification sending.
/// Events are sent directly to the Hub event loop via `HubEventSender`.
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_push(lua: &Lua, hub_event_tx: HubEventSender) -> Result<()> {
    push::register(lua, hub_event_tx)?;
    Ok(())
}

/// Register WebRTC primitives with a shared event sender.
///
/// Call this after `register_all()` to set up WebRTC message handling.
/// Events are sent directly to the Hub event loop via `HubEventSender`.
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_webrtc(lua: &Lua, hub_event_tx: HubEventSender) -> Result<()> {
    webrtc::register(lua, hub_event_tx)?;
    Ok(())
}

/// Register socket IPC primitives with a shared event sender.
///
/// Call this after `register_all()` to set up socket client message handling.
/// Events are sent directly to the Hub event loop via `HubEventSender`.
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_socket(lua: &Lua, hub_event_tx: HubEventSender) -> Result<()> {
    socket::register(lua, hub_event_tx)?;
    Ok(())
}

/// Register TUI primitives with a shared event sender.
///
/// Call this after `register_all()` to set up TUI message handling.
/// Events are sent directly to the Hub event loop via `HubEventSender`.
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_tui(lua: &Lua, hub_event_tx: HubEventSender) -> Result<()> {
    tui::register(lua, hub_event_tx)?;
    Ok(())
}

/// Register PTY primitives with a shared event sender.
///
/// Call this after `register_all()` to set up PTY operations.
/// Events are sent directly to the Hub event loop via `HubEventSender`.
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_pty(lua: &Lua, hub_event_tx: HubEventSender) -> Result<()> {
    pty::register(lua, hub_event_tx)?;
    Ok(())
}

/// Register Hub state primitives with a shared event sender, handle cache, and shared state.
///
/// Call this after `register_all()` to set up Hub state queries and operations.
/// Events are sent directly to the Hub event loop via `HubEventSender`.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `hub_event_tx` - Shared sender for Hub events
/// * `handle_cache` - Thread-safe cache of agent handles for queries
/// * `hub_identifier` - Local hub identifier (stable hash, matches hub_discovery IDs)
/// * `server_id` - Server-assigned hub ID (set after registration)
/// * `shared_state` - Shared hub state for agent queries
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_hub(
    lua: &Lua,
    hub_event_tx: HubEventSender,
    handle_cache: Arc<HandleCache>,
    hub_identifier: String,
    server_id: SharedServerId,
    shared_state: Arc<std::sync::RwLock<crate::hub::state::HubState>>,
) -> Result<()> {
    hub::register(lua, hub_event_tx, handle_cache, hub_identifier, server_id, shared_state)?;
    Ok(())
}

/// Register connection primitives with a shared event sender and handle cache.
///
/// Call this after `register_all()` to set up connection URL queries and
/// code regeneration. Events are sent directly to the Hub event loop.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `hub_event_tx` - Shared sender for Hub events
/// * `handle_cache` - Thread-safe cache for connection URL queries
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_connection(
    lua: &Lua,
    hub_event_tx: HubEventSender,
    handle_cache: Arc<HandleCache>,
) -> Result<()> {
    connection::register(lua, hub_event_tx, handle_cache)?;
    Ok(())
}

/// Register worktree primitives with a shared event sender and handle cache.
///
/// Call this after `register_all()` to set up worktree queries and operations.
/// Events are sent directly to the Hub event loop.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `hub_event_tx` - Shared sender for Hub events
/// * `handle_cache` - Thread-safe cache for worktree queries
/// * `worktree_base` - Base directory for worktree storage
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_worktree(
    lua: &Lua,
    hub_event_tx: HubEventSender,
    handle_cache: Arc<HandleCache>,
    worktree_base: PathBuf,
) -> Result<()> {
    worktree::register(lua, hub_event_tx, handle_cache, worktree_base)?;
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

/// Register HTTP primitives with an async response registry.
///
/// Call this after `register_all()` to set up HTTP request functions.
/// Sync functions (`http.get/post/put/delete`) block the caller.
/// The async function (`http.request`) spawns a background thread that
/// sends `HubEvent::HttpResponse` to the Hub event loop.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `registry` - Shared async HTTP registry
///
/// # Errors
///
/// Returns an error if registration fails.
pub fn register_http(lua: &Lua, registry: HttpAsyncRegistry) -> Result<()> {
    http::register(lua, registry)?;
    Ok(())
}

/// Register timer primitives with a timer registry.
///
/// Call this after `register_all()` to set up one-shot and repeating timers.
/// Each timer spawns a tokio task that sends `HubEvent::TimerFired`.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `registry` - Shared timer registry
///
/// # Errors
///
/// Returns an error if registration fails.
pub fn register_timer(lua: &Lua, registry: TimerRegistry) -> Result<()> {
    timer::register(lua, registry)?;
    Ok(())
}

/// Register file watch primitives with a watcher registry.
///
/// Call this after `register_all()` to set up user-facing file watching.
/// Each watch spawns a blocking forwarder that sends `HubEvent::UserFileWatch`.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `registry` - Shared watcher registry
///
/// # Errors
///
/// Returns an error if registration fails.
pub fn register_watch(lua: &Lua, registry: WatcherRegistry) -> Result<()> {
    watch::register(lua, registry)?;
    Ok(())
}

/// Register WebSocket primitives with a connection registry.
///
/// Call this after `register_all()` to set up persistent WebSocket connections.
/// Each `websocket.connect()` spawns a background thread that sends
/// `HubEvent::WebSocketEvent` to the Hub event loop.
///
/// # Errors
///
/// Returns an error if registration fails.
pub fn register_websocket(lua: &Lua, registry: WebSocketRegistry) -> Result<()> {
    websocket::register(lua, registry)?;
    Ok(())
}

/// Register ActionCable primitives with the shared event sender.
///
/// Call this after `register_all()` to set up ActionCable connection management.
/// Lua closures send `HubEvent::LuaActionCableRequest` via the shared sender.
/// In production, a forwarding task per channel sends `HubEvent::AcChannelMessage`
/// for incoming messages.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `hub_event_tx` - Shared event sender for ActionCable operations
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_action_cable(
    lua: &Lua,
    hub_event_tx: HubEventSender,
    callback_registry: ActionCableCallbackRegistry,
) -> Result<()> {
    action_cable::register_action_cable(lua, hub_event_tx, callback_registry)?;
    Ok(())
}

/// Register hub client primitives with the shared event sender and registries.
///
/// Call this after `register_all()` to set up outgoing hub-to-hub socket
/// connections. `hub_client.send()` routes through the Hub event loop.
/// `hub_client.request()` bypasses the event loop by writing directly to
/// `frame_senders` and reading from `pending_requests` — both populated by Hub.
///
/// # Arguments
///
/// * `lua` - The Lua state to register primitives in
/// * `hub_event_tx` - Shared event sender for connect/close operations
/// * `callback_registry` - Shared callback registry for `on_message()` callbacks
/// * `pending_requests` - Shared map for blocking `request()` response delivery
/// * `frame_senders` - Shared map of conn_id → frame write channel for `request()`
///
/// # Errors
///
/// Returns an error if registration fails.
pub(crate) fn register_hub_client(
    lua: &Lua,
    hub_event_tx: HubEventSender,
    callback_registry: HubClientCallbackRegistry,
    pending_requests: HubClientPendingRequests,
    frame_senders: HubClientFrameSenders,
) -> Result<()> {
    hub_client::register(lua, hub_event_tx, callback_registry, pending_requests, frame_senders)?;
    Ok(())
}
