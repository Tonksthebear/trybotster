//! Hub - Central orchestrator for agent management.
//!
//! The Hub is the core of botster, owning all state and running the main
//! event loop. It follows the centralized state store pattern where TUI and
//! Relay adapters query the Hub for state rather than owning it themselves.
//!
//! # Architecture
//!
//! ```text
//!            ┌──────────────────────┐
//!            │        Hub           │
//!            │  - Owns all state    │
//!            │  - Runs event loop   │
//!            │  - Source of truth   │
//!            └──────────┬───────────┘
//!                       │
//!        ┌──────────────┼──────────────┐
//!        │              │              │
//!        ▼              ▼              ▼
//!      TUI           Server         Relay
//!   (renders)     (Rails API)    (Browser WS)
//! ```
//!
//! # Module Structure
//!
//! - `server_comms`: WebSocket command channel, notification worker, registration
//! - `actions`: Hub action dispatch
//! - Agent lifecycle is fully Lua-owned (`handlers/agents.lua` + `lib/agent.lua`)
//! - `registration`: Device and hub registration
//!
//! # Usage
//!
//! ```ignore
//! let hub = Hub::new(config)?;
//! hub.run()?;  // Starts event loop with TUI
//! // or
//! hub.run_headless()?;  // Starts event loop without TUI
//! ```

// Rust guideline compliant 2026-02-04

pub mod action_cable_connection;
pub mod actions;
pub mod agent_handle;
pub mod daemon;
pub(crate) mod events;
pub mod handle_cache;
pub mod registration;
pub mod run;
mod server_comms;
pub mod state;

pub use actions::HubAction;
pub use agent_handle::AgentPtys;
pub use state::{HubState, SharedHubState};

use std::sync::{Arc, Mutex};
use std::time::Instant;

use reqwest::blocking::Client;

use sha2::{Digest, Sha256};

use crate::channel::Channel;
use crate::config::Config;
use crate::device::Device;
use crate::git::WorktreeManager;
use crate::lua::LuaRuntime;
use crate::lua::primitives::SharedServerId;

/// Queued PTY output message for WebRTC delivery.
///
/// Spawned forwarder tasks queue these messages; the main loop drains and sends.
/// Includes context for hook processing.
#[derive(Debug)]
pub struct WebRtcPtyOutput {
    /// Subscription ID for routing on the browser side.
    pub subscription_id: String,
    /// Browser identity for encryption.
    pub browser_identity: String,
    /// Raw PTY data (already prefixed with 0x01 for terminal output).
    pub data: Vec<u8>,
    /// Agent index for hook context.
    pub agent_index: usize,
    /// PTY index for hook context.
    pub pty_index: usize,
}

/// Pending observer notification for PTY output.
///
/// Queued during [`Hub::poll_webrtc_pty_output`] and drained separately in
/// [`Hub::poll_pty_observers`] to avoid blocking the WebRTC send path.
#[derive(Debug)]
pub struct PtyObserverNotification {
    /// Context for the hook callback.
    pub ctx: crate::lua::primitives::PtyOutputContext,
    /// Data that was sent (post-interception).
    pub data: Vec<u8>,
}

/// A PTY notification event queued by a watcher task for the Hub tick loop.
#[derive(Debug)]
pub struct PtyNotificationEvent {
    /// Agent key for the Lua hook context.
    pub agent_key: String,
    /// Session name (e.g., "cli", "server").
    pub session_name: String,
    /// The notification detected in PTY output.
    pub notification: crate::agent::AgentNotification,
}

/// Item queued for a per-peer async send task.
///
/// The Hub event loop pushes these into a bounded `mpsc` channel instead of
/// calling `block_in_place`. A dedicated `tokio::spawn` task per peer drains
/// the channel and performs the actual async DataChannel send with timeout.
#[derive(Debug)]
pub(crate) enum WebRtcSendItem {
    /// PTY output (hot path): subscription_id + raw data.
    Pty {
        /// Subscription ID for browser-side routing.
        subscription_id: String,
        /// Raw PTY data (already prefixed with 0x01 for terminal output).
        data: Vec<u8>,
    },
    /// JSON control message from Lua `webrtc.send()`.
    Json {
        /// Serialized JSON bytes.
        data: Vec<u8>,
    },
    /// Binary message from Lua `webrtc.send_binary()`.
    Binary {
        /// Raw binary data.
        data: Vec<u8>,
    },
    /// Stream multiplexer frame.
    Stream {
        /// Frame type byte.
        frame_type: u8,
        /// Stream identifier.
        stream_id: u16,
        /// Frame payload.
        payload: Vec<u8>,
    },
    /// Bundle refresh (ratchet restart, unencrypted).
    BundleRefresh {
        /// 161-byte DeviceKeyBundle.
        bundle_bytes: Vec<u8>,
    },
}

