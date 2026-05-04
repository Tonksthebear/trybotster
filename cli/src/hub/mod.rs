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
use std::time::{Duration, Instant};

use reqwest::blocking::Client;

use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::device::Device;
use crate::lua::primitives::SharedServerId;
use crate::lua::LuaRuntime;

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

#[derive(Debug, Default)]
pub(crate) struct VolumeBurstState {
    entries: std::collections::VecDeque<(&'static str, Instant)>,
    warned: std::collections::HashSet<&'static str>,
}

impl VolumeBurstState {
    pub(crate) const WINDOW: Duration = Duration::from_secs(30);
    pub(crate) const THRESHOLD: usize = 1000;

    pub(crate) fn record(&mut self, name: &'static str, now: Instant) -> Option<usize> {
        while self
            .entries
            .front()
            .is_some_and(|(_, at)| now.duration_since(*at) > Self::WINDOW)
        {
            if let Some((old_name, _)) = self.entries.pop_front() {
                if !self.entries.iter().any(|(n, _)| *n == old_name) {
                    self.warned.remove(old_name);
                }
            }
        }
        self.entries.push_back((name, now));
        let count = self.entries.iter().filter(|(n, _)| *n == name).count();
        if count > Self::THRESHOLD && self.warned.insert(name) {
            Some(count)
        } else {
            None
        }
    }
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

/// State for a session awaiting background reconnect.
pub(crate) struct ReconnectState {
    /// When the reconnect was first requested.
    pub started_at: std::time::Instant,
    /// When the current in-flight attempt was launched (if any).
    pub attempt_started_at: Option<std::time::Instant>,
    /// Generation counter to detect stale completions from background tasks.
    pub generation: u64,
    /// Whether a background reconnect task is currently in flight.
    pub in_flight: bool,
}

pub(crate) const SESSION_IO_SNAPSHOT_PENDING_TTL: std::time::Duration =
    std::time::Duration::from_secs(30);

pub(crate) struct PendingSessionIoSnapshot {
    pub session_uuid: String,
    pub started_at: std::time::Instant,
    pub target: PendingSessionIoSnapshotTarget,
}

pub(crate) enum PendingSessionIoSnapshotTarget {
    WebRtcOutput {
        peer_id: String,
        rows: u16,
        cols: u16,
        kitty_enabled: bool,
        forwarder_key: Option<String>,
        active_flag: Option<std::sync::Arc<std::sync::Mutex<bool>>>,
    },
    WebRtcPeerRecovery {
        request: crate::worker::webrtc::WebRtcRecoverySnapshotRequest,
    },
}

/// Central orchestrator that owns all hub state and runs the event loop.
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

    /// Adapter-owned WebRTC peer registry.
    pub(crate) webrtc: crate::worker::webrtc::WebRtcPeerRegistry,
    /// Bounded OSC/timer volume guardrail state.
    volume_bursts: std::sync::Mutex<VolumeBurstState>,

    /// Active PTY forwarder task handles for cleanup on unsubscribe.
    ///
    /// Maps subscriptionId -> JoinHandle for the forwarder task.
    pty_forwarders: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,

    /// Pending terminal attach intents waiting for session registration.
    ///
    /// Keyed by forwarder ID (`{peer_id}:{session_uuid}` / `tui:{session_uuid}` /
    /// `{client_id}:{session_uuid}`) so re-subscribe replaces stale intent
    /// atomically (idempotent reattach) across all transport clients.
    pending_terminal_attaches: std::collections::HashMap<String, PendingTerminalAttach>,

