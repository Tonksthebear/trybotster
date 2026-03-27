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
pub(crate) mod terminal_profile;

pub use actions::HubAction;
pub use agent_handle::{SessionHandle, SessionType};
pub use state::{HubState, SharedHubState};

use std::sync::{Arc, Mutex};
use std::time::Instant;

use reqwest::blocking::Client;

use sha2::{Digest, Sha256};

use crate::channel::Channel;
use crate::config::Config;
use crate::device::Device;
use crate::lua::primitives::SharedServerId;
use crate::lua::LuaRuntime;

const WEBRTC_PTY_OUTPUT_QUEUE_CAPACITY: usize = 2048;
const WEBRTC_OUTGOING_SIGNAL_QUEUE_CAPACITY: usize = 512;
const WEBRTC_STREAM_FRAME_QUEUE_CAPACITY: usize = 1024;
const WEBRTC_PTY_INPUT_QUEUE_CAPACITY: usize = 2048;
const WEBRTC_FILE_INPUT_QUEUE_CAPACITY: usize = 128;
const WORKTREE_RESULT_QUEUE_CAPACITY: usize = 256;

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
    /// Session UUID for hook context.
    pub session_uuid: String,
}

/// Pending terminal attach request across all client transports.
#[derive(Debug, Clone)]
pub(crate) enum PendingTerminalAttachRequest {
    WebRtc(crate::lua::primitives::CreateForwarderRequest),
    Tui(crate::lua::primitives::CreateTuiForwarderRequest),
    Socket(crate::lua::primitives::CreateSocketForwarderRequest),
}

impl PendingTerminalAttachRequest {
    #[must_use]
    pub(crate) fn session_uuid(&self) -> &str {
        match self {
            Self::WebRtc(req) => &req.session_uuid,
            Self::Tui(req) => &req.session_uuid,
            Self::Socket(req) => &req.session_uuid,
        }
    }

    #[must_use]
    pub(crate) fn is_active(&self) -> bool {
        let flag = match self {
            Self::WebRtc(req) => &req.active_flag,
            Self::Tui(req) => &req.active_flag,
            Self::Socket(req) => &req.active_flag,
        };
        *flag.lock().expect("Forwarder active_flag mutex poisoned")
    }

    pub(crate) fn deactivate(&self) {
        let flag = match self {
            Self::WebRtc(req) => &req.active_flag,
            Self::Tui(req) => &req.active_flag,
            Self::Socket(req) => &req.active_flag,
        };
        *flag.lock().expect("Forwarder active_flag mutex poisoned") = false;
    }
}

/// Pending terminal attach intent.
///
/// Created when a client subscribes to a terminal session before the session
/// is present in `HandleCache`. The Hub retries attach until either the session
/// appears (`attached`) or the intent expires (`not_found`).
#[derive(Debug, Clone)]
pub(crate) struct PendingTerminalAttach {
    /// Original forwarder request from Lua.
    pub request: PendingTerminalAttachRequest,
    /// Timestamp when the attach intent was first recorded.
    pub requested_at: Instant,
}

/// A PTY notification event queued by a watcher task for the Hub tick loop.
#[derive(Debug)]
pub struct PtyNotificationEvent {
    /// Session UUID for routing and Lua hook context.
    pub session_uuid: String,
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
            .field(
                "dead",
                &self.dead.load(std::sync::atomic::Ordering::Relaxed),
            )
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
/// Cooldown before sending a backpressure-recovery snapshot.
///
/// After the per-peer send channel drops PTY frames, we wait this long
/// for the burst to subside before sending a fresh snapshot. This avoids
/// sending large snapshots into a still-congested channel.
const BACKPRESSURE_SNAPSHOT_COOLDOWN: std::time::Duration = std::time::Duration::from_millis(500);

/// Result of attempting to queue a PTY frame for WebRTC delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebRtcSendOutcome {
    /// Frame queued successfully.
    Sent,
    /// Per-peer channel full — frame was dropped (peer is slow).
    Backpressure,
    /// Peer is dead or disconnected — no send task available.
    Dead,
}

/// Entry tracking a peer+session that needs a backpressure-recovery snapshot.
#[derive(Debug, Clone)]
struct BackpressureRecoveryEntry {
    browser_identity: String,
    session_uuid: String,
    subscription_id: String,
    last_drop: Instant,
}

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

/// Generate the stable local hub identifier for a device identity.
///
/// This is device-scoped, not repo-scoped. The fingerprint is already a stable
/// hash of the device verifying key, so we normalize it into a socket-safe ID.
#[must_use]
pub fn hub_id_for_device_fingerprint(fingerprint: &str) -> String {
    let normalized: String = fingerprint
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase();
    format!("device-{normalized}")
}

/// Generate the stable local hub identifier for a loaded device.
#[must_use]
pub fn hub_id_for_device(device: &Device) -> String {
    hub_id_for_device_fingerprint(&device.fingerprint)
}