/// Per-peer send task state stored in the Hub.
///
/// When a WebRTC DataChannel opens, the Hub creates one of these per browser
/// identity. The bounded sender feeds items to a spawned async task that
/// performs the actual `send_pty_raw` / `send_to` calls with timeout.
/// Dropping the sender causes the task to exit naturally.
pub(crate) struct PeerSendState {
    /// Bounded channel sender for queuing send items.
    pub tx: tokio::sync::mpsc::Sender<WebRtcSendItem>,
    /// Set to `true` when the send task detects a dead peer (timeout/error).
    /// The event loop checks this to skip further sends (circuit breaker).
    pub dead: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Handle for the spawned send task (aborted on cleanup).
    pub task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for PeerSendState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerSendState")
            .field("dead", &self.dead.load(std::sync::atomic::Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// Capacity of the per-peer send channel.
///
/// 256 items provides enough buffering for bursty PTY output while bounding
/// memory usage (~1MB per peer at ~4KB per PTY chunk). When full, the event
/// loop drops the oldest item (same behavior as the previous bounded channel).
const PEER_SEND_CHANNEL_CAPACITY: usize = 256;

/// Timeout for individual DataChannel sends in per-peer tasks.
///
/// Dead peers cause SCTP retransmit backpressure that can block `send_data()`
/// for 60+ seconds. This timeout ensures the send task marks the peer as dead
/// rather than blocking indefinitely.
const PEER_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Maximum pending observer notifications before oldest are dropped.
///
/// Prevents unbounded memory growth if observers are registered but
/// the Lua callback is slow. At ~4KB per PTY chunk this caps at ~4MB.
const PTY_OBSERVER_QUEUE_CAPACITY: usize = 1024;

/// Generate a stable hub_identifier from a repo path.
///
/// Uses SHA256 hash of the absolute path to ensure the same repo
/// always gets the same hub_id, even across CLI restarts.
#[must_use]
pub fn hub_id_for_repo(repo_path: &std::path::Path) -> String {
    let canonical = repo_path
        .canonicalize()
        .unwrap_or_else(|_| repo_path.to_path_buf());

    let hash = Sha256::digest(canonical.to_string_lossy().as_bytes());

    // Use first 16 bytes as hex (32 chars) - enough uniqueness, shorter than UUID
    hash[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Central orchestrator for the botster application.
///
/// The Hub owns all application state and coordinates between the TUI,
/// server integration, and browser relay components. It can run in either
/// TUI mode (with terminal rendering) or headless mode (for CI/daemon use).
pub struct Hub {
    // === Core State ===
    /// Core agent and worktree state (shared for thread-safe access).
    pub state: SharedHubState,
    /// Application configuration.
    pub config: Config,
    /// HTTP client for server communication.
    pub client: Client,
    /// Device identity for E2E encryption.
    pub device: Device,

    // === Runtime ===
    /// Local identifier for this hub session (used for config directories).
    pub hub_identifier: String,
    /// Server-assigned ID for server communication (set after registration).
    pub botster_id: Option<String>,
    /// Shared copy of `botster_id` for Lua primitives (updated on registration).
    pub shared_server_id: SharedServerId,
    /// Async runtime for relay and preview channel operations.
    pub tokio_runtime: tokio::runtime::Runtime,

    // === Control Flags ===
    /// Whether the hub should quit.
    pub quit: bool,
    /// Whether to exec-restart after shutdown (for self-update).
    pub exec_restart: bool,
    /// Whether shutdown was requested as a graceful restart.
    ///
    /// When `true`, `shutdown()` calls `disconnect_graceful()` on the broker
    /// so it preserves PTY file descriptors across the restart window.
    /// When `false` (plain quit), `kill_all` is sent and the broker exits.
    pub graceful_shutdown: bool,

    // === Browser Relay ===
    /// Browser connection state and communication.
    pub browser: crate::relay::BrowserState,

    // === WebRTC Channels ===
    /// WebRTC peer connections indexed by browser identity.
    ///
    /// Each browser that connects via WebRTC gets its own peer connection.
    /// The connection persists to keep the DataChannel alive.
    pub webrtc_channels: std::collections::HashMap<String, crate::channel::WebRtcChannel>,

    /// Tracks when WebRTC connections were initiated.
    ///
    /// Used to timeout connections stuck in "Connecting" state (e.g., ICE
    /// negotiation that never completes due to network issues).
    /// Connections that don't reach "Connected" within 30 seconds are cleaned up.
    webrtc_connection_started: std::collections::HashMap<String, Instant>,

    /// Per-peer async send tasks, keyed by browser identity.
    ///
    /// Each entry has a bounded channel sender + a spawned task that drains
    /// items and performs the actual async DataChannel send. Created when
    /// `DcOpened` fires; dropped when the peer disconnects.
    pub(crate) webrtc_send_tasks: std::collections::HashMap<String, PeerSendState>,

    /// Periodic DataChannel ping tasks, keyed by browser identity.
    ///
    /// Each task sends `{ "type": "dc_ping" }` every 10 seconds through the
    /// peer's send channel. The browser responds with `dc_pong`; if pongs
    /// stop arriving, the browser detects the dead connection and reconnects.
    /// Aborted when the peer disconnects (cleanup_webrtc_channel).
    dc_ping_tasks: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,

    /// Pending close notifications keyed by Olm identity key.
    ///
    /// When a WebRTC channel is cleaned up, its `close_complete` watch receiver
    /// is stored here. Before creating a replacement channel for the same device,
    /// the offer handler awaits `wait_for(|v| *v)` (with timeout) to ensure old
    /// sockets are released first, preventing fd exhaustion from rapid reconnection
    /// cycles. Using `watch` instead of `Notify` avoids the race where the close
    /// signal fires before anyone is waiting.
    webrtc_pending_closes: std::collections::HashMap<String, tokio::sync::watch::Receiver<bool>>,

    /// Sender for PTY output messages from forwarder tasks.
    ///
    /// Forwarder tasks send PTY output here; main loop drains and sends via WebRTC.
    pub webrtc_pty_output_tx: tokio::sync::mpsc::UnboundedSender<WebRtcPtyOutput>,
    /// Receiver for PTY output messages.
    ///
    /// Wrapped in `Option` so the event loop can extract it for `tokio::select!`.
    pub webrtc_pty_output_rx: Option<tokio::sync::mpsc::UnboundedReceiver<WebRtcPtyOutput>>,

    /// Active PTY forwarder task handles for cleanup on unsubscribe.
    ///
    /// Maps subscriptionId -> JoinHandle for the forwarder task.
    pty_forwarders: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,

    /// Sender for outgoing WebRTC signals (ICE candidates) from async callbacks.
    ///
    /// Cloned for each new WebRTC channel. The async `on_ice_candidate` callback
    /// encrypts the candidate and sends it here. `poll_outgoing_signals()` drains
    /// the receiver and relays via `ChannelHandle::perform("signal", ...)`.
    pub webrtc_outgoing_signal_tx: tokio::sync::mpsc::UnboundedSender<crate::channel::webrtc::OutgoingSignal>,
    /// Receiver for outgoing WebRTC signals. Drained in `tick()`.
    ///
    /// Wrapped in `Option` so the event loop can extract it for `tokio::select!`.
    webrtc_outgoing_signal_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::channel::webrtc::OutgoingSignal>>,

    /// TCP stream multiplexers per browser identity for preview tunneling.
    stream_muxes: std::collections::HashMap<String, crate::relay::stream_mux::StreamMultiplexer>,
    /// Receiver for incoming stream frames from WebRTC DataChannels.
    ///
    /// Wrapped in `Option` so the event loop can extract it for `tokio::select!`.
    stream_frame_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::channel::webrtc::StreamIncoming>>,
    /// Sender for incoming stream frames (cloned into each WebRtcChannel).
    pub stream_frame_tx: tokio::sync::mpsc::UnboundedSender<crate::channel::webrtc::StreamIncoming>,
    /// Receiver for binary PTY input from WebRTC DataChannels (bypasses JSON/Lua).
    ///
    /// Wrapped in `Option` so the event loop can extract it for `tokio::select!`.
    pty_input_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::channel::webrtc::PtyInputIncoming>>,
    /// Sender for binary PTY input (cloned into each WebRtcChannel).
    pub pty_input_tx: tokio::sync::mpsc::UnboundedSender<crate::channel::webrtc::PtyInputIncoming>,
    /// Receiver for file transfers from browser via WebRTC DataChannels.
    ///
    /// Wrapped in `Option` so the event loop can extract it for `tokio::select!`.
    file_input_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::channel::webrtc::FileInputIncoming>>,
    /// Sender for file transfers (cloned into each WebRtcChannel).
    pub file_input_tx: tokio::sync::mpsc::UnboundedSender<crate::channel::webrtc::FileInputIncoming>,
    /// Temp files from browser paste/drop, keyed by agent session key.
    /// Cleaned up when the agent is closed.
    paste_files: std::collections::HashMap<String, Vec<std::path::PathBuf>>,

    // === Handle Cache ===
    /// Thread-safe cache of agent handles for non-blocking client access.
    ///
    /// Updated by Lua via `hub.register_agent()` and `hub.unregister_agent()`.
    /// `HandleCache::get_agent()` reads from this cache directly, allowing clients
    /// to access agent handles without blocking commands - safe from any thread.
    pub handle_cache: Arc<handle_cache::HandleCache>,

    // === Lua Scripting ===
    /// Lua scripting runtime for hot-reloadable behavior customization.
    pub lua: LuaRuntime,

    // === Lua ActionCable ===
    /// Lua-managed ActionCable connections keyed by connection ID.
    lua_ac_connections: std::collections::HashMap<String, crate::lua::primitives::action_cable::LuaAcConnection>,
    /// Lua-managed ActionCable channel subscriptions keyed by channel ID.
    lua_ac_channels: std::collections::HashMap<String, crate::lua::primitives::action_cable::LuaAcChannel>,

    // === Lua Hub Client ===
    /// Lua-managed outgoing hub client connections keyed by connection ID.
    lua_hub_client_connections: std::collections::HashMap<String, crate::lua::primitives::hub_client::LuaHubClientConn>,

    /// Pending PTY output observer notifications.
    ///
    /// Populated during [`Self::poll_webrtc_pty_output`] (after WebRTC send),
    /// drained independently in [`Self::poll_pty_observers`] so slow observers
    /// never block the WebRTC fast path.
    pty_observer_queue: std::collections::VecDeque<PtyObserverNotification>,

    /// Pending PTY notification events from watcher tasks (test-only fallback).
    ///
    /// Production path uses `HubEvent::PtyNotification` via the event channel.
    /// Tests without the event bus still push to this queue and drain it
    /// in the `#[cfg(test)]` `tick()` method.
    #[cfg(test)]
    pty_notification_queue: std::sync::Arc<std::sync::Mutex<Vec<PtyNotificationEvent>>>,

    /// Count of PTY output messages processed by `poll_webrtc_pty_output`.
    ///
    /// Incremented for each message drained from the channel, regardless of
    /// whether the WebRTC send succeeds. Used by regression tests to verify
    /// that messages are not silently dropped by the `select!` pattern.
    #[cfg(test)]
    pub(crate) pty_output_messages_drained: usize,

    /// Handles for notification watcher tasks, keyed by "{agent_key}:{session_name}".
    notification_watcher_handles: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,

    /// Tracks peers that received a ratchet restart during the current cleanup window.
    /// Cleared every `CleanupTick` (5s) to coalesce decrypt failure storms.
    ratchet_restarted_peers: std::collections::HashSet<String>,

    // === Web Push Notifications ===
    /// VAPID keys for web push authentication (loaded on startup).
    pub(crate) vapid_keys: Option<crate::notifications::vapid::VapidKeys>,
    /// Browser push subscriptions (persisted to encrypted storage).
    pub(crate) push_subscriptions: crate::notifications::push::PushSubscriptionStore,

    // === Socket IPC ===
    /// Unix domain socket server for external client connections.
    socket_server: Option<crate::socket::server::SocketServer>,
    /// Connected socket clients, keyed by client_id.
    socket_clients: std::collections::HashMap<String, crate::socket::client_conn::SocketClientConn>,

    // === PTY Broker ===
    /// Connection to the PTY broker process.
    ///
    /// Wrapped in `Arc<Mutex<Option<...>>>` so the Lua `hub.register_pty_with_broker()`
    /// primitive can access it without going through Hub commands.
    ///
    /// `None` if the broker has not been started or the connection failed.
    pub broker_connection: crate::broker::SharedBrokerConnection,

    /// Maps broker session IDs to their owning agent and PTY index.
    ///
    /// Populated when Lua fires `HubEvent::BrokerSessionRegistered` after
    /// calling `hub.register_pty_with_broker()`. Used to route incoming
    /// `HubEvent::BrokerPtyOutput` frames to the correct `PtyHandle`.
    broker_sessions: std::collections::HashMap<u32, (String, usize)>,

    // === TUI via Lua (Hub-side Processing) ===
    /// Sender for TUI output messages to TuiRunner.
    ///
    /// Set by `register_tui_via_lua()`. Hub sends `TuiOutput` messages
    /// through this channel directly.
    tui_output_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::client::TuiOutput>>,
    /// Write end of the TUI wake pipe.
    ///
    /// When set, Hub writes 1 byte after sending to `tui_output_tx` to wake
    /// the TUI thread from its blocking `libc::poll()`. This replaces
    /// the old `thread::sleep(16ms)` polling in TuiRunner.
    pub(crate) tui_wake_fd: Option<std::os::unix::io::RawFd>,
    /// Receiver for TUI requests from TuiRunner.
    ///
    /// Set by `register_tui_via_lua()`. Polled by `poll_tui_requests()`.
    tui_request_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::client::TuiRequest>>,

    // === Async Worktree Creation ===
    /// Sender for async worktree creation results from blocking tasks.
    ///
    /// Cloned into each `spawn_blocking` task. Results are polled in
    /// `poll_worktree_results()` during `tick()`.
    worktree_result_tx: crate::lua::primitives::WorktreeResultSender,
    /// Receiver for async worktree creation results.
    ///
    /// Drained in `poll_worktree_results()` which fires Lua events
    /// to resume agent spawning. Wrapped in `Option` so the event loop
    /// can extract it for `tokio::select!`.
    worktree_result_rx: Option<crate::lua::primitives::WorktreeResultReceiver>,

    // === Unified Event Channel ===
    /// Sender for the unified event bus. Cloned to background producers
    /// (HTTP threads, WebSocket threads, timer tasks, etc.) so they can
    /// deliver events to the Hub event loop without polling.
    pub(crate) hub_event_tx: tokio::sync::mpsc::UnboundedSender<events::HubEvent>,
    /// Receiver for the unified event bus. Extracted into the `select!`
    /// loop by `run_event_loop()`.
    hub_event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<events::HubEvent>>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("state", &self.state)
            .field("hub_identifier", &self.hub_identifier)
            .field("quit", &self.quit)
            .finish_non_exhaustive()
    }
}

impl Hub {
    /// Create a new Hub with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The tokio runtime cannot be created
    /// - The HTTP client cannot be created
    /// - Device identity cannot be loaded
    pub fn new(config: Config) -> anyhow::Result<Self> {
        use std::sync::RwLock;
        use std::time::Duration;

        let state = Arc::new(RwLock::new(HubState::new(config.worktree_base.clone())));
        let tokio_runtime = tokio::runtime::Runtime::new()?;

        // Generate stable hub_identifier: env var > repo path hash > cwd hash
        let hub_identifier = if let Ok(id) = std::env::var("BOTSTER_HUB_ID") {
            log::info!("Hub identifier (from env): {}...", &id[..id.len().min(8)]);
            id
        } else {
            match WorktreeManager::detect_current_repo() {
                Ok((repo_path, _)) => {
                    let id = hub_id_for_repo(&repo_path);
                    log::info!("Hub identifier (from repo): {}...", &id[..8]);
                    id
                }
                Err(_) => {
                    // Not in a git repo — derive hub ID from current working directory
                    let cwd = std::env::current_dir()?;
                    let id = hub_id_for_repo(&cwd);
                    log::info!("Hub identifier (from cwd): {}...", &id[..8]);
                    id
                }
            }
        };

        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        // Load or create device identity for E2E encryption
        let device = Device::load_or_create()?;
        log::info!("Device fingerprint: {}", device.fingerprint);

        // Create handle cache for thread-safe agent handle access
        let handle_cache = Arc::new(handle_cache::HandleCache::new());
        // Create channel for WebRTC PTY output from forwarder tasks
        let (webrtc_pty_output_tx, webrtc_pty_output_rx) = tokio::sync::mpsc::unbounded_channel();
        // Create channel for outgoing WebRTC signals (ICE candidates from async callbacks)
        let (webrtc_outgoing_signal_tx, webrtc_outgoing_signal_rx) = tokio::sync::mpsc::unbounded_channel();
        // Create channel for incoming stream multiplexer frames from WebRTC DataChannels
        let (stream_frame_tx, stream_frame_rx) = tokio::sync::mpsc::unbounded_channel();
        // Create channel for binary PTY input from WebRTC DataChannels
        let (pty_input_tx, pty_input_rx) = tokio::sync::mpsc::unbounded_channel();
        // Create channel for file transfers from browser via WebRTC DataChannels
        let (file_input_tx, file_input_rx) = tokio::sync::mpsc::unbounded_channel();
        // Create channel for async worktree creation results
        let (worktree_result_tx, worktree_result_rx) = tokio::sync::mpsc::unbounded_channel();
        // Unified event bus for background producers (HTTP, WS, timers, etc.)
        let (hub_event_tx, hub_event_rx) = tokio::sync::mpsc::unbounded_channel();

        // Initialize Lua scripting runtime
        let mut lua = LuaRuntime::new()?;

        // Wire the unified event bus into Lua primitive registries so background
        // threads can send events directly instead of pushing to shared vecs.
        lua.set_hub_event_tx(hub_event_tx.clone(), tokio_runtime.handle().clone());

        Ok(Self {
            state,
            config,
            client,
            device,
            hub_identifier,
            botster_id: None,
            shared_server_id: Arc::new(Mutex::new(None)),
            tokio_runtime,
            quit: false,
            exec_restart: false,
            graceful_shutdown: false,
            browser: crate::relay::BrowserState::default(),
            handle_cache,
            webrtc_channels: std::collections::HashMap::new(),
            webrtc_connection_started: std::collections::HashMap::new(),
            webrtc_send_tasks: std::collections::HashMap::new(),
            dc_ping_tasks: std::collections::HashMap::new(),
            webrtc_pending_closes: std::collections::HashMap::new(),
            webrtc_pty_output_tx,
            webrtc_pty_output_rx: Some(webrtc_pty_output_rx),
            pty_forwarders: std::collections::HashMap::new(),
            webrtc_outgoing_signal_tx,
            webrtc_outgoing_signal_rx: Some(webrtc_outgoing_signal_rx),
            stream_muxes: std::collections::HashMap::new(),
            stream_frame_rx: Some(stream_frame_rx),
            stream_frame_tx,
            pty_input_rx: Some(pty_input_rx),
            pty_input_tx,
            file_input_rx: Some(file_input_rx),
            file_input_tx,
            paste_files: std::collections::HashMap::new(),
            lua,
            lua_ac_connections: std::collections::HashMap::new(),
            lua_ac_channels: std::collections::HashMap::new(),
            lua_hub_client_connections: std::collections::HashMap::new(),
            pty_observer_queue: std::collections::VecDeque::new(),
            #[cfg(test)]
            pty_notification_queue: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            pty_output_messages_drained: 0,
            notification_watcher_handles: std::collections::HashMap::new(),
            ratchet_restarted_peers: std::collections::HashSet::new(),
            vapid_keys: None,
            push_subscriptions: crate::notifications::push::PushSubscriptionStore::default(),
            socket_server: None,
            socket_clients: std::collections::HashMap::new(),
            broker_connection: Arc::new(Mutex::new(None)),
            broker_sessions: std::collections::HashMap::new(),
            tui_output_tx: None,
            tui_wake_fd: None,
            tui_request_rx: None,
            worktree_result_tx,
            worktree_result_rx: Some(worktree_result_rx),
            hub_event_tx,
            hub_event_rx: Some(hub_event_rx),
        })
    }

    /// Get the hub ID to use for server communication.
    ///
    /// Returns the server-assigned `botster_id` if available (after registration),
    /// otherwise falls back to local `hub_identifier`.
    #[must_use]
    pub fn server_hub_id(&self) -> &str {
        self.botster_id.as_deref().unwrap_or(&self.hub_identifier)
    }

    /// Check if the hub should quit.
    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Request the hub to quit.
    pub fn request_quit(&mut self) {
        self.quit = true;
    }

    /// Handle a hub action.
    ///
    /// This is the central dispatch point for all actions. TUI input,
    /// browser events, and server messages all eventually become actions
    /// that are processed here.
    ///
    /// Delegates to `actions::dispatch()` for the actual processing.
    pub fn handle_action(&mut self, action: HubAction) {
        actions::dispatch(self, action);
    }

    /// Load available worktrees for the selection UI.
    ///
    /// Delegates to `HubState::load_available_worktrees()` and syncs
    /// to HandleCache for non-blocking client reads.
    pub fn load_available_worktrees(&mut self) -> anyhow::Result<()> {
        self.state.write().unwrap().load_available_worktrees()?;
        // Sync to HandleCache so clients can read without blocking commands
        let worktrees = self.state.read().unwrap().available_worktrees.clone();
        self.handle_cache.set_worktrees(worktrees);
        Ok(())
    }


    // === Event Loop ===

    /// Perform all initial setup steps.
    ///
    /// Note: DeviceKeyBundle generation is deferred until the connection
    /// URL is first requested (TUI QR display, external automation, etc.).
    /// This avoids blocking boot on bundle generation.
    pub fn setup(&mut self) {
        if !crate::env::is_test_mode() {
            self.register_device();
            self.register_hub_with_server();
        }
        self.init_crypto_service();
        self.init_web_push();

        // ActionCable connections are now managed by Lua plugins
        // (hub_commands.lua and github.lua handle subscription lifecycle)

        // Seed shared state so clients have data immediately
        if let Err(e) = self.load_available_worktrees() {
            log::warn!("Failed to load initial worktrees: {}", e);
        }

        // Register Hub primitives with Lua runtime (must happen before loading init script)
        if let Err(e) = self.lua.register_hub_primitives(
            Arc::clone(&self.handle_cache),
            self.config.worktree_base.clone(),
            self.hub_identifier.clone(),
            Arc::clone(&self.shared_server_id),
            Arc::clone(&self.state),
            Arc::clone(&self.broker_connection),
        ) {
            log::warn!("Failed to register Hub Lua primitives: {}", e);
        }

        // Load Lua init script (hot-reload is now handled by Lua's module_watcher)
        self.load_lua_init();

        // Connect to (or spawn) the PTY broker for zero-downtime restarts.
        // Must run after load_lua_init so Lua can call hub.register_pty_with_broker().
        if !crate::env::is_test_mode() {
            self.try_connect_broker();
        }


        // Bundle generation is deferred - don't call generate_connection_url() here.
        // The bundle will be generated lazily when:
        // 1. TUI requests QR code display (GetConnectionCode command)
        // 2. External automation requests the connection URL
        // 3. Headless mode calls setup_headless() which eagerly generates it
        // This avoids blocking boot for up to 10 seconds in TUI mode.
    }

    // === PTY Broker ===

    /// Attempt to connect to an existing broker process, or spawn a new one.
    ///
    /// Called during startup after the socket server is initialised.
    ///
    /// # Flow
    ///
    /// 1. Check for an existing broker socket (`/tmp/botster-{uid}/broker-{hub_id}.sock`).
    /// 2. If found, attempt to connect and ping — reuse if alive.
    /// 3. If no socket or connection fails, spawn a new broker subprocess and
    ///    connect to it.
    /// 4. On success, configure the reconnect timeout and start the async
    ///    output forwarder thread.
    pub fn try_connect_broker(&mut self) {
        let path = match crate::broker::broker_socket_path(&self.hub_identifier) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("[broker] could not resolve socket path: {e}");
                return;
            }
        };

        // Try to connect to an existing broker.
        let existing_conn = if path.exists() {
            match crate::broker::BrokerConnection::connect(&path) {
                Ok(mut conn) => {
                    if conn.ping().is_ok() {
                        log::info!("[broker] reconnected to existing broker at {}", path.display());
                        Some(conn)
                    } else {
                        log::debug!("[broker] existing broker socket unresponsive, will spawn fresh");
                        None
                    }
                }
                Err(e) => {
                    log::debug!("[broker] could not connect to existing broker: {e}");
                    None
                }
            }
        } else {
            None
        };

        // Track whether this is a reconnect (existing broker) vs a fresh spawn.
        // Used below to fire the `broker_reconnected` Lua event so the restart
        // recovery handler can replay scrollback for surviving agents.
        let is_reconnect = existing_conn.is_some();

        let mut conn = if let Some(c) = existing_conn {
            c
        } else {
            // No live broker — spawn one then retry until it binds (up to 500 ms).
            self.spawn_broker();
            let conn_opt = (0u8..10).find_map(|attempt| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                match crate::broker::BrokerConnection::connect(&path) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        log::debug!("[broker] connect attempt {}: {e}", attempt + 1);
                        None
                    }
                }
            });
            match conn_opt {
                Some(c) => c,
                None => {
                    log::warn!("[broker] failed to connect after 10 attempts (500 ms)");
                    return;
                }
            }
        };