    /// Cached fallback terminal theme replies seeded at boot.
    ///
    /// Used only as a fallback when a PTY emits startup probes before any
    /// active terminal client is attached to answer them live.
    terminal_profiles: terminal_profile::TerminalProfileStore,
    /// Shared color cache from boot probe, shared with all `HubEventListener` instances.
    ///
    /// Populated once at startup. `HubEventListener` references this Arc so
    /// `ColorRequest` events are answered immediately from cached values.
    shared_color_cache:
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<usize, crate::terminal::Rgb>>>,
    /// Last known terminal color profile per client peer.
    ///
    /// Used to push the active client's colors into the session parser so
    /// OSC 4/10/11/12 queries are answered from the active client rather than
    /// stale boot defaults.
    terminal_client_profiles:
        std::collections::HashMap<String, std::collections::HashMap<usize, crate::terminal::Rgb>>,
    /// Connected terminal peers per session.
    ///
    /// Tracks which peers currently have a terminal forwarder attached for a
    /// given session so disconnect/unsubscribe can promote another client or
    /// fall back to the boot profile deterministically.
    terminal_session_peers: std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Reverse lookup for terminal forwarder ownership.
    ///
    /// Keyed by forwarder ID (`peer:session`, `tui:session`) so forwarder
    /// teardown can cleanly remove session peer registrations.
    terminal_forwarder_peers: std::collections::HashMap<String, (String, String)>,
    /// Focused terminal owner per session.
    ///
    /// Used to ensure OSC color queries are only forwarded to the active
    /// client terminal, avoiding duplicate auto-replies from passive clients.
    active_terminal_peers: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// TCP stream multiplexers per browser identity for preview tunneling.
    stream_muxes: std::collections::HashMap<String, crate::relay::stream_mux::StreamMultiplexer>,
    /// Temp files from browser paste/drop, keyed by session UUID.
    /// Cleaned up when the session is closed or the hub exits.
    paste_files: std::collections::HashMap<String, Vec<std::path::PathBuf>>,

    /// Hub-owned correlation for prepared SessionIoWorker snapshots.
    pending_session_io_snapshots: std::collections::HashMap<String, PendingSessionIoSnapshot>,

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

    /// Sessions with dead reader threads awaiting background reconnect.
    ///
    /// Keyed by session_uuid. Entries are inserted when `SessionProcessExited`
    /// fires with `exit_code: None` (reader death, not real process exit).
    /// Background tasks attempt reconnect; `CleanupTick` retries and expires
    /// entries older than 110s.
    pending_reconnects: std::collections::HashMap<String, ReconnectState>,

    /// Monotonic counter for reconnect generation tracking.
    reconnect_generation: u64,

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
    /// Workerized terminal clients, keyed by terminal forwarder id.
    terminal_client_workers:
        std::collections::HashMap<String, crate::worker::client::ClientWorkerHandle>,
    /// Workerized browser clients, keyed by WebRTC browser identity.
    browser_client_workers:
        std::collections::HashMap<String, crate::worker::client::ClientWorkerHandle>,
    /// Browser terminal dimensions captured at subscribe time for worker attach policy.
    browser_terminal_attach_sizes: std::collections::HashMap<String, (u16, u16)>,

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
        let webrtc = crate::worker::webrtc::WebRtcPeerRegistry::new();
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

        let hub = Self {
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
            webrtc,
            volume_bursts: std::sync::Mutex::new(VolumeBurstState::default()),
            pty_forwarders: std::collections::HashMap::new(),
            pending_terminal_attaches: std::collections::HashMap::new(),
            terminal_profiles: terminal_profile::TerminalProfileStore::default(),
            shared_color_cache: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            terminal_client_profiles: std::collections::HashMap::new(),
            terminal_session_peers: std::collections::HashMap::new(),
            terminal_forwarder_peers: std::collections::HashMap::new(),
            active_terminal_peers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            stream_muxes: std::collections::HashMap::new(),
            paste_files: std::collections::HashMap::new(),
            pending_session_io_snapshots: std::collections::HashMap::new(),
            lua,
            lua_ac_connections: std::collections::HashMap::new(),
            lua_ac_channels: std::collections::HashMap::new(),
            lua_hub_client_connections: std::collections::HashMap::new(),
            #[cfg(test)]
            pty_notification_queue: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            pty_output_messages_drained: 0,
            notification_watcher_handles: std::collections::HashMap::new(),
            pending_reconnects: std::collections::HashMap::new(),
            reconnect_generation: 0,
            vapid_keys: None,
            push_subscriptions: crate::notifications::push::PushSubscriptionStore::default(),
            singleton_lock: None,
            socket_server: None,
            socket_clients: std::collections::HashMap::new(),
            terminal_client_workers: std::collections::HashMap::new(),
            browser_client_workers: std::collections::HashMap::new(),
            browser_terminal_attach_sizes: std::collections::HashMap::new(),
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
        Ok(hub)
    }

