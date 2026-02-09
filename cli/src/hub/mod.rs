//! Hub - Central orchestrator for agent management.
//!
//! The Hub is the core of botster-hub, owning all state and running the main
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
//! - `lifecycle`: Agent close operations (spawn is now Lua-owned)
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

pub mod actions;
pub mod agent_handle;
pub mod command_channel;
pub mod handle_cache;
pub mod lifecycle;
pub mod registration;
pub mod run;
mod server_comms;
pub mod state;
pub mod workers;

pub use actions::HubAction;
pub use agent_handle::AgentPtys;
pub use state::{HubState, SharedHubState};

use std::sync::Arc;
use std::time::Instant;

use reqwest::blocking::Client;

use sha2::{Digest, Sha256};

use crate::channel::Channel;
use crate::config::Config;
use crate::device::Device;
use crate::git::WorktreeManager;
use crate::lua::LuaRuntime;

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
pub(crate) fn hub_id_for_repo(repo_path: &std::path::Path) -> String {
    let canonical = repo_path
        .canonicalize()
        .unwrap_or_else(|_| repo_path.to_path_buf());

    let hash = Sha256::digest(canonical.to_string_lossy().as_bytes());

    // Use first 16 bytes as hex (32 chars) - enough uniqueness, shorter than UUID
    hash[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Central orchestrator for the botster-hub application.
///
/// The Hub owns all application state and coordinates between the TUI,
/// server polling, and browser relay components. It can run in either
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
    /// Async runtime for relay and preview channel operations.
    pub tokio_runtime: tokio::runtime::Runtime,

    // === Control Flags ===
    /// Whether the hub should quit.
    pub quit: bool,

    // === Timing ===
    /// Last time we sent a heartbeat.
    pub last_heartbeat: Instant,

    // === Browser Relay ===
    /// Browser connection state and communication.
    pub browser: crate::relay::BrowserState,

    // === Background Workers ===
    /// Background worker for notification sending (non-blocking).
    pub notification_worker: Option<workers::NotificationWorker>,
    /// WebSocket command channel for real-time message delivery from Rails.
    pub command_channel: Option<command_channel::CommandChannelHandle>,

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

    /// Sender for PTY output messages from forwarder tasks.
    ///
    /// Forwarder tasks send PTY output here; main loop drains and sends via WebRTC.
    pub webrtc_pty_output_tx: tokio::sync::mpsc::UnboundedSender<WebRtcPtyOutput>,
    /// Receiver for PTY output messages.
    pub webrtc_pty_output_rx: tokio::sync::mpsc::UnboundedReceiver<WebRtcPtyOutput>,

    /// Active PTY forwarder task handles for cleanup on unsubscribe.
    ///
    /// Maps subscriptionId -> JoinHandle for the forwarder task.
    webrtc_pty_forwarders: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,

    /// Sender for outgoing WebRTC signals (ICE candidates) from async callbacks.
    ///
    /// Cloned for each new WebRTC channel. The async `on_ice_candidate` callback
    /// encrypts the candidate and sends it here. `poll_outgoing_signals()` drains
    /// the receiver and relays via `CommandChannelHandle::perform("signal", ...)`.
    pub webrtc_outgoing_signal_tx: tokio::sync::mpsc::UnboundedSender<crate::channel::webrtc::OutgoingSignal>,
    /// Receiver for outgoing WebRTC signals. Drained in `tick()`.
    webrtc_outgoing_signal_rx: tokio::sync::mpsc::UnboundedReceiver<crate::channel::webrtc::OutgoingSignal>,

    /// TCP stream multiplexers per browser identity for preview tunneling.
    stream_muxes: std::collections::HashMap<String, crate::relay::stream_mux::StreamMultiplexer>,
    /// Receiver for incoming stream frames from WebRTC DataChannels.
    stream_frame_rx: tokio::sync::mpsc::UnboundedReceiver<crate::channel::webrtc::StreamIncoming>,
    /// Sender for incoming stream frames (cloned into each WebRtcChannel).
    pub stream_frame_tx: tokio::sync::mpsc::UnboundedSender<crate::channel::webrtc::StreamIncoming>,

    // === Handle Cache ===
    /// Thread-safe cache of agent handles for non-blocking client access.
    ///
    /// Updated by Hub when agents are created/deleted via `sync_handle_cache()`.
    /// `HandleCache::get_agent()` reads from this cache directly, allowing clients
    /// to access agent handles without blocking commands - safe from any thread.
    pub handle_cache: Arc<handle_cache::HandleCache>,

    // === Lua Scripting ===
    /// Lua scripting runtime for hot-reloadable behavior customization.
    pub lua: LuaRuntime,

    /// Pending PTY output observer notifications.
    ///
    /// Populated during [`Self::poll_webrtc_pty_output`] (after WebRTC send),
    /// drained independently in [`Self::poll_pty_observers`] so slow observers
    /// never block the WebRTC fast path.
    pty_observer_queue: std::collections::VecDeque<PtyObserverNotification>,

    // === TUI via Lua (Hub-side Processing) ===
    /// Sender for TUI output messages to TuiRunner.
    ///
    /// Set by `register_tui_via_lua()`. Hub sends `TuiOutput` messages
    /// through this channel directly.
    tui_output_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::client::TuiOutput>>,
    /// Receiver for TUI requests from TuiRunner.
    ///
    /// Set by `register_tui_via_lua()`. Polled by `poll_tui_requests()`.
    tui_request_rx: Option<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>,
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

        // Generate stable hub_identifier: env var > repo path hash
        // NO FALLBACKS - if we can't determine a stable identifier, fail explicitly
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
                Err(e) => {
                    // No silent fallback to random UUID - that causes session mismatches
                    // Either set BOTSTER_HUB_ID env var or run from a git repository
                    anyhow::bail!(
                        "Cannot determine hub identifier. Either:\n\
                         1. Run from within a git repository, or\n\
                         2. Set BOTSTER_HUB_ID environment variable\n\
                         \n\
                         Error: {e}"
                    );
                }
            }
        };

        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        // Load or create device identity for E2E encryption
        let device = Device::load_or_create()?;
        log::info!("Device fingerprint: {}", device.fingerprint);

        // Initialize heartbeat timestamp to past to trigger immediate heartbeat on first tick
        let past = Instant::now() - std::time::Duration::from_secs(3600);

        // Create handle cache for thread-safe agent handle access
        let handle_cache = Arc::new(handle_cache::HandleCache::new());
        // Create channel for WebRTC PTY output from forwarder tasks
        let (webrtc_pty_output_tx, webrtc_pty_output_rx) = tokio::sync::mpsc::unbounded_channel();
        // Create channel for outgoing WebRTC signals (ICE candidates from async callbacks)
        let (webrtc_outgoing_signal_tx, webrtc_outgoing_signal_rx) = tokio::sync::mpsc::unbounded_channel();
        // Create channel for incoming stream multiplexer frames from WebRTC DataChannels
        let (stream_frame_tx, stream_frame_rx) = tokio::sync::mpsc::unbounded_channel();

        // Initialize Lua scripting runtime
        let lua = LuaRuntime::new()?;

        Ok(Self {
            state,
            config,
            client,
            device,
            hub_identifier,
            botster_id: None,
            tokio_runtime,
            quit: false,
            last_heartbeat: past,
            browser: crate::relay::BrowserState::default(),
            // Workers are started later via start_background_workers() after registration
            notification_worker: None,
            command_channel: None,
            handle_cache,
            webrtc_channels: std::collections::HashMap::new(),
            webrtc_connection_started: std::collections::HashMap::new(),
            webrtc_pty_output_tx,
            webrtc_pty_output_rx,
            webrtc_pty_forwarders: std::collections::HashMap::new(),
            webrtc_outgoing_signal_tx,
            webrtc_outgoing_signal_rx,
            stream_muxes: std::collections::HashMap::new(),
            stream_frame_rx,
            stream_frame_tx,
            lua,
            pty_observer_queue: std::collections::VecDeque::new(),
            tui_output_tx: None,
            tui_request_rx: None,
        })
    }

    /// Start background workers for non-blocking network I/O.
    ///
    /// Call this after hub registration completes and `botster_id` is set.
    /// Currently starts the NotificationWorker for background notification sending.
    fn start_background_workers(&mut self) {
        let worker_config = workers::WorkerConfig {
            server_url: self.config.server_url.clone(),
            api_key: self.config.get_api_key().to_string(),
            server_hub_id: self.server_hub_id().to_string(),
        };

        // Start notification worker
        if self.notification_worker.is_none() {
            log::info!("Starting background notification worker");
            self.notification_worker = Some(workers::NotificationWorker::new(worker_config));
        }
    }

    /// Connect to the HubCommandChannel for real-time message delivery.
    ///
    /// Call this after hub registration completes and `botster_id` is set.
    /// The command channel replaces HTTP polling for message delivery.
    fn connect_command_channel(&mut self) {
        if self.command_channel.is_some() {
            log::warn!("Command channel already connected");
            return;
        }

        let server_url = self.config.server_url.clone();
        let api_key = self.config.get_api_key().to_string();
        let hub_id = self.server_hub_id().to_string();

        // Start from sequence 0 on first connect (no messages acked yet)
        let start_from = 0i64;

        // Detect repo for GitHub event subscription
        let repo = if let Ok(repo) = std::env::var("BOTSTER_REPO") {
            Some(repo)
        } else {
            match crate::git::WorktreeManager::detect_current_repo() {
                Ok((_path, name)) => Some(name),
                Err(_) => None,
            }
        };

        log::info!("Connecting command channel to {} (hub={}, repo={:?})", server_url, hub_id, repo);

        // Must enter tokio runtime context for tokio::spawn in connect()
        let _guard = self.tokio_runtime.enter();
        let handle = command_channel::connect(&server_url, &api_key, &hub_id, start_from, repo.as_deref());
        self.command_channel = Some(handle);
    }

    /// Shutdown all background workers gracefully.
    fn shutdown_background_workers(&mut self) {
        if let Some(worker) = self.notification_worker.take() {
            worker.shutdown();
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

    /// Get the number of active agents.
    #[must_use]
    pub fn agent_count(&self) -> usize {
        self.state.read().unwrap().agent_count()
    }

    /// Sync the handle cache with current state.
    ///
    /// Rebuilds HandleCache from HubState's agent PTY handles. Call this after
    /// agents are created or deleted. Agent metadata is not included -- Lua
    /// manages that separately.
    pub fn sync_handle_cache(&self) {
        let state = self.state.read().unwrap();
        let handles: Vec<AgentPtys> = (0..state.agent_count())
            .filter_map(|i| state.get_agent_handle(i))
            .collect();
        self.handle_cache.set_all(handles);
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
    /// Delegates to `HubState::load_available_worktrees()` and syncs the
    /// result to HandleCache for non-blocking client reads.
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
        self.register_device();
        self.register_hub_with_server();
        self.init_crypto_service();

        // Start background workers for non-blocking network I/O
        // Must be called after register_hub_with_server() sets botster_id
        self.start_background_workers();

        // Connect command channel for real-time WebSocket message delivery
        // Must be called after register_hub_with_server() sets botster_id
        self.connect_command_channel();

        // Seed shared state so clients have data immediately
        if let Err(e) = self.load_available_worktrees() {
            log::warn!("Failed to load initial worktrees: {}", e);
        }

        // Register Hub primitives with Lua runtime (must happen before loading init script)
        if let Err(e) = self.lua.register_hub_primitives(
            Arc::clone(&self.handle_cache),
            self.config.worktree_base.clone(),
        ) {
            log::warn!("Failed to register Hub Lua primitives: {}", e);
        }

        // Load Lua init script and start file watching for hot-reload
        self.load_lua_init();
        self.start_lua_file_watching();

        // Bundle generation is deferred - don't call generate_connection_url() here.
        // The bundle will be generated lazily when:
        // 1. TUI requests QR code display (GetConnectionCode command)
        // 2. External automation requests the connection URL
        // This avoids blocking boot for up to 10 seconds.
    }

    /// Load the Lua initialization script.
    ///
    /// Loading priority:
    /// 1. User overrides in `~/.botster/lua/` - filesystem with hot-reload
    /// 2. Dev mode (debug build) - `cli/lua/` filesystem with hot-reload
    /// 3. Release mode - embedded files from binary (no hot-reload)
    fn load_lua_init(&mut self) {
        use std::path::Path;

        // Check if user has their own Lua files (always takes priority)
        let user_init_path = self.lua.base_path().join("core").join("init.lua");
        if user_init_path.exists() {
            let init_path = Path::new("core/init.lua");
            if let Err(e) = self.lua.load_file(init_path) {
                log::warn!("Failed to load user init.lua: {}", e);
            } else {
                log::info!("Loaded Lua from user path: {}", user_init_path.display());
                return;
            }
        }

        // In debug builds, use source directory for hot-reload during development
        #[cfg(debug_assertions)]
        {
            let dev_lua_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("lua");
            let dev_init_path = dev_lua_dir.join("core").join("init.lua");

            if dev_init_path.exists() {
                log::info!("Dev mode: using Lua files from {}", dev_lua_dir.display());

                // Update base path so file watcher monitors this directory
                self.lua.set_base_path(dev_lua_dir.clone());

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

        // Release mode: extract embedded Lua to filesystem, then load from there.
        // This enables hot-reload even in release builds (agent can edit Lua on disk).
        #[cfg(not(debug_assertions))]
        {
            match self.lua.ensure_lua_on_filesystem() {
                Ok(extracted_path) => {
                    log::info!(
                        "Release mode: Lua extracted to {}, loading from filesystem",
                        extracted_path.display()
                    );
                    let init_path = std::path::Path::new("core/init.lua");
                    if let Err(e) = self.lua.load_file(init_path) {
                        log::warn!("Failed to load extracted init.lua: {}", e);
                        // Fall back to in-memory embedded loading
                        if let Err(e) = self.lua.load_embedded() {
                            log::warn!("Fallback embedded load also failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Failed to extract Lua files: {}, using in-memory fallback", e);
                    if let Err(e) = self.lua.load_embedded() {
                        log::warn!("Failed to load embedded Lua: {}", e);
                    }
                }
            }
        }

        // Fallback for debug builds where dev directory doesn't exist
        #[cfg(debug_assertions)]
        {
            log::info!("Dev directory not found, using embedded Lua files");
            if let Err(e) = self.lua.load_embedded() {
                log::warn!("Failed to load embedded Lua: {}", e);
            }
        }
    }

    /// Start Lua file watching for hot-reload support.
    ///
    /// If the Lua base path exists, this enables automatic reloading of
    /// modified Lua scripts during the event loop.
    fn start_lua_file_watching(&mut self) {
        if let Err(e) = self.lua.start_file_watching() {
            log::warn!("Failed to start Lua file watching: {}", e);
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
        // Notify Lua that TUI is disconnecting
        if let Err(e) = self.lua.call_tui_disconnected() {
            log::warn!("Lua tui_disconnected callback error: {}", e);
        }

        // Fire Lua shutdown event (before any cleanup)
        if let Err(e) = self.lua.fire_shutdown() {
            log::warn!("Lua shutdown event error: {}", e);
        }

        // Shutdown command channel
        if let Some(ref channel) = self.command_channel {
            channel.shutdown();
        }
        self.command_channel = None;

        // Shutdown background workers (allows pending notifications to drain)
        self.shutdown_background_workers();

        // Abort all PTY forwarder tasks
        for (_key, task) in self.webrtc_pty_forwarders.drain() {
            task.abort();
        }

        // Close all stream multiplexers
        for (_id, mut mux) in self.stream_muxes.drain() {
            mux.close_all();
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
    /// * `request_rx` - Receiver for JSON messages from TuiRunner
    ///
    /// # Returns
    ///
    /// Receiver for TuiOutput messages to TuiRunner.
    pub fn register_tui_via_lua(
        &mut self,
        request_rx: tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>,
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
        self.flush_lua_queues();

        log::info!("TUI registered via Lua (Hub-side processing)");

        output_rx
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
        }
    }

    #[test]
    fn test_hub_creation() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

        assert!(!hub.should_quit());
        assert_eq!(hub.agent_count(), 0);
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

    // === Agent Lifecycle / HandleCache Integration Tests ===
    //
    // These tests verify the integration between Hub, HandleCache, and HubAction
    // for agent lifecycle operations (create, select, delete).

    /// Helper: create a test agent and return (key, agent).
    fn make_agent(issue: u32) -> (String, crate::agent::Agent) {
        let agent = crate::agent::Agent::new(
            uuid::Uuid::new_v4(),
            "test/repo".to_string(),
            Some(issue),
            format!("botster-issue-{}", issue),
            PathBuf::from("/tmp/test"),
        );
        (format!("test-repo-{}", issue), agent)
    }

    /// Helper: add agent to hub state and return the key.
    fn add_agent_to_hub(hub: &Hub, issue: u32) -> String {
        let (key, agent) = make_agent(issue);
        hub.state.write().unwrap().add_agent(key.clone(), agent);
        key
    }

    #[test]
    fn test_handle_cache_syncs_on_agent_create() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

        // Initially empty
        assert!(hub.handle_cache.is_empty());

        // Add agent to state
        let key = add_agent_to_hub(&hub, 42);

        // Sync cache
        hub.sync_handle_cache();

        // Cache should have 1 agent
        assert_eq!(hub.handle_cache.len(), 1);
        let cached = hub.handle_cache.get_agent(0);
        assert!(cached.is_some(), "get_agent(0) should return Some after sync");

        // Verify agent_id matches
        let handle = cached.unwrap();
        assert_eq!(handle.agent_key(), key);
    }

    #[test]
    fn test_handle_cache_syncs_on_agent_delete() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

        // Add 2 agents
        let key1 = add_agent_to_hub(&hub, 1);
        let key2 = add_agent_to_hub(&hub, 2);
        hub.sync_handle_cache();

        assert_eq!(hub.handle_cache.len(), 2);
        assert_eq!(hub.handle_cache.get_agent(0).unwrap().agent_key(), &key1);
        assert_eq!(hub.handle_cache.get_agent(1).unwrap().agent_key(), &key2);

        // Remove agent 0 (key1)
        hub.state.write().unwrap().remove_agent(&key1);
        hub.sync_handle_cache();

        // Cache should now have 1 agent, and index 0 should point to what was agent 1 (key2)
        assert_eq!(hub.handle_cache.len(), 1);
        let remaining = hub.handle_cache.get_agent(0).unwrap();
        assert_eq!(
            remaining.agent_key(), key2,
            "After deleting agent 0, index 0 should now point to what was agent 1"
        );
    }

    // Note: TUI request processing tests are in runner.rs.

    #[test]
    fn test_create_delete_agent_cycle() {
        let config = test_config();
        let hub = Hub::new(config).unwrap();

        // Add 3 agents, sync cache after each
        let key1 = add_agent_to_hub(&hub, 1);
        hub.sync_handle_cache();
        assert_eq!(hub.handle_cache.len(), 1);

        let key2 = add_agent_to_hub(&hub, 2);
        hub.sync_handle_cache();
        assert_eq!(hub.handle_cache.len(), 2);

        let key3 = add_agent_to_hub(&hub, 3);
        hub.sync_handle_cache();
        assert_eq!(hub.handle_cache.len(), 3);

        // Verify cache contents
        assert_eq!(hub.handle_cache.get_agent(0).unwrap().agent_key(), &key1);
        assert_eq!(hub.handle_cache.get_agent(1).unwrap().agent_key(), &key2);
        assert_eq!(hub.handle_cache.get_agent(2).unwrap().agent_key(), &key3);

        // Delete middle agent (key2)
        hub.state.write().unwrap().remove_agent(&key2);
        hub.sync_handle_cache();

        // Cache should have 2 agents with correct IDs
        assert_eq!(hub.handle_cache.len(), 2);
        assert_eq!(
            hub.handle_cache.get_agent(0).unwrap().agent_key(), &key1,
            "After deleting middle agent, index 0 should still be agent 1"
        );
        assert_eq!(
            hub.handle_cache.get_agent(1).unwrap().agent_key(), &key3,
            "After deleting middle agent, index 1 should now be agent 3"
        );
    }

    // === Stress / Concurrency Tests ===
    //
    // These tests verify thread safety and deadlock freedom under concurrent
    // access patterns. Each uses a timeout to detect deadlocks.

    /// Run a closure on a background thread with a timeout.
    ///
    /// If the closure doesn't complete within the given duration, the test
    /// fails with a "possible deadlock" message. This is the primary mechanism
    /// for detecting deadlocks in concurrent tests.
    fn run_with_timeout<F: FnOnce() + Send + 'static>(f: F, timeout: std::time::Duration) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            f();
            let _ = tx.send(());
        });
        rx.recv_timeout(timeout).expect("Test timed out - possible deadlock");
    }

    /// Stress test: concurrent reads and writes to HandleCache.
    ///
    /// Spawns a reader thread that continuously reads from the cache while
    /// the main thread adds/removes agents and syncs the cache. Verifies
    /// that the RwLock-based HandleCache doesn't deadlock or panic under
    /// concurrent access.
    ///
    /// This exercises the production pattern where clients read from
    /// HandleCache while Hub mutates it on agent lifecycle events.
    #[test]
    fn test_handle_cache_concurrent_read_write() {
        use std::sync::Arc;
        use std::time::Duration;

        run_with_timeout(|| {
            let config = test_config();
            let hub = Hub::new(config).unwrap();
            let cache = Arc::clone(&hub.handle_cache);

            // Spawn reader thread: continuously reads from cache
            let reader_cache = Arc::clone(&cache);
            let reader_handle = std::thread::spawn(move || {
                for _ in 0..100 {
                    // Exercise all read paths
                    let _len = reader_cache.len();
                    let _empty = reader_cache.is_empty();
                    let _agent = reader_cache.get_agent(0);
                    let _all = reader_cache.get_all_agents();

                    // Small yield to interleave with writer
                    std::thread::yield_now();
                }
            });

            // Main thread: add/remove agents and sync cache
            for i in 0..100u32 {
                let key = add_agent_to_hub(&hub, i);
                hub.sync_handle_cache();

                // Every other iteration, remove the agent we just added
                if i % 2 == 0 {
                    hub.state.write().unwrap().remove_agent(&key);
                    hub.sync_handle_cache();
                }
            }

            // Join reader - should not have panicked
            reader_handle.join().expect("Reader thread panicked during concurrent access");
        }, Duration::from_secs(5));
    }

    /// Consistency test: cache count matches state count through add/remove cycle.
    ///
    /// Adds 5 agents one by one, calling sync_handle_cache() after each, then
    /// removes 3 agents one by one with sync after each. At every step, verifies
    /// that the cache count matches the state count.
    ///
    /// This is a deterministic correctness test (not a concurrency test) that
    /// ensures the cache faithfully mirrors Hub state through lifecycle events.
    #[test]
    fn test_multiple_cache_syncs_are_consistent() {
        use std::time::Duration;

        run_with_timeout(|| {
            let config = test_config();
            let hub = Hub::new(config).unwrap();

            // Add 5 agents one by one, verify cache matches state at each step
            let mut keys = Vec::new();
            for i in 1..=5u32 {
                let key = add_agent_to_hub(&hub, i);
                keys.push(key);
                hub.sync_handle_cache();

                let state_count = hub.state.read().unwrap().agent_count();
                let cache_count = hub.handle_cache.len();
                assert_eq!(
                    cache_count, state_count,
                    "After adding agent {i}: cache count ({cache_count}) should match state count ({state_count})"
                );
                assert_eq!(cache_count, i as usize);
            }

            // Remove 3 agents one by one, verify cache matches state at each step
            for (removed_count, key) in keys.iter().take(3).enumerate() {
                hub.state.write().unwrap().remove_agent(key);
                hub.sync_handle_cache();

                let state_count = hub.state.read().unwrap().agent_count();
                let cache_count = hub.handle_cache.len();
                let expected = 5 - (removed_count + 1);
                assert_eq!(
                    cache_count, state_count,
                    "After removing agent {}: cache count ({cache_count}) should match state count ({state_count})",
                    removed_count + 1
                );
                assert_eq!(cache_count, expected);
            }

            // Final verification: 2 agents remain
            assert_eq!(hub.handle_cache.len(), 2);
            assert_eq!(hub.state.read().unwrap().agent_count(), 2);

            // Verify the remaining agents are correct (keys[3] and keys[4])
            let cached_agents = hub.handle_cache.get_all_agents();
            assert_eq!(cached_agents[0].agent_key(), &keys[3]);
            assert_eq!(cached_agents[1].agent_key(), &keys[4]);
        }, Duration::from_secs(5));
    }

}