        // Configure the reconnect window.
        if let Err(e) = conn.set_timeout(120) {
            log::warn!("[broker] failed to set timeout: {e}");
        }

        // Install the demux reader thread before storing conn.
        //
        // The demux thread is the sole reader of the broker socket. It routes:
        //   PtyOutput / PtyExited   → hub_event_tx (HubEvent channel)
        //   Control frames          → BrokerConnection::response_rx (channel)
        //
        // After install_forwarder(), read_response() no longer reads the socket
        // directly, eliminating the race where the forwarder and read_response()
        // both competed to consume the same Registered frame off the shared
        // kernel receive buffer (which caused silent registration failure and
        // zero TUI output).
        //
        // install_forwarder() is called BEFORE broker_reconnected fires so the
        // demux thread is running when get_pty_snapshot_from_broker() calls
        // read_response() inside the Lua callback — responses come through the
        // channel, not the socket, so there is no contention.
        if let Err(e) = conn.install_forwarder(self.hub_event_tx.clone()) {
            log::warn!("[broker] failed to install demux reader: {e}; output forwarding disabled");
        }

        // Store the connection before firing broker_reconnected so that
        // get_pty_snapshot_from_broker() works inside the Lua callback.
        *self.broker_connection.lock().unwrap() = Some(conn);
        log::info!("[broker] connected");