    /// Seed the hub fallback terminal profile from a boot-time color cache.
    pub fn seed_boot_color_cache(
        &mut self,
        cache: &std::sync::Arc<
            std::sync::Mutex<std::collections::HashMap<usize, crate::terminal::Rgb>>,
        >,
    ) {
        let Ok(colors) = cache.lock() else {
            log::warn!("[PTY-PROBE] Failed to lock boot color cache for hub seed");
            return;
        };

        self.terminal_profiles.seed_hub_profile_from_colors(&colors);
        drop(colors);

        self.terminal_profiles
            .refresh_shared_color_cache(&self.shared_color_cache);

        if let Ok(shared) = self.shared_color_cache.lock() {
            log::info!(
                "[PTY-PROBE] Seeded hub fallback cache with {} entries ({})",
                shared.len(),
                self.terminal_profiles.describe_hub_profile()
            );
        }
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
        // Install bundled xterm-ghostty terminfo (pre-compiled at build time).
        // Runs before Lua primitives so config.terminfo() has a result.
        if let Some(data_dir) = crate::env::data_dir() {
            let ti = crate::terminfo::init(&data_dir);
            log::info!("Terminfo: TERM={}, dir={:?}", ti.term, ti.terminfo_dir);
        }

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
    /// Scans the session socket directory for `.sock` files backed by live
    /// session PID files and fires `sessions_discovered`.
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

        let artifacts = daemon::HubRuntimeArtifacts::new(self.hub_identifier.clone());

        // Clean up stale files from previous runs
        artifacts.cleanup_stale_files();

        // Sweep orphaned sockets left by crashed/killed processes
        daemon::cleanup_orphaned_sockets();
        crate::session::cleanup_orphaned_session_files();

        let path = artifacts.socket_path()?;
        let socket_path = path.display().to_string();
        let server = crate::socket::server::SocketServer::start(path, self.hub_event_tx.clone())?;
        log::info!(
            "Socket server started for hub {}",
            &self.hub_identifier[..self.hub_identifier.len().min(8)]
        );
        self.socket_server = Some(server);

        // Persist ownership metadata after a successful bind so failed startup
        // attempts never steal pid/manifest ownership from a live hub.
        if let Err(e) = artifacts.publish_current_process(self.botster_id.as_deref()) {
            log::warn!("Failed to publish hub runtime artifacts: {e}");
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

    /// Rebind the public hub socket if its pathname was unlinked while the
    /// hub is still running.
    ///
    /// Existing socket clients keep their connected streams. This only
    /// replaces the listener used by new clients such as `botster mcp-serve`.
    pub(crate) fn repair_missing_socket_path(&mut self) {
        let _guard = self.tokio_runtime.enter();

        if self.socket_server.is_none() {
            return;
        }

        let artifacts = daemon::HubRuntimeArtifacts::new(self.hub_identifier.clone());
        let path = match artifacts.socket_path() {
            Ok(path) => path,
            Err(e) => {
                log::error!("[Socket] Failed to resolve hub socket path for repair: {e}");
                return;
            }
        };

        if path.exists() {
            return;
        }
        self.hub_event_metrics
            .record_counter("socket_path.repair", 1);

        log::error!(
            "[Socket] Hub socket path disappeared while hub is running; rebinding {}",
            path.display()
        );

        if let Some(server) = self.socket_server.take() {
            server.shutdown();
        }

        let socket_path = path.display().to_string();
        match crate::socket::server::SocketServer::start(path, self.hub_event_tx.clone()) {
            Ok(server) => {
                self.socket_server = Some(server);
                if let Err(e) = artifacts.publish_current_process(self.botster_id.as_deref()) {
                    log::warn!("[Socket] Failed to refresh runtime artifacts after repair: {e}");
                }
                self.fire_hub_recovery_state(
                    "socket_repaired",
                    serde_json::json!({ "socket_path": socket_path }),
                );
            }
            Err(e) => {
                self.hub_event_metrics
                    .record_counter("socket_path.repair_error", 1);
                log::error!(
                    "[Socket] Failed to rebind missing hub socket at {}: {e}",
                    socket_path
                );
            }
        }
    }

    /// Eagerly generate the connection URL.
    ///
    /// In headless mode there is no TUI to trigger lazy generation, so
    /// external tools (system tests, automation) need the URL written to
    /// disk at startup. TUI mode now also calls this so the connection_code
    /// entity is available to late subscribers and the Share dialog without
    /// a separate first-use `get_connection_code` round-trip.
    ///
    /// Fires `connection_code_ready` on success so the Lua handler at
    /// `cli/lua/handlers/connections.lua` persists the URL into
    /// `connections.last_connection_code` state and ships the
    /// `connection_code` entity. Without that, the snapshot path
    /// (`cli/lua/hub/init.lua`) returns an empty array even after the
    /// bundle is generated.
    pub fn eager_generate_connection_url(&mut self) {
        match self.generate_connection_url() {
            Ok(url) => {
                log::info!("Connection URL generated ({} chars)", url.len());
                if let Err(e) = self.lua.fire_connection_code_ready(&url) {
                    log::warn!("Failed to fire connection_code_ready: {e}");
                }
            }
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
        daemon::HubRuntimeArtifacts::new(self.hub_identifier.clone()).cleanup_on_shutdown();

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

        self.webrtc.shutdown(&self.tokio_runtime);

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
        let mut config = Config::default();
        config.server_url = "http://localhost:3000".to_string();
        config.token = "btstr_test-key".to_string();
        config.poll_interval = 10;
        config.agent_timeout = 300;
        config.max_sessions = 10;
        config.worktree_base = PathBuf::from("/tmp/test-worktrees");
        config
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

    #[test]
    fn test_hub_repairs_missing_socket_path_without_shutdown() {
        let test_hub_id = format!("_test_socket_repair_{}", std::process::id());

        let mut hub = Hub::with_runtime(test_config(), shared_test_runtime()).unwrap();
        hub.hub_identifier = test_hub_id.clone();
        hub.start_socket_server()
            .expect("hub should start socket server");

        let sock_path = daemon::socket_path(&test_hub_id).unwrap();
        assert!(sock_path.exists(), "socket should exist after startup");

        std::fs::remove_file(&sock_path).expect("test should unlink socket path");
        assert!(
            !sock_path.exists(),
            "test precondition: socket path missing"
        );

        hub.repair_missing_socket_path();

        assert!(
            sock_path.exists(),
            "repair should recreate the socket pathname"
        );
        std::os::unix::net::UnixStream::connect(&sock_path)
            .expect("repaired socket should accept new clients");

        hub.shutdown();
        drop(hub);
        let _ = std::fs::remove_file(daemon::lock_file_path(&test_hub_id).unwrap());
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
    ///   1. Spawn bash → reader thread reads master FD → broadcast output
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
                    command: "bash".to_string(),
                    args: vec!["--norc".to_string(), "--noprofile".to_string()],
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

            let (shared_state, event_tx, kitty_enabled, cursor_visible, resize_pending) =
                pty_session.get_direct_access();

            let pty = PtyHandle::new(
                event_tx,
                Arc::clone(&shared_state),
                kitty_enabled,
                cursor_visible,
                resize_pending,
                None,
            );

            // Store the pty handle for the reader thread to use
            *pty_handle_ref.lock().unwrap() = Some(pty.clone());

            // Start a reader thread that reads from the master FD and
            // broadcasts output — hub is a router, no local parsing.
            let stop_flag = Arc::new(AtomicBool::new(false));
            let stop_clone = Arc::clone(&stop_flag);
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
                            // Broadcast output to subscribers (no local parsing)
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
                // Scrollback is empty: test PtyHandle has no session process,
                // so get_snapshot() returns empty bytes. Live output still
                // flows through the broadcast channel.
                let _ = data;
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
                // Scrollback is empty: test PtyHandle has no session process.
                let _ = data;
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