/// Resolve the stable local hub identifier for the current device.
pub fn local_device_hub_id() -> anyhow::Result<String> {
    let device = Device::load_or_create()?;
    Ok(hub_id_for_device(&device))
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
    ///
    /// Wrapped in `Arc` so tests can share a single runtime across all
    /// `Hub` instances, preventing kqueue file-descriptor exhaustion on
    /// macOS (each `Runtime::new()` creates ~1 kqueue per worker thread).
    pub tokio_runtime: Arc<tokio::runtime::Runtime>,

    // === Control Flags ===
    /// Whether the hub should quit.
    pub quit: bool,
    /// Whether to exec-restart after shutdown (for self-update).
    pub exec_restart: bool,
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
    /// Connections that don't reach "Connected" within the bounded cleanup
    /// window are removed so retries do not require a manual refresh.
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

    /// Monotonic offer generation per browser identity.
    ///
    /// Each new WebRTC offer increments the browser's generation. Async
    /// `WebRtcOfferCompleted` events include the generation they started with,
    /// allowing the hub to discard stale completions instead of re-inserting
    /// outdated channels.
    webrtc_offer_generation: std::collections::HashMap<String, u64>,

    /// ICE candidates that arrived before the peer channel was re-attached.
    ///
    /// During async offer negotiation the channel is temporarily removed from
    /// `webrtc_channels`. Remote ICE can arrive in that window; queue it here
    /// and drain after `WebRtcOfferCompleted` to avoid dropping connectivity.
    webrtc_pending_ice_candidates: std::collections::HashMap<String, Vec<(u64, serde_json::Value)>>,

    /// Sender for PTY output messages from forwarder tasks.
    ///
    /// Forwarder tasks send PTY output here; main loop drains and sends via WebRTC.
    pub webrtc_pty_output_tx: tokio::sync::mpsc::Sender<WebRtcPtyOutput>,
    /// Receiver for PTY output messages.
    ///
    /// Wrapped in `Option` so the event loop can extract it for `tokio::select!`.
    pub webrtc_pty_output_rx: Option<tokio::sync::mpsc::Receiver<WebRtcPtyOutput>>,

    /// Active PTY forwarder task handles for cleanup on unsubscribe.
    ///
    /// Maps subscriptionId -> JoinHandle for the forwarder task.
    pty_forwarders: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,

    /// Pending backpressure-recovery snapshots.
    ///
    /// When PTY frames are dropped due to per-peer channel backpressure,
    /// we record the affected session here. After a cooldown, `tick_periodic`
    /// sends a fresh snapshot to resync the browser's terminal parser.
    /// Key: `{browser_identity}:{session_uuid}`.
    webrtc_backpressure_recovery: std::collections::HashMap<String, BackpressureRecoveryEntry>,
    /// Pending terminal attach intents waiting for session registration.
    ///
    /// Keyed by forwarder ID (`{peer_id}:{session_uuid}` / `tui:{session_uuid}` /
    /// `{client_id}:{session_uuid}`) so re-subscribe replaces stale intent
    /// atomically (idempotent reattach) across all transport clients.
    pending_terminal_attaches: std::collections::HashMap<String, PendingTerminalAttach>,

    /// Cached terminal theme replies learned from live attached clients.
    ///
    /// Used only as a headless fallback when a PTY emits startup probes before
    /// any terminal client is attached to answer them live.
    terminal_profiles: terminal_profile::TerminalProfileStore,
    /// Shared color cache from boot probe, shared with all `HubEventListener` instances.
    ///
    /// Populated once at startup. `HubEventListener` references this Arc so
    /// `ColorRequest` events are answered immediately from cached values.
    shared_color_cache: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<usize, crate::terminal::Rgb>>>,
    /// Focused terminal owner per session.
    ///
    /// Used to ensure OSC color queries are only forwarded to the active
    /// client terminal, avoiding duplicate auto-replies from passive clients.
    active_terminal_peers: Arc<Mutex<std::collections::HashMap<String, String>>>,

    /// Sender for outgoing WebRTC signals (ICE candidates) from async callbacks.
    ///
    /// Cloned for each new WebRTC channel. The async `on_ice_candidate` callback
    /// encrypts the candidate and sends it here. `poll_outgoing_signals()` drains
    /// the receiver and relays via `ChannelHandle::perform("signal", ...)`.
    pub webrtc_outgoing_signal_tx:
        tokio::sync::mpsc::Sender<crate::channel::webrtc::OutgoingSignal>,
    /// Receiver for outgoing WebRTC signals. Drained in `tick()`.
    ///
    /// Wrapped in `Option` so the event loop can extract it for `tokio::select!`.
    webrtc_outgoing_signal_rx:
        Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::OutgoingSignal>>,

    /// TCP stream multiplexers per browser identity for preview tunneling.
    stream_muxes: std::collections::HashMap<String, crate::relay::stream_mux::StreamMultiplexer>,
    /// Receiver for incoming stream frames from WebRTC DataChannels.
    ///
    /// Wrapped in `Option` so the event loop can extract it for `tokio::select!`.
    stream_frame_rx: Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::StreamIncoming>>,
    /// Sender for incoming stream frames (cloned into each WebRtcChannel).
    pub stream_frame_tx: tokio::sync::mpsc::Sender<crate::channel::webrtc::StreamIncoming>,
    /// Receiver for binary PTY input from WebRTC DataChannels (bypasses JSON/Lua).
    ///
    /// Wrapped in `Option` so the event loop can extract it for `tokio::select!`.
    pty_input_rx: Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::PtyInputIncoming>>,
    /// Sender for binary PTY input (cloned into each WebRtcChannel).
    pub pty_input_tx: tokio::sync::mpsc::Sender<crate::channel::webrtc::PtyInputIncoming>,
    /// Receiver for file transfers from browser via WebRTC DataChannels.
    ///
    /// Wrapped in `Option` so the event loop can extract it for `tokio::select!`.
    file_input_rx: Option<tokio::sync::mpsc::Receiver<crate::channel::webrtc::FileInputIncoming>>,
    /// Sender for file transfers (cloned into each WebRtcChannel).
    pub file_input_tx: tokio::sync::mpsc::Sender<crate::channel::webrtc::FileInputIncoming>,
    /// Temp files from browser paste/drop, keyed by agent session key.
    /// Cleaned up when the agent is closed.
    paste_files: std::collections::HashMap<String, Vec<std::path::PathBuf>>,

    // === Handle Cache ===
    /// Thread-safe cache of session handles for non-blocking client access.
    ///
    /// Updated by Lua via `hub.register_session()` and `hub.unregister_session()`.
    /// `HandleCache::get_session()` reads from this cache directly, allowing clients
    /// to access session handles without blocking commands - safe from any thread.
    pub handle_cache: Arc<handle_cache::HandleCache>,

    // === Lua Scripting ===
    /// Lua scripting runtime for hot-reloadable behavior customization.
    pub lua: LuaRuntime,

    // === Lua ActionCable ===
    /// Lua-managed ActionCable connections keyed by connection ID.
    lua_ac_connections:
        std::collections::HashMap<String, crate::lua::primitives::action_cable::LuaAcConnection>,
    /// Lua-managed ActionCable channel subscriptions keyed by channel ID.
    lua_ac_channels:
        std::collections::HashMap<String, crate::lua::primitives::action_cable::LuaAcChannel>,

    // === Lua Hub Client ===
    /// Lua-managed outgoing hub client connections keyed by connection ID.
    lua_hub_client_connections:
        std::collections::HashMap<String, crate::lua::primitives::hub_client::LuaHubClientConn>,

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

    /// Handles for notification watcher tasks, keyed by "{session_uuid}:{session_name}".
    notification_watcher_handles: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,

    /// Tracks peers that received a ratchet restart during the current cleanup window.
    /// Cleared every `CleanupTick` (5s) to coalesce decrypt failure storms.
    ratchet_restarted_peers: std::collections::HashSet<String>,

    // === Web Push Notifications ===
    /// VAPID keys for web push authentication (loaded on startup).
    pub(crate) vapid_keys: Option<crate::notifications::vapid::VapidKeys>,
    /// Browser push subscriptions (persisted to encrypted storage).
    pub(crate) push_subscriptions: crate::notifications::push::PushSubscriptionStore,

    // === Singleton Lock ===
    /// OS-level exclusive lock held for the hub's lifetime.
    ///
    /// Acquired before socket bind to prevent duplicate hubs for the same
    /// hub_id. Dropped on shutdown (RAII releases `flock`).
    singleton_lock: Option<daemon::HubLock>,

    // === Socket IPC ===
    /// Unix domain socket server for external client connections.
    socket_server: Option<crate::socket::server::SocketServer>,
    /// Connected socket clients, keyed by client_id.
    socket_clients: std::collections::HashMap<String, crate::socket::client_conn::SocketClientConn>,

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
    pub(crate) hub_event_tx: events::HubEventTx,
    /// Metrics for the unified Hub event bus (enqueue/dequeue/pending/high-water).
    pub(crate) hub_event_metrics: Arc<events::HubEventMetrics>,
    /// Last time hub event bus metrics were emitted to logs.
    pub(crate) hub_event_metrics_last_log: Instant,
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
        let runtime = Arc::new(tokio::runtime::Runtime::new()?);
        Self::with_runtime(config, runtime)
    }

    /// Create a Hub that shares an externally-owned tokio runtime.
    ///
    /// Used by tests to avoid creating one runtime per Hub instance (each
    /// runtime allocates ~1 kqueue FD per worker thread on macOS, which
    /// exhausts file descriptors when dozens of tests run in parallel).
    pub(crate) fn with_runtime(
        config: Config,
        tokio_runtime: Arc<tokio::runtime::Runtime>,
    ) -> anyhow::Result<Self> {
        use std::sync::RwLock;
        use std::time::Duration;

        let state = Arc::new(RwLock::new(HubState::new(config.worktree_base.clone())));

        // Load or create device identity before computing the local hub ID.
        // Device-scoped startup must never derive trust or identity from cwd.
        let device = Device::load_or_create()?;
        log::info!("Device fingerprint: {}", device.fingerprint);
        let hub_identifier = hub_id_for_device(&device);
        log::info!("Hub identifier (from device): {}...", &hub_identifier[..8]);

        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        // Create handle cache for thread-safe agent handle access
        let handle_cache = Arc::new(handle_cache::HandleCache::new());
        // Create channel for WebRTC PTY output from forwarder tasks
        let (webrtc_pty_output_tx, webrtc_pty_output_rx) =
            tokio::sync::mpsc::channel(WEBRTC_PTY_OUTPUT_QUEUE_CAPACITY);
        // Create channel for outgoing WebRTC signals (ICE candidates from async callbacks)
        let (webrtc_outgoing_signal_tx, webrtc_outgoing_signal_rx) =
            tokio::sync::mpsc::channel(WEBRTC_OUTGOING_SIGNAL_QUEUE_CAPACITY);
        // Create channel for incoming stream multiplexer frames from WebRTC DataChannels
        let (stream_frame_tx, stream_frame_rx) =
            tokio::sync::mpsc::channel(WEBRTC_STREAM_FRAME_QUEUE_CAPACITY);
        // Create channel for binary PTY input from WebRTC DataChannels
        let (pty_input_tx, pty_input_rx) =
            tokio::sync::mpsc::channel(WEBRTC_PTY_INPUT_QUEUE_CAPACITY);
        // Create channel for file transfers from browser via WebRTC DataChannels
        let (file_input_tx, file_input_rx) =
            tokio::sync::mpsc::channel(WEBRTC_FILE_INPUT_QUEUE_CAPACITY);
        // Create channel for async worktree creation results
        let (worktree_result_tx, worktree_result_rx) =
            tokio::sync::mpsc::channel(WORKTREE_RESULT_QUEUE_CAPACITY);
        // Unified event bus for background producers (HTTP, WS, timers, etc.)
        let (hub_event_raw_tx, hub_event_rx) = tokio::sync::mpsc::unbounded_channel();
        let hub_event_metrics = Arc::new(events::HubEventMetrics::default());
        let hub_event_tx =
            events::HubEventTx::new(hub_event_raw_tx, Arc::clone(&hub_event_metrics));

        // Initialize Lua scripting runtime
        let mut lua = LuaRuntime::new()?;

        // Wire the unified event bus into Lua primitive registries so background
        // threads can send events directly instead of pushing to shared vecs.
        lua.set_hub_event_tx(hub_event_tx.clone(), tokio_runtime.handle().clone());

        let mut hub = Self {
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
            browser: crate::relay::BrowserState::default(),
            handle_cache,
            webrtc_channels: std::collections::HashMap::new(),
            webrtc_connection_started: std::collections::HashMap::new(),
            webrtc_send_tasks: std::collections::HashMap::new(),
            dc_ping_tasks: std::collections::HashMap::new(),
            webrtc_pending_closes: std::collections::HashMap::new(),
            webrtc_offer_generation: std::collections::HashMap::new(),
            webrtc_pending_ice_candidates: std::collections::HashMap::new(),
            webrtc_pty_output_tx,
            webrtc_pty_output_rx: Some(webrtc_pty_output_rx),
            pty_forwarders: std::collections::HashMap::new(),
            webrtc_backpressure_recovery: std::collections::HashMap::new(),
            pending_terminal_attaches: std::collections::HashMap::new(),
            terminal_profiles: {
                let mut store = terminal_profile::TerminalProfileStore::default();
                store.probe_spawning_terminal();
                store
            },
            // Populated below after terminal_profiles is available.
            shared_color_cache: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            active_terminal_peers: Arc::new(Mutex::new(std::collections::HashMap::new())),
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
            #[cfg(test)]
            pty_notification_queue: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            pty_output_messages_drained: 0,
            notification_watcher_handles: std::collections::HashMap::new(),
            ratchet_restarted_peers: std::collections::HashSet::new(),
            vapid_keys: None,
            push_subscriptions: crate::notifications::push::PushSubscriptionStore::default(),
            singleton_lock: None,
            socket_server: None,
            socket_clients: std::collections::HashMap::new(),
            tui_output_tx: None,
            tui_wake_fd: None,
            tui_request_rx: None,
            worktree_result_tx,
            worktree_result_rx: Some(worktree_result_rx),
            hub_event_tx,
            hub_event_metrics,
            hub_event_metrics_last_log: Instant::now(),
            hub_event_rx: Some(hub_event_rx),
        };

        // Populate shared color cache from boot probe results.
        hub.shared_color_cache = hub.terminal_profiles.shared_color_cache();
        if let Ok(cache) = hub.shared_color_cache.lock() {
            log::info!(
                "[PTY-PROBE] Shared color cache populated with {} entries",
                cache.len()
            );
        }

        Ok(hub)
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
        let offline = crate::env::is_offline();

        if !crate::env::is_test_mode() && !offline {
            self.register_hub_with_server();
        }

        if !offline {
            self.init_crypto_service();
            self.init_web_push();
        } else {
            log::info!("Offline mode: skipping crypto service and web push initialization");
        }

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
            Arc::clone(&self.shared_color_cache),
        ) {
            log::warn!("Failed to register Hub Lua primitives: {}", e);
        }

        // Load Lua init script (hot-reload is now handled by Lua's module_watcher)
        self.load_lua_init();
        self.fire_hub_recovery_state("starting", serde_json::json!({}));

        // Bundle generation is deferred - don't call generate_connection_url() here.
        // The bundle will be generated lazily when:
        // 1. TUI requests QR code display (GetConnectionCode command)
        // 2. External automation requests the connection URL
        // 3. Headless mode calls setup_headless() which eagerly generates it
        // This avoids blocking boot for up to 10 seconds in TUI mode.
    }

    /// Emit a startup/recovery lifecycle transition for hub clients.
    ///
    /// Lua `handlers/connections.lua` persists and broadcasts this payload to
    /// all hub subscribers as `hub_recovery_state`.
    fn fire_hub_recovery_state(&self, state: &str, mut payload: serde_json::Value) {
        if !payload.is_object() {
            payload = serde_json::json!({});
        }
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "state".to_string(),
                serde_json::Value::String(state.to_string()),
            );
            obj.insert(
                "hub_id".to_string(),
                serde_json::Value::String(self.server_hub_id().to_string()),
            );
        }
        if let Err(e) = self.lua.fire_json_event("hub_recovery_state", &payload) {
            log::warn!("[hub] hub_recovery_state({state}) event error: {e}");
        }
    }

    /// Discover live session process sockets and fire Lua recovery event.
    ///
    /// Scans the session socket directory for `.sock` files. For each one,
    /// attempts a connect + handshake to verify liveness and extract metadata.
    /// Fires `sessions_discovered` with the list of live sessions.
    fn recover_session_processes(&mut self) -> usize {
        let sockets = match crate::session::discover_sessions() {
            Ok(s) => s,
            Err(e) => {
                log::warn!("[session] recovery scan failed: {e}");
                return 0;
            }
        };

        if sockets.is_empty() {
            log::debug!("[session] no session sockets found");
            return 0;
        }

        log::info!("[session] found {} session socket(s)", sockets.len());

        // Don't connect during scan — just list socket files and extract
        // session_uuid from filenames. Lua connects once via hub.connect_session.
        // Connecting here and dropping would force the session process into
        // reconnect mode, racing with Lua's subsequent connect.
        let mut discovered = Vec::new();

        for socket_path in &sockets {
            let session_uuid = match socket_path.file_stem().and_then(|s| s.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };

            log::info!(
                "[session] discovered socket: {}",
                &session_uuid[..session_uuid.len().min(16)]
            );
            discovered.push(serde_json::json!({
                "session_uuid": session_uuid,
                "socket_path": socket_path.display().to_string(),
            }));
        }

        let count = discovered.len();

        if let Err(e) = self.lua.fire_json_event(
            "sessions_discovered",
            &serde_json::json!({ "sockets": discovered }),
        ) {
            log::warn!("[session] sessions_discovered event error: {e}");
        }

        count
    }

    /// Start the Unix domain socket server for IPC.
    ///
    /// Creates the socket at `/tmp/botster-{uid}/{hub_id}.sock`,
    /// writes a PID file, and begins accepting client connections.
    /// Socket events are delivered via `HubEvent` variants.
    pub fn start_socket_server(&mut self) -> anyhow::Result<()> {
        let _guard = self.tokio_runtime.enter();

        // Acquire exclusive OS lock BEFORE any socket/PID operations.
        // This is the atomic singleton gate — prevents TOCTOU races between
        // the PID check in main.rs and socket bind below.
        let lock = daemon::try_lock_hub(&self.hub_identifier)?;
        self.singleton_lock = Some(lock);

        // Clean up stale files from previous runs
        daemon::cleanup_stale_files(&self.hub_identifier);

        // Sweep orphaned sockets left by crashed/killed processes
        daemon::cleanup_orphaned_sockets();

        let path = daemon::socket_path(&self.hub_identifier)?;
        let socket_path = path.display().to_string();
        let server = crate::socket::server::SocketServer::start(path, self.hub_event_tx.clone())?;
        log::info!(
            "Socket server started for hub {}",
            &self.hub_identifier[..self.hub_identifier.len().min(8)]
        );
        self.socket_server = Some(server);

        // Persist ownership metadata after a successful bind so failed startup
        // attempts never steal pid/manifest ownership from a live hub.
        if let Err(e) = daemon::write_pid_file(&self.hub_identifier) {
            log::warn!("Failed to write PID file: {e}");
        }
        if let Err(e) = daemon::write_manifest(&self.hub_identifier, self.botster_id.as_deref()) {
            log::warn!("Failed to write hub manifest: {e}");
        }

        self.fire_hub_recovery_state(
            "socket_ready",
            serde_json::json!({ "socket_path": socket_path }),
        );

        // Recover sessions from per-session processes
        let session_count = if !crate::env::is_test_mode() {
            self.recover_session_processes()
        } else {
            0
        };

        self.fire_hub_recovery_state(
            "sessions_recovered",
            serde_json::json!({
                "count": session_count,
                "inventory_authority": true,
            }),
        );

        self.fire_hub_recovery_state("ready", serde_json::json!({}));
        Ok(())
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
                if let Err(e) = self
                    .lua
                    .lua()
                    .globals()
                    .set("_lua_base_path", dev_lua_dir.to_string_lossy().to_string())
                {
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
        // Disconnect all socket clients
        for (client_id, conn) in self.socket_clients.drain() {
            log::debug!("Disconnecting socket client: {}", client_id);
            conn.disconnect();
        }
        // Shutdown socket server
        if let Some(server) = self.socket_server.take() {
            server.shutdown();
        }
        // Release singleton lock (flock released on fd close)
        if let Some(lock) = self.singleton_lock.take() {
            log::info!("Released singleton lock: {}", lock.path.display());
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
        for (_key, intent) in self.pending_terminal_attaches.drain() {
            intent.request.deactivate();
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
        self.webrtc_pending_ice_candidates.clear();

        // Notify server of shutdown (skip in offline mode)
        if !crate::env::is_offline() {
            registration::shutdown(
                &self.client,
                &self.config.server_url,
                self.server_hub_id(),
                self.config.get_api_key(),
            );
        }
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
        if crate::env::is_offline() {
            return Err("Connection URL unavailable in offline mode".to_string());
        }
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

    /// Single shared tokio runtime for all Hub tests.
    ///
    /// Each `Runtime::new()` allocates ~1 kqueue FD per worker thread on
    /// macOS. With dozens of tests each creating a Hub (and thus a runtime),
    /// the process quickly exhausts file descriptors. Sharing one runtime
    /// eliminates the leak while still allowing parallel test execution
    /// (tokio runtimes are thread-safe by design).
    fn shared_test_runtime() -> Arc<tokio::runtime::Runtime> {
        use std::sync::OnceLock;
        static RT: OnceLock<Arc<tokio::runtime::Runtime>> = OnceLock::new();
        Arc::clone(RT.get_or_init(|| Arc::new(tokio::runtime::Runtime::new().unwrap())))
    }

    #[test]
    fn test_hub_id_for_device_fingerprint_normalizes_to_device_scoped_id() {
        assert_eq!(
            hub_id_for_device_fingerprint("AA:bb:11:22:CC:dd:33:44"),
            "device-aabb1122ccdd3344"
        );
    }

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
        let hub = Hub::with_runtime(config, shared_test_runtime()).unwrap();

        assert!(!hub.should_quit());
    }

    /// Offline mode: setup() skips registration and crypto without panicking.
    ///
    /// Verifies that `Hub::setup()` completes successfully when
    /// `BOTSTER_OFFLINE=1`, even though no server is reachable.
    ///
    /// Runs single-threaded to prevent env var races with other tests.
    #[test]
    #[ignore = "env-var-mutating — run with: cargo test -- --ignored --test-threads=1 test_hub_setup_offline"]
    fn test_hub_setup_offline_skips_registration() {
        std::env::set_var("BOTSTER_OFFLINE", "1");

        let config = test_config();
        let mut hub = Hub::with_runtime(config, shared_test_runtime()).unwrap();
        hub.setup();

        // Server registration was skipped — botster_id should be None
        assert!(
            hub.botster_id.is_none(),
            "botster_id should be None in offline mode"
        );
        // Crypto service was skipped
        assert!(
            hub.browser.crypto_service.is_none(),
            "crypto_service should be None in offline mode"
        );
        // Connection URL should return an error, not panic
        let url_result = hub.generate_connection_url();
        assert!(
            url_result.is_err(),
            "generate_connection_url should return Err in offline mode"
        );

        std::env::remove_var("BOTSTER_OFFLINE");
    }

    /// Offline mode: generate_connection_url returns Err without panicking.
    ///
    /// This test does NOT require env var mutation — it tests the guard
    /// indirectly by verifying the crypto_service=None path.
    #[test]
    fn test_generate_connection_url_without_crypto_returns_err() {
        let config = test_config();
        let mut hub = Hub::with_runtime(config, shared_test_runtime()).unwrap();
        // Don't call setup() — crypto_service stays None
        assert!(hub.browser.crypto_service.is_none());
        // The non-offline path should also fail gracefully (no panic)
        // when crypto isn't initialized
        let result = hub.get_or_generate_connection_url();
        assert!(
            result.is_err(),
            "connection URL without crypto should fail gracefully"
        );
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
    ///
    /// NOTE: This test intentionally uses a dedicated runtime (not the shared
    /// test runtime) so the runtime actually drops when the Hub drops,
    /// exercising the real drop-order deadlock scenario.
    #[test]
    fn test_hub_drop_completes_with_shutdown() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let done = std::sync::Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();

        let handle = std::thread::spawn(move || {
            let config = test_config();
            let dedicated_rt = Arc::new(tokio::runtime::Runtime::new().unwrap());
            let mut hub = Hub::with_runtime(config, dedicated_rt).unwrap();

            let tx = hub.hub_event_tx.clone();
            hub.lua
                .set_hub_event_tx(tx, hub.tokio_runtime.handle().clone());

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
    ///
    /// NOTE: Dedicated runtime — same rationale as `test_hub_drop_completes_with_shutdown`.
    #[test]
    fn test_hub_drop_without_shutdown() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let done = std::sync::Arc::new(AtomicBool::new(false));
        let done_clone = done.clone();

        let handle = std::thread::spawn(move || {
            let config = test_config();
            let dedicated_rt = Arc::new(tokio::runtime::Runtime::new().unwrap());
            let mut hub = Hub::with_runtime(config, dedicated_rt).unwrap();

            let tx = hub.hub_event_tx.clone();
            hub.lua
                .set_hub_event_tx(tx, hub.tokio_runtime.handle().clone());

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
        let mut hub = Hub::with_runtime(config, shared_test_runtime()).unwrap();

        assert!(!hub.should_quit());
        hub.request_quit();
        assert!(hub.should_quit());
    }

    #[test]
    fn test_handle_action_quit() {
        let config = test_config();
        let mut hub = Hub::with_runtime(config, shared_test_runtime()).unwrap();

        hub.handle_action(HubAction::Quit);
        assert!(hub.should_quit());
    }

    /// Full singleton lifecycle: start → duplicate blocked → stop → restart succeeds.
    ///
    /// Exercises the actual `start_socket_server` path (lock + socket bind + PID write),
    /// not just the low-level `try_lock_hub` primitive.
    #[test]
    fn test_singleton_lock_blocks_duplicate_hub_then_allows_reboot() {
        let test_hub_id = format!("_test_singleton_reboot_{}", std::process::id());

        // --- Hub A: starts successfully ---
        let mut hub_a = Hub::with_runtime(test_config(), shared_test_runtime()).unwrap();
        hub_a.hub_identifier = test_hub_id.clone();
        hub_a
            .start_socket_server()
            .expect("Hub A should start successfully");

        // Verify lock and socket exist
        let lock_path = daemon::lock_file_path(&test_hub_id).unwrap();
        assert!(
            lock_path.exists(),
            "lock file should exist while hub is running"
        );
        let sock_path = daemon::socket_path(&test_hub_id).unwrap();
        assert!(
            sock_path.exists(),
            "socket should exist while hub is running"
        );

        // --- Hub B: blocked by singleton lock ---
        let mut hub_b = Hub::with_runtime(test_config(), shared_test_runtime()).unwrap();
        hub_b.hub_identifier = test_hub_id.clone();
        let err = hub_b
            .start_socket_server()
            .expect_err("Hub B must fail while Hub A holds the lock");
        assert!(
            err.to_string().contains("Another hub is already running"),
            "expected singleton error, got: {err}"
        );

        // --- Hub A shuts down ---
        hub_a.shutdown();
        drop(hub_a);

        // Socket should be cleaned up after shutdown
        assert!(
            !sock_path.exists(),
            "socket should be cleaned up after shutdown"
        );

        // --- Hub C: reboot succeeds after A released the lock ---
        let mut hub_c = Hub::with_runtime(test_config(), shared_test_runtime()).unwrap();
        hub_c.hub_identifier = test_hub_id.clone();
        hub_c
            .start_socket_server()
            .expect("Hub C should start after Hub A released the lock");

        // Clean up
        hub_c.shutdown();
        drop(hub_c);
        let _ = std::fs::remove_file(lock_path);
        let _ = std::fs::remove_dir(daemon::hub_dir(&test_hub_id).unwrap());
    }

    /// Full hub reboot cycle with real socket I/O and bidirectional PTY data.
    ///
    /// Proves the complete data path survives a hub restart:
    /// 1. Hub boots with real socket server, full Lua handlers
    /// 2. Real socket client connects, subscribes (hub + terminal channels)
    /// 3. PTY input sent via socket → routed to session handle
    /// 4. PTY output injected via reader thread → forwarder → client reads Frame::PtyOutput
    /// 5. Hub shuts down (simulates reboot)
    /// 6. New hub boots, new client connects, repeat — both directions work
    ///
    /// Agent creation is not exercised in this test. Sessions are registered
    /// directly in handle_cache; output path is still a real PTY round-trip
    /// across hub reboot.
    ///
    /// This test uses NO mocks:
    /// - Real bash process spawned via PtySession::spawn()
    /// - Real master FD reader thread inside the PTY session
    /// - Real socket client (same protocol the TUI uses)
    /// - Real Lua handler processing for subscriptions
    ///
    /// Flow per phase:
    ///   1. Spawn bash → reader thread reads master FD → shadow_screen + broadcast
    ///   2. Socket client subscribes to terminal channel
    ///   3. Send "echo MARKER\n" via PtyInput frame → hub routes → bash
    ///   4. bash echoes MARKER → reader thread → broadcast → forwarder → PtyOutput frame
    ///   5. Client reads PtyOutput, asserts MARKER present in output
    ///   6. Hub shuts down, bash killed, reader exits
    ///   7. New hub boots, repeat with fresh bash
    #[test]
    fn test_hub_reboot_cycle_with_socket_pty_roundtrip() {
        use crate::agent::pty::{PtySession, PtySpawnConfig};
        use crate::hub::agent_handle::{PtyHandle, SessionHandle, SessionType};
        use crate::relay::create_crypto_service;
        use crate::socket::framing::{Frame, FrameDecoder};
        use std::collections::HashMap;
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicBool, Ordering};

        let test_hub_id = format!("_test_reboot_pty_{}", std::process::id());

        // --- Helpers ---

        fn init_hub_with_lua(hub: &mut Hub) {
            let crypto_service = create_crypto_service("test-reboot");
            hub.browser.crypto_service = Some(crypto_service);
            hub.lua
                .register_hub_primitives(
                    Arc::clone(&hub.handle_cache),
                    hub.config.worktree_base.clone(),
                    hub.hub_identifier.clone(),
                    Arc::clone(&hub.shared_server_id),
                    Arc::clone(&hub.state),
                    Arc::clone(&hub.shared_color_cache),
                )
                .expect("register hub primitives");
            hub.load_lua_init();
        }

        /// Spawn a real bash process and return:
        /// - SessionHandle with real PTY writer
        /// - PtySession (must stay alive to keep child process running)
        /// - Stop flag for the reader thread
        fn spawn_real_session(
            uuid: &str,
            pty_handle_ref: &Arc<std::sync::Mutex<Option<PtyHandle>>>,
        ) -> (SessionHandle, PtySession, Arc<AtomicBool>) {
            let mut pty_session = PtySession::new(24, 80);
            let tmpdir = std::env::temp_dir();
            pty_session
                .spawn(PtySpawnConfig {
                    worktree_path: tmpdir,
                    command: "bash --norc --noprofile".to_string(),
                    env: {
                        let mut env = HashMap::new();
                        env.insert("PS1".to_string(), "$ ".to_string());
                        env.insert("TERM".to_string(), "dumb".to_string());
                        env
                    },
                    init_commands: vec![],
                    detect_notifications: false,
                    port: None,
                    context: String::new(),
                })
                .expect("spawn bash");

            let (
                shared_state,
                shadow_screen,
                event_tx,
                kitty_enabled,
                cursor_visible,
                resize_pending,
            ) = pty_session.get_direct_access();

            let pty = PtyHandle::new(
                event_tx,
                Arc::clone(&shared_state),
                shadow_screen,
                kitty_enabled,
                cursor_visible,
                resize_pending,
                None,
            );

            // Store the pty handle for the reader thread to use
            *pty_handle_ref.lock().unwrap() = Some(pty.clone());

            // Start a reader thread that reads from the master FD, feeds the
            // shadow screen, and broadcasts output — same as session reader.
            let stop_flag = Arc::new(AtomicBool::new(false));
            let stop_clone = Arc::clone(&stop_flag);
            let reader_shadow = pty.shadow_screen().expect("test PTY has shadow screen");
            let reader_event_tx = pty.event_tx_clone();

            // Get a reader from the master PTY FD
            let reader = {
                let state = shared_state.lock().expect("shared_state lock");
                state
                    .master_pty
                    .as_ref()
                    .expect("master_pty should exist after spawn")
                    .try_clone_reader()
                    .expect("try_clone_reader")
            };

            std::thread::spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 4096];
                loop {
                    if stop_clone.load(Ordering::Relaxed) {
                        break;
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            // Feed shadow screen for snapshot support
                            if let Ok(mut screen) = reader_shadow.lock() {
                                screen.process(&buf[..n]);
                            }
                            // Broadcast output to subscribers
                            let _ = reader_event_tx.send(
                                crate::agent::pty::events::PtyEvent::output(buf[..n].to_vec()),
                            );
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break, // EIO = child exited
                    }
                }
            });

            let session = SessionHandle::new(uuid, "test-real-bash", SessionType::Agent, None, pty);
            (session, pty_session, stop_flag)
        }

        /// Drain pending hub events and dispatch them.
        fn drain_hub_events(hub: &mut Hub) {
            let mut rx = hub.hub_event_rx.take();
            if let Some(ref mut rx) = rx {
                while let Ok(event) = rx.try_recv() {
                    hub.handle_hub_event(event);
                }
            }
            hub.hub_event_rx = rx;
        }

        /// Give async tasks time to fire events, then drain.
        fn settle(hub: &mut Hub, ms: u64) {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            drain_hub_events(hub);
        }

        /// Read all available frames from a socket with a read timeout.
        fn read_frames(stream: &mut std::os::unix::net::UnixStream) -> Vec<Frame> {
            let mut buf = [0u8; 16384];
            let mut decoder = FrameDecoder::new();
            let mut all_frames = Vec::new();

            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(frames) = decoder.feed(&buf[..n]) {
                            all_frames.extend(frames);
                        }
                    }
                    Err(ref e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        break;
                    }
                    Err(_) => break,
                }
            }
            all_frames
        }

        /// Wait until PtyOutput frames contain the expected marker string.
        /// Retries up to `max_attempts` times with `interval_ms` between.
        fn wait_for_output_containing(
            stream: &mut std::os::unix::net::UnixStream,
            hub: &mut Hub,
            marker: &str,
            max_attempts: usize,
            interval_ms: u64,
        ) -> bool {
            for _ in 0..max_attempts {
                settle(hub, interval_ms);
                let frames = read_frames(stream);
                for frame in &frames {
                    if let Frame::PtyOutput { data, .. } = frame {
                        let text = String::from_utf8_lossy(data);
                        if text.contains(marker) {
                            return true;
                        }
                    }
                }
            }
            false
        }

        /// Wait for the terminal scrollback frame for `session_uuid`.
        fn wait_for_scrollback(
            stream: &mut std::os::unix::net::UnixStream,
            hub: &mut Hub,
            session_uuid: &str,
            max_attempts: usize,
            interval_ms: u64,
        ) -> Option<Frame> {
            for _ in 0..max_attempts {
                settle(hub, interval_ms);
                let frames = read_frames(stream);
                for frame in frames {
                    if let Frame::Scrollback {
                        session_uuid: got, ..
                    } = &frame
                    {
                        if got == session_uuid {
                            return Some(frame);
                        }
                    }
                }
            }
            None
        }

        // ============================================================
        // Phase 1: Hub A — real bash, real I/O
        // ============================================================

        let mut hub_a = Hub::with_runtime(test_config(), shared_test_runtime()).unwrap();
        hub_a.hub_identifier = test_hub_id.clone();
        init_hub_with_lua(&mut hub_a);
        hub_a
            .start_socket_server()
            .expect("Hub A should start socket server");

        let sock_path = daemon::socket_path(&test_hub_id).unwrap();

        let pty_handle_a = Arc::new(std::sync::Mutex::new(None));
        let (session_a, _pty_session_a, stop_a) =
            spawn_real_session("sess-reboot-001", &pty_handle_a);
        hub_a.handle_cache.add_session(session_a);

        // Wait for bash to start and produce its initial prompt
        settle(&mut hub_a, 300);

        // Connect socket client
        let mut client_a =
            std::os::unix::net::UnixStream::connect(&sock_path).expect("[Phase 1] connect failed");
        client_a
            .set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        settle(&mut hub_a, 100);

        assert!(
            !hub_a.socket_clients.is_empty(),
            "[Phase 1] no socket client after connect"
        );

        // Subscribe to hub channel
        client_a
            .write_all(
                &Frame::Json(serde_json::json!({
                    "type": "subscribe",
                    "channel": "hub",
                    "subscriptionId": "tui_hub"
                }))
                .encode(),
            )
            .unwrap();
        settle(&mut hub_a, 100);

        // Subscribe to terminal channel — Lua creates the socket PTY forwarder
        client_a
            .write_all(
                &Frame::Json(serde_json::json!({
                    "type": "subscribe",
                    "channel": "terminal",
                    "subscriptionId": "tui:sess-reboot-001",
                    "params": {
                        "session_uuid": "sess-reboot-001",
                        "rows": 24,
                        "cols": 80
                    }
                }))
                .encode(),
            )
            .unwrap();
        settle(&mut hub_a, 200);

        let scrollback_a =
            wait_for_scrollback(&mut client_a, &mut hub_a, "sess-reboot-001", 20, 100)
                .expect("[Phase 1] expected terminal scrollback after subscribe");
        match scrollback_a {
            Frame::Scrollback {
                rows, cols, data, ..
            } => {
                assert_eq!(
                    (rows, cols),
                    (24, 80),
                    "[Phase 1] unexpected scrollback dims"
                );
                assert!(
                    !data.is_empty(),
                    "[Phase 1] scrollback should not be empty after bash startup"
                );
            }
            other => panic!("[Phase 1] expected Scrollback frame, got {other:?}"),
        }

        // Send real input to bash: echo a unique marker
        let marker_a = format!("REBOOT_TEST_{}", std::process::id());
        client_a
            .write_all(
                &Frame::PtyInput {
                    session_uuid: "sess-reboot-001".to_string(),
                    data: format!("echo {marker_a}\n").into_bytes(),
                }
                .encode(),
            )
            .unwrap();

        // Wait for bash to echo our marker back through the full pipeline:
        // bash output → master FD → reader thread → broadcast →
        // PtyEvent::Output broadcast → socket forwarder → Frame::PtyOutput → client
        let found_a = wait_for_output_containing(
            &mut client_a,
            &mut hub_a,
            &marker_a,
            20,  // up to 20 attempts
            100, // 100ms between
        );
        assert!(
            found_a,
            "[Phase 1] never received marker '{marker_a}' in PtyOutput frames \
             — real bash echo did not round-trip through hub"
        );

        // Verify input was registered on the PTY handle
        let session_handle_a = hub_a
            .handle_cache
            .get_session("sess-reboot-001")
            .expect("[Phase 1] session not in handle_cache");
        assert!(
            session_handle_a.pty().last_human_input_ms() > 0,
            "[Phase 1] last_human_input_ms should be > 0 after real input"
        );

        // ============================================================
        // Phase 2: Hub A shuts down (reboot)
        // ============================================================

        drop(client_a);
        stop_a.store(true, Ordering::Relaxed);
        drop(session_handle_a);
        hub_a.shutdown();
        drop(hub_a);

        assert!(
            !sock_path.exists(),
            "Socket should be cleaned up after shutdown"
        );

        // ============================================================
        // Phase 3: Hub B boots — fresh bash, same flow
        // ============================================================

        let mut hub_b = Hub::with_runtime(test_config(), shared_test_runtime()).unwrap();
        hub_b.hub_identifier = test_hub_id.clone();
        init_hub_with_lua(&mut hub_b);
        hub_b
            .start_socket_server()
            .expect("Hub B should start after reboot");

        let pty_handle_b = Arc::new(std::sync::Mutex::new(None));
        let (session_b, _pty_session_b, stop_b) =
            spawn_real_session("sess-reboot-002", &pty_handle_b);
        hub_b.handle_cache.add_session(session_b);

        settle(&mut hub_b, 300);

        let mut client_b =
            std::os::unix::net::UnixStream::connect(&sock_path).expect("[Phase 2] connect failed");
        client_b
            .set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        settle(&mut hub_b, 100);

        assert!(
            !hub_b.socket_clients.is_empty(),
            "[Phase 2] no socket client after connect"
        );

        client_b
            .write_all(
                &Frame::Json(serde_json::json!({
                    "type": "subscribe",
                    "channel": "hub",
                    "subscriptionId": "tui_hub"
                }))
                .encode(),
            )
            .unwrap();
        settle(&mut hub_b, 100);

        client_b
            .write_all(
                &Frame::Json(serde_json::json!({
                    "type": "subscribe",
                    "channel": "terminal",
                    "subscriptionId": "tui:sess-reboot-002",
                    "params": {
                        "session_uuid": "sess-reboot-002",
                        "rows": 24,
                        "cols": 80
                    }
                }))
                .encode(),
            )
            .unwrap();
        settle(&mut hub_b, 200);

        let scrollback_b =
            wait_for_scrollback(&mut client_b, &mut hub_b, "sess-reboot-002", 20, 100)
                .expect("[Phase 2] expected terminal scrollback after subscribe");
        match scrollback_b {
            Frame::Scrollback {
                rows, cols, data, ..
            } => {
                assert_eq!(
                    (rows, cols),
                    (24, 80),
                    "[Phase 2] unexpected scrollback dims"
                );
                assert!(
                    !data.is_empty(),
                    "[Phase 2] scrollback should not be empty after bash startup"
                );
            }
            other => panic!("[Phase 2] expected Scrollback frame, got {other:?}"),
        }

        let marker_b = format!("REBOOT_PHASE2_{}", std::process::id());
        client_b
            .write_all(
                &Frame::PtyInput {
                    session_uuid: "sess-reboot-002".to_string(),
                    data: format!("echo {marker_b}\n").into_bytes(),
                }
                .encode(),
            )
            .unwrap();

        let found_b = wait_for_output_containing(&mut client_b, &mut hub_b, &marker_b, 20, 100);
        assert!(
            found_b,
            "[Phase 2] never received marker '{marker_b}' in PtyOutput frames \
             — real bash echo did not round-trip through hub after reboot"
        );

        let session_handle_b = hub_b
            .handle_cache
            .get_session("sess-reboot-002")
            .expect("[Phase 2] session not in handle_cache");
        assert!(
            session_handle_b.pty().last_human_input_ms() > 0,
            "[Phase 2] last_human_input_ms should be > 0 after real input"
        );

        // ============================================================
        // Cleanup
        // ============================================================

        drop(client_b);
        stop_b.store(true, Ordering::Relaxed);
        drop(session_handle_b);
        hub_b.shutdown();
        drop(hub_b);
        let _ = std::fs::remove_file(daemon::lock_file_path(&test_hub_id).unwrap());
        let _ = std::fs::remove_dir(daemon::hub_dir(&test_hub_id).unwrap());
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