        if is_reconnect {
            if let Err(e) = self.lua.fire_json_event("broker_reconnected", &serde_json::json!({})) {
                log::warn!("[broker] broker_reconnected event error: {e}");
            }
        }
    }

    /// Spawn the broker subprocess detached from the current process.
    ///
    /// Runs `botster broker --hub-id <id> --timeout 120` as a background
    /// process. It exits on its own when the reconnect timeout expires.
    fn spawn_broker(&self) {
        let hub_id = self.hub_identifier.clone();
        match std::env::current_exe() {
            Ok(exe) => {
                match std::process::Command::new(&exe)
                    .args(["broker", "--hub-id", &hub_id, "--timeout", "120"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(child) => {
                        log::info!("[broker] spawned broker process (pid {})", child.id());
                        // Detach — the broker manages its own lifetime.
                        std::mem::forget(child);
                    }
                    Err(e) => log::warn!("[broker] failed to spawn broker: {e}"),
                }
            }
            Err(e) => log::warn!("[broker] could not locate current exe for broker spawn: {e}"),
        }
    }

    /// Start the Unix domain socket server for IPC.
    ///
    /// Creates the socket at `/tmp/botster-{uid}/{hub_id}.sock`,
    /// writes a PID file, and begins accepting client connections.
    /// Socket events are delivered via `HubEvent` variants.
    pub fn start_socket_server(&mut self) {
        let _guard = self.tokio_runtime.enter();

        // Clean up stale files from previous runs
        daemon::cleanup_stale_files(&self.hub_identifier);

        // Sweep orphaned sockets left by crashed/killed processes
        daemon::cleanup_orphaned_sockets();

        // Write PID file
        if let Err(e) = daemon::write_pid_file(&self.hub_identifier) {
            log::warn!("Failed to write PID file: {e}");
        }

        // Start socket server
        match daemon::socket_path(&self.hub_identifier) {
            Ok(path) => {
                match crate::socket::server::SocketServer::start(path, self.hub_event_tx.clone()) {
                    Ok(server) => {
                        log::info!("Socket server started for hub {}", &self.hub_identifier[..self.hub_identifier.len().min(8)]);
                        self.socket_server = Some(server);
                    }
                    Err(e) => {
                        log::warn!("Failed to start socket server: {e}");
                    }
                }
            }
            Err(e) => {
                log::warn!("Failed to resolve socket path: {e}");
            }
        }
    }

    /// Eagerly generate the connection URL.
    ///
    /// In headless mode there is no TUI to trigger lazy generation, so
    /// external tools (system tests, automation) need the URL written to
    /// disk at startup.
    pub fn eager_generate_connection_url(&mut self) {
        match self.generate_connection_url() {
            Ok(url) => log::info!("Connection URL generated ({} chars)", url.len()),
            Err(e) => log::warn!("Failed to generate connection URL: {e}"),
        }
    }

    /// Load the Lua initialization script.
    ///
    /// Module resolution priority (highest to lowest):
    /// 1. Project root (`{repo}/.botster/lua/`) — project-specific overrides
    /// 2. Userspace (`~/.botster/lua/`) — user overrides
    /// 3. Embedded (compiled from `cli/lua/`) — fallback/base
    ///
    /// Debug builds skip embedded entirely — they load from `cli/lua/`
    /// filesystem with hot-reload support.
    pub(crate) fn load_lua_init(&mut self) {
        // In debug builds, use source directory for hot-reload during development
        #[cfg(debug_assertions)]
        {
            let dev_lua_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua");
            let dev_init_path = dev_lua_dir.join("hub").join("init.lua");

            if dev_init_path.exists() {
                log::info!("Dev mode: using Lua files from {}", dev_lua_dir.display());

                // Update base path for module resolution
                self.lua.set_base_path(dev_lua_dir.clone());

                // Expose base path to Lua so module_watcher can watch core modules
                if let Err(e) = self.lua.lua().globals().set(
                    "_lua_base_path",
                    dev_lua_dir.to_string_lossy().to_string(),
                ) {
                    log::warn!("Failed to set _G._lua_base_path: {}", e);
                }

                // Update package.path for require() calls
                if let Err(e) = self.lua.update_package_path(&dev_lua_dir) {
                    log::warn!("Failed to update package.path: {}", e);
                }

                // Load the init script
                if let Err(e) = self.lua.load_file_absolute(&dev_init_path) {
                    log::warn!("Failed to load dev init.lua: {}", e);
                }
                return;
            }
        }

        // Release mode: add project root to package.path (highest priority).
        // update_package_path prepends, so project root is searched before
        // the userspace ~/.botster/lua/ that setup_package_path already configured.
        if let Ok((repo_path, _)) = crate::git::WorktreeManager::detect_current_repo() {
            let project_lua = repo_path.join(".botster").join("lua");
            if project_lua.exists() {
                log::info!("Adding project Lua path: {}", project_lua.display());
                if let Err(e) = self.lua.update_package_path(&project_lua) {
                    log::warn!("Failed to add project Lua path: {}", e);
                }
            }
        }

        // Load embedded Lua as fallback (searcher appended to end of package.searchers).
        log::info!("Loading embedded Lua files");
        if let Err(e) = self.lua.load_embedded() {
            log::warn!("Failed to load embedded Lua: {}", e);
        }
    }

    /// Run the Hub event loop without TUI.
    ///
    /// For TUI mode, use `crate::tui::run_with_hub()` instead - the TUI
    /// module now owns TuiRunner instantiation.
    ///
    /// For headless mode, use `hub::run::run_headless_loop()`.
    pub fn run_headless(
        &mut self,
        shutdown_flag: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<()> {
        run::run_headless_loop(self, shutdown_flag)
    }

    /// Send shutdown notification to server and cleanup resources.
    pub fn shutdown(&mut self) {
        // Signal the PTY broker before any other cleanup.
        //
        // - graceful_shutdown || exec_restart → disconnect_graceful(): broker
        //   keeps PTY FDs alive for the reconnect window so agents survive.
        //   The broker kills PTYs itself if the Hub does not reconnect within
        //   its configured timeout (120 s by default).
        // - plain quit (e.g., ctrl+c on headless) → kill_all(): broker kills
        //   children immediately and exits, avoiding orphan processes.
        if let Ok(mut guard) = self.broker_connection.lock() {
            if let Some(conn) = guard.take() {
                if self.graceful_shutdown || self.exec_restart {
                    log::info!("[broker] graceful disconnect — PTYs preserved for reconnect window");
                    conn.disconnect_graceful();
                } else {
                    log::info!("[broker] kill_all (clean shutdown)");
                    conn.kill_all();
                }
            }
        }

        // Disconnect all socket clients
        for (client_id, conn) in self.socket_clients.drain() {
            log::debug!("Disconnecting socket client: {}", client_id);
            conn.disconnect();
        }
        // Shutdown socket server
        if let Some(server) = self.socket_server.take() {
            server.shutdown();
        }
        // Clean up daemon files (PID, socket)
        daemon::cleanup_on_shutdown(&self.hub_identifier);

        // Notify Lua that TUI is disconnecting
        if let Err(e) = self.lua.call_tui_disconnected() {
            log::warn!("Lua tui_disconnected callback error: {}", e);
        }

        // Fire Lua shutdown event (before any cleanup)
        if let Err(e) = self.lua.fire_shutdown() {
            log::warn!("Lua shutdown event error: {}", e);
        }

        // Stop all file watcher forwarder tasks (Lua hot-reload + user watches).
        // These are spawn_blocking tasks that block on rx.recv() — the senders
        // live inside FileWatcher (owned by LuaRuntime). If we don't stop them
        // here, tokio::Runtime::drop will deadlock waiting for tasks that can
        // never complete (the senders drop AFTER the runtime in struct field order).
        self.lua.stop_all_watchers();

        // Abort all PTY forwarder tasks
        for (_key, task) in self.pty_forwarders.drain() {
            task.abort();
        }

        // Abort all notification watcher tasks
        for (_key, task) in self.notification_watcher_handles.drain() {
            task.abort();
        }

        // Close all stream multiplexers
        for (_id, mut mux) in self.stream_muxes.drain() {
            mux.close_all();
        }

        // Shut down per-peer send tasks (dropping sender causes task exit)
        for (_id, state) in self.webrtc_send_tasks.drain() {
            drop(state.tx);
            state.task.abort();
        }

        // Disconnect all WebRTC channels (fire-and-forget to avoid deadlock)
        for (_id, mut channel) in self.webrtc_channels.drain() {
            self.tokio_runtime.spawn(async move {
                channel.disconnect().await;
            });
        }
        self.webrtc_connection_started.clear();

        // Persist crypto session state to disk on shutdown
        if let Some(ref cs) = self.browser.crypto_service {
            match cs.lock() {
                Ok(guard) => {
                    if let Err(e) = guard.persist() {
                        log::warn!("CryptoService persist failed: {e}");
                    }
                }
                Err(e) => {
                    log::warn!("CryptoService mutex poisoned on shutdown: {e}");
                }
            }
        }

        // Notify server of shutdown
        registration::shutdown(
            &self.client,
            &self.config.server_url,
            self.server_hub_id(),
            self.config.get_api_key(),
        );
    }

    /// Register TUI with Hub-side request processing.
    ///
    /// Hub processes TUI requests directly in its tick loop (no async task).
    ///
    /// Notifies Lua that the TUI is connected, registering it in the shared
    /// connection registry alongside browser clients.
    ///
    /// # Arguments
    ///
    /// * `request_rx` - Receiver for TUI requests (JSON + raw PTY input)
    ///
    /// # Returns
    ///
    /// Receiver for TuiOutput messages to TuiRunner.
    pub fn register_tui_via_lua(
        &mut self,
        request_rx: tokio::sync::mpsc::UnboundedReceiver<crate::client::TuiRequest>,
    ) -> tokio::sync::mpsc::UnboundedReceiver<crate::client::TuiOutput> {
        use crate::client::TuiOutput;

        let (output_tx, output_rx) = tokio::sync::mpsc::unbounded_channel::<TuiOutput>();

        // Store channels for Hub-side processing
        self.tui_output_tx = Some(output_tx);
        self.tui_request_rx = Some(request_rx);

        // Notify Lua that TUI is connected (registers in connection registry)
        if let Err(e) = self.lua.call_tui_connected() {
            log::warn!("Lua tui_connected callback error: {}", e);
        }

        log::info!("TUI registered via Lua (Hub-side processing)");

        output_rx
    }

    /// Write 1 byte to the TUI wake pipe to unblock its `libc::poll()`.
    ///
    /// Safe to call from any thread — pipe writes ≤ PIPE_BUF are atomic.
    /// No-op if no TUI wake pipe is configured (headless mode).
    pub(crate) fn wake_tui(&self) {
        if let Some(fd) = self.tui_wake_fd {
            wake_tui_pipe(fd);
        }
    }

    /// Generate connection URL, lazily generating bundle if needed.
    ///
    /// Format: `{server_url}/hubs/{id}#{base32_binary_bundle}`
    /// - URL portion: byte mode (any case allowed)
    /// - Bundle (after #): alphanumeric mode (uppercase Base32)
    ///
    /// On first call, this generates the PreKeyBundle (lazy initialization).
    /// Subsequent calls return the cached bundle unless it was used (in which
    /// case a fresh bundle is auto-generated).
    ///
    /// Always updates HandleCache so `connection.get_url()` in Lua returns
    /// the current value.
    pub(crate) fn generate_connection_url(&mut self) -> Result<String, String> {
        let result = self.get_or_generate_connection_url();
        // Always update cache so Lua connection.get_url() returns current value
        self.handle_cache.set_connection_url(result.clone());
        result
    }

}

impl Drop for Hub {
    /// Safety net: stop all blocking watcher tasks before the runtime drops.
    ///
    /// Rust drops struct fields in declaration order. `tokio_runtime` is
    /// declared before `lua`, so it drops first. But `lua` owns file watcher
    /// forwarder tasks (`spawn_blocking`) that block on `rx.recv()` — the
    /// senders live inside `FileWatcher` (also owned by `lua`). If those
    /// tasks aren't stopped before the runtime drops, `Runtime::drop` blocks
    /// forever waiting for tasks that can never complete.
    ///
    /// `shutdown()` handles this in the normal path. This `Drop` impl is the
    /// safety net for panic unwinds, early returns, or any path that skips
    /// `shutdown()`.
    fn drop(&mut self) {
        // Clean up any remaining paste files
        let keys: Vec<String> = self.paste_files.keys().cloned().collect();
        for key in keys {
            self.cleanup_paste_files(&key);
        }
        self.lua.stop_all_watchers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_config() -> Config {
        Config {
            server_url: "http://localhost:3000".to_string(),
            token: "btstr_test-key".to_string(),
            poll_interval: 10,
            agent_timeout: 300,
            max_sessions: 10,
            worktree_base: PathBuf::from("/tmp/test-worktrees"),
            hub_name: None,
        }
    }

    #[test]
    fn test_hub_creation() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

        assert!(!hub.should_quit());
    }

    /// Verifies Hub drop completes without deadlocking.
    ///
    /// Regression test for a drop-order deadlock: `tokio_runtime` is declared
    /// before `lua` in Hub, so it drops first. But `lua` owns `spawn_blocking`
    /// watcher forwarder tasks that block on `rx.recv()` — the senders live
    /// inside `FileWatcher` (also owned by `lua`). Without the `Drop` impl,
    /// runtime drop blocks forever waiting for tasks that can never complete.
    ///
    /// The fix: `Hub::drop()` calls `lua.stop_all_watchers()` before the
    /// runtime drops, aborting forwarder tasks and dropping watchers so the
    /// blocking pool can shut down cleanly.
    #[test]
    fn test_hub_drop_completes_with_shutdown() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let done = std::sync::Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();

        let handle = std::thread::spawn(move || {
            let config = test_config();
            let mut hub = Hub::new(config).unwrap();

            let tx = hub.hub_event_tx.clone();
            hub.lua.set_hub_event_tx(tx, hub.tokio_runtime.handle().clone());

            // Simulate the shutdown path: call shutdown then drop.
            // shutdown() stops watchers, and Drop is the safety net.
            hub.shutdown();
            drop(hub);

            done_clone.store(true, Ordering::SeqCst);
        });

        // Wait up to 5 seconds for Hub drop to complete.
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(5) {
            if done.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        assert!(
            done.load(Ordering::SeqCst),
            "Hub::drop deadlocked — watcher forwarder tasks were not stopped \
             before the tokio runtime dropped"
        );

        handle.join().expect("Hub drop thread should not panic");
    }

    /// Verifies Hub drop completes even without calling shutdown().
    ///
    /// The `Drop` impl must handle this case (panic unwind, early return).
    #[test]
    fn test_hub_drop_without_shutdown() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let done = std::sync::Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();

        let handle = std::thread::spawn(move || {
            let config = test_config();
            let mut hub = Hub::new(config).unwrap();

            let tx = hub.hub_event_tx.clone();
            hub.lua.set_hub_event_tx(tx, hub.tokio_runtime.handle().clone());

            // Drop WITHOUT calling shutdown() — Drop impl must handle it.
            drop(hub);

            done_clone.store(true, Ordering::SeqCst);
        });

        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(5) {
            if done.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        assert!(
            done.load(Ordering::SeqCst),
            "Hub::drop deadlocked without shutdown() — Drop impl did not stop watchers"
        );

        handle.join().expect("Hub drop thread should not panic");
    }

    #[test]
    fn test_hub_quit() {
        let config = test_config();
        let mut hub = Hub::new(config).unwrap();

        assert!(!hub.should_quit());
        hub.request_quit();
        assert!(hub.should_quit());
    }

    #[test]
    fn test_handle_action_quit() {
        let config = test_config();
        let mut hub = Hub::new(config).unwrap();

        hub.handle_action(HubAction::Quit);
        assert!(hub.should_quit());
    }

}

/// Write 1 byte to a wake pipe fd to unblock a `libc::poll()` waiter.
///
/// Pipe writes ≤ PIPE_BUF bytes are atomic per POSIX, so this is safe
/// to call from any thread (Hub main thread or tokio forwarder tasks).
pub(crate) fn wake_tui_pipe(fd: std::os::unix::io::RawFd) {
    unsafe {
        libc::write(fd, [1u8].as_ptr() as *const libc::c_void, 1);
    }
}
