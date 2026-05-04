//! Unified event channel for the Hub event loop.
//!
//! All background producers (HTTP threads, WebSocket threads, tokio tasks,
//! PTY watchers, timers, forwarding tasks) send events through a single
//! `mpsc::UnboundedSender<HubEvent>`. The `select!` loop receives on the
//! corresponding receiver and dispatches via `handle_hub_event()`.

// Rust guideline compliant 2026-02

use crate::file_watcher::FileEvent;
use crate::lua::primitives::action_cable::ActionCableRequest;
use crate::lua::primitives::connection::ConnectionRequest;
use crate::lua::primitives::http::CompletedHttpResponse;
use crate::lua::primitives::hub::HubRequest;
use crate::lua::primitives::hub_client::HubClientRequest;
use crate::lua::primitives::pty::PtyRequest;
use crate::lua::primitives::tui::TuiSendRequest;
use crate::lua::primitives::webrtc::WebRtcSendRequest;
use crate::lua::primitives::websocket::WsEvent;
use crate::lua::primitives::worktree::WorktreeRequest;
use crate::socket::client_conn::SocketClientConn;
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Event from a background producer delivered to the Hub event loop.
///
/// Background threads and spawned tasks send events through a single
/// `mpsc::UnboundedSender<HubEvent>`. The `select!` loop dispatches
/// each variant via `handle_hub_event()`.
#[derive(Debug)]
pub(crate) enum HubEvent {
    /// Completed HTTP response from a background thread.
    HttpResponse(CompletedHttpResponse),

    /// WebSocket event from a background connection thread.
    WebSocketEvent(WsEvent),

    /// PTY notification from a watcher task.
    PtyNotification(super::PtyNotificationEvent),

    /// PTY OSC metadata event forwarded from a watcher task.
    ///
    /// A single variant carries agent context + the raw PtyEvent.
    /// The Lua bridge method discriminates the event type and fires
    /// the appropriate hook (e.g., `pty_title_changed`, `pty_cwd_changed`).
    PtyOscEvent {
        /// Session UUID for routing and Lua hook context.
        session_uuid: String,
        /// Session name (e.g., "agent", "server").
        session_name: String,
        /// The PtyEvent variant (TitleChanged, CwdChanged, PromptMark).
        event: crate::agent::pty::PtyEvent,
    },

    /// PTY process exited (reader thread detected EOF).
    ///
    /// Sent from the notification watcher task when it receives
    /// `PtyEvent::ProcessExited`. Triggers Lua `process_exited` event
    /// which updates agent status and broadcasts to all clients.
    PtyProcessExited {
        /// Session UUID identifying which session's PTY exited.
        session_uuid: String,
        /// Session name (e.g., "agent", "server").
        session_name: String,
        /// Exit code if available (None if killed by signal or unknown).
        exit_code: Option<i32>,
    },

    /// PTY output observed directly from the session PTY stream.
    ///
    /// Used to feed Lua `pty_output` hooks from the session itself, so
    /// output-driven logic like idle detection does not depend on any client
    /// transport being attached.
    PtyOutputObserved {
        /// Session UUID for hook context.
        session_uuid: String,
        /// Raw PTY bytes produced by the session.
        data: Vec<u8>,
    },

    /// Compact session-I/O worker batch.
    ///
    /// Worker-origin batches preserve byte order while intentionally allowing
    /// Lua `pty_output` observers to see coalesced chunks instead of the
    /// session protocol's original frame boundaries.
    SessionIoBatch(crate::worker::session_io::SessionIoBatch),

    /// Session-I/O worker result for non-output mailbox requests.
    SessionIo(crate::worker::session_io::SessionIoEvent),

    /// Drop a Hub-owned pending session-I/O snapshot request.
    ///
    /// Spawned Hub tasks emit this when a snapshot request was reserved in the
    /// pending map but no worker mailbox request was accepted.
    DropPendingSessionIoSnapshot {
        /// Pending snapshot request ID.
        request_id: String,
    },

    /// Client-worker request that needs hub-owned orchestration state.
    ClientWorkerControl(crate::worker::hub_control::HubControlMessage),

    /// WebRTC DataChannel has opened for a browser peer.
    ///
    /// Sent from the `on_data_channel` callback. Triggers `peer_connected`
    /// Lua callback and spawns the WebRTC message forwarding task.
    DcOpened {
        /// Browser identity key for the peer whose DC just opened.
        browser_identity: String,
    },

    /// A bounded WebRTC ingress queue filled up for a browser peer.
    ///
    /// Indicates the Hub is no longer keeping up with inbound frames from that
    /// peer. The channel should be cleaned up so the browser reconnects and
    /// re-synchronizes state from a clean baseline.
    WebRtcIngressBackpressure {
        /// Browser identity for the overloaded WebRTC connection.
        browser_identity: String,
        /// Queue/source label for diagnostics.
        source: &'static str,
    },

    /// Lua timer has fired (one-shot or repeating iteration).
    ///
    /// Spawned tokio tasks send this after `tokio::time::sleep()` completes.
    /// The handler looks up the callback key in the timer registry.
    TimerFired {
        /// Unique timer ID (e.g. `"timer_0"`).
        timer_id: String,
    },

    /// ActionCable channel message from a forwarding task.
    ///
    /// One forwarding task per channel reads from `ChannelHandle.message_rx`
    /// and sends this event for each received message.
    AcChannelMessage {
        /// Channel ID for callback lookup.
        channel_id: String,
        /// Raw JSON message from the ActionCable channel.
        message: serde_json::Value,
    },

    /// WebRTC DataChannel message from a forwarding task.
    ///
    /// One forwarding task per peer reads from `recv_rx` and sends this
    /// event for each received message.
    WebRtcMessage {
        /// Browser identity key for the peer that sent this message.
        browser_identity: String,
        /// Decrypted message payload bytes.
        payload: Vec<u8>,
    },

    /// Binary PTY input frame from a WebRTC registry-owned queue forwarder.
    WebRtcPtyInput(crate::channel::webrtc::PtyInputIncoming),

    /// Browser file-transfer frame from a WebRTC registry-owned queue forwarder.
    WebRtcFileInput(crate::channel::webrtc::FileInputIncoming),

    /// Outgoing signaling envelope from a WebRTC registry-owned queue forwarder.
    WebRtcOutgoingSignal(crate::channel::webrtc::OutgoingSignal),

    /// Transport egress produced by a browser ClientWorker.
    WebRtcClientWorkerEgress {
        /// Browser identity key for the peer that owns the worker.
        browser_identity: String,
        /// Transport-neutral egress frame from the worker.
        egress: crate::worker::transport::TransportEgress,
    },

    /// Stream multiplexer frame from a WebRTC registry-owned queue forwarder.
    WebRtcStreamFrame(crate::channel::webrtc::StreamIncoming),

    /// User file watch event from a blocking forwarder task.
    ///
    /// One forwarder per `watch.directory()` call reads from the `notify`
    /// crate's `std::sync::mpsc::Receiver` and sends classified events.
    UserFileWatch {
        /// Watch ID for callback lookup (e.g. `"watch_0"`).
        watch_id: String,
        /// Classified file events from the OS watcher.
        events: Vec<FileEvent>,
    },

    /// Periodic cleanup tick from a spawned interval task.
    ///
    /// Fires every 5 seconds. Handles WebRTC connection cleanup
    /// (timeout/disconnect checks) and safety-net queue drains for
    /// stream frames and PTY observers.
    CleanupTick,

    // =========================================================================
    // Lua primitive events — sent directly from Lua closures via HubEventSender
    // =========================================================================
    /// WebRTC send request from a Lua callback.
    WebRtcSend(WebRtcSendRequest),

    /// TUI send request from a Lua callback.
    TuiSend(TuiSendRequest),

    /// PTY operation request from a Lua callback.
    LuaPtyRequest(PtyRequest),

    /// Hub operation request from a Lua callback.
    LuaHubRequest(HubRequest),

    /// Connection operation request from a Lua callback.
    LuaConnectionRequest(ConnectionRequest),

    /// Worktree operation request from a Lua callback.
    LuaWorktreeRequest(WorktreeRequest),

    /// ActionCable operation request from a Lua callback.
    LuaActionCableRequest(ActionCableRequest),

    /// Hub client operation request from a Lua callback.
    LuaHubClientRequest(HubClientRequest),

    /// Incoming JSON message from a remote hub via outgoing socket client.
    HubClientMessage {
        /// Connection ID for callback lookup.
        connection_id: String,
        /// JSON message from the remote hub.
        message: serde_json::Value,
    },

    /// Remote hub connection disconnected (EOF or error).
    HubClientDisconnected {
        /// Connection ID that disconnected.
        connection_id: String,
    },

    /// Web push notification request from a Lua callback.
    ///
    /// Sent from Lua's `push.send()` with a JSON payload containing
    /// notification fields (kind, title, body, url, icon, tag, data).
    /// The Hub merges defaults (id, hubId, createdAt) and broadcasts
    /// to all subscribed browsers.
    LuaPushRequest {
        /// Notification payload from Lua (must include at least `kind`).
        payload: serde_json::Value,
    },

    /// Browser push-notification control routed through Lua command dispatch.
    ///
    /// Sent by Lua's `push.control(peer_id, command)` from hub command
    /// handlers. Rust still owns push subscription persistence and VAPID
    /// mechanics, but browser-originated commands pass through `client.lua`
    /// first like other hub UI commands.
    BrowserPushControl {
        browser_identity: String,
        payload: serde_json::Value,
    },

    /// Stale push subscriptions to remove (410 Gone from push service).
    ///
    /// Sent from the async web push broadcast task when subscriptions expire.
    PushSubscriptionsExpired {
        /// Browser identity keys whose subscriptions returned 410 Gone.
        identities: Vec<String>,
    },

    /// URL readiness probe requested by a plugin completed.
    UrlProbeReady {
        connector_session_uuid: String,
        parent_session_uuid: String,
        url: String,
        ready: bool,
        error: Option<String>,
    },

    /// Plugin command preparation requested from Lua completed.
    PluginCommandPrepared {
        /// Plugin-scoped request token from the original request.
        request_id: String,
        /// Resolved executable path when preparation succeeded.
        command: Option<String>,
        /// Optional config file path written for the command.
        config_path: Option<String>,
        /// Opaque plugin-owned context from the original request.
        context: serde_json::Value,
        /// Stable machine-readable error kind when preparation failed.
        error_kind: Option<String>,
        /// Error message when preparation failed.
        error: Option<String>,
    },

    /// One-shot plugin command gate completed.
    CommandGateCompleted {
        /// Plugin-scoped request token from the original request.
        request_id: String,
        /// Opaque plugin-owned metadata from the original request.
        metadata: serde_json::Value,
        /// Opaque plugin-owned context from the original request.
        context: serde_json::Value,
        /// Whether the command exited successfully before timeout.
        success: bool,
        /// Process exit status when available.
        exit_status: Option<i32>,
        /// Bounded stdout tail.
        stdout_tail: String,
        /// Bounded stderr tail.
        stderr_tail: String,
        /// True when captured output exceeded the tail bound.
        output_truncated: bool,
        /// Stable machine-readable error kind when the gate failed.
        error_kind: Option<String>,
        /// Human-readable error message when the gate failed.
        error: Option<String>,
        /// Elapsed runtime in milliseconds.
        duration_ms: u128,
    },

    // =========================================================================
    // Socket IPC events — Unix domain socket client connections
    // =========================================================================
    /// A new socket client has connected.
    ///
    /// Sent from the socket server accept loop. The Hub stores the connection
    /// and notifies Lua via the socket client_connected callback.
    SocketClientConnected {
        /// Unique identifier for this socket client (e.g., "socket:a1b2c3").
        client_id: String,
        /// Connection handle for sending frames back to this client.
        conn: SocketClientConn,
    },

    /// A socket client has disconnected (EOF or error).
    SocketClientDisconnected {
        /// Client identifier.
        client_id: String,
    },

    /// JSON message from a socket client.
    ///
    /// Routed through Lua's socket message callback, which delegates
    /// to the shared `client.lua` protocol (same as TUI and WebRTC).
    SocketMessage {
        /// Client identifier.
        client_id: String,
        /// JSON message payload.
        msg: serde_json::Value,
    },

    /// Binary PTY input from a socket client.
    ///
    /// Raw keyboard bytes, written directly to the PTY (bypasses Lua).
    SocketPtyInput {
        /// Client identifier (for focus tracking).
        client_id: String,
        /// Session UUID identifying the target PTY.
        session_uuid: String,
        /// Raw input bytes.
        data: Vec<u8>,
    },

    /// Socket send request from a Lua callback.
    ///
    /// Lua's `socket.send(client_id, msg)` pushes this event.
    SocketSend(crate::lua::primitives::socket::SocketSendRequest),

    /// A queued message was successfully delivered to an agent PTY.
    ///
    /// Sent from the message delivery task after probe succeeded and
    /// message was injected. Can be used by Lua for delivery confirmation.
    MessageDelivered {
        /// Length of the delivered message in bytes.
        message_len: usize,
    },

    // =========================================================================
    // Session Process Events
    // =========================================================================
    /// A per-session process has exited or disconnected.
    ///
    /// Sent by the session reader thread when the session socket closes
    /// (child process exited) or the reader detects a `ProcessExited` frame.
    /// The hub routes this to the appropriate session's `PtyEvent` broadcast
    /// channel and notifies Lua via the `process_exited` hook.
    SessionProcessExited {
        /// Session UUID identifying the session.
        session_uuid: String,
        /// Exit code, or `None` if killed by signal or socket EOF.
        exit_code: Option<i32>,
    },

    /// A background reconnect task completed successfully.
    ///
    /// Sent by `spawn_blocking` after `SessionConnection::connect_and_seed()`
    /// succeeds. The hub loop validates the generation, installs the reader
    /// thread, and publishes the new connection into the shared mutex.
    SessionReconnectReady {
        /// Session UUID that reconnected.
        session_uuid: String,
        /// Generation counter to detect stale completions.
        generation: u64,
        /// Fresh connection (reader not yet installed).
        conn: crate::session::connection::SessionConnection,
        /// Mode flags fetched during reconnect handshake.
        mode_flags: Option<crate::session::protocol::ModeFlags>,
    },

    /// A session was removed from `HandleCache` by `hub.unregister_session()`.
    ///
    /// The Hub removes any per-session routing state whose `session_uuid`
    /// matches so in-memory indexes do not grow without bound when sessions cycle.
    SessionUnregistered {
        /// The session UUID that was removed.
        session_uuid: String,
    },

    /// Async worktree deletion completed.
    ///
    /// Sent by the `spawn_blocking` task in the `WorktreeRequest::Delete`
    /// handler after `delete_worktree_by_path` finishes (success or failure).
    /// The main loop removes the worktree from `HandleCache` on success so
    /// `worktree.list()` / `worktree.find()` reflect the deletion immediately.
    WorktreeDeleteCompleted {
        /// Filesystem path of the deleted worktree (retained for logging context).
        path: String,
        /// Branch name that was deleted (for logging).
        branch: String,
        /// `Ok(())` on success, `Err(message)` on failure.
        result: Result<(), String>,
    },

    /// Async WebRTC offer negotiation completed.
    ///
    /// Sent by the spawned runner after SDP negotiation and answer encryption.
    /// The WebRTC registry owns stale generation checks, channel retention, and
    /// cleanup when this completion returns to the hub thread.
    WebRtcOfferNegotiated(crate::worker::webrtc::WebRtcOfferCompletion),

    /// Hub-owned snapshot provider finished a WebRTC recovery request.
    WebRtcRecoverySnapshotReady {
        request: crate::worker::webrtc::WebRtcRecoverySnapshotRequest,
        result: crate::worker::webrtc::WebRtcRecoverySnapshotResult,
    },
}

impl HubEvent {
    #[must_use]
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::HttpResponse(_) => "http_response",
            Self::WebSocketEvent(_) => "websocket_event",
            Self::PtyNotification(_) => "pty_notification",
            Self::PtyOscEvent { event, .. } => match event {
                crate::agent::pty::PtyEvent::TitleChanged(_) => "pty_osc_title",
                crate::agent::pty::PtyEvent::CwdChanged(_) => "pty_osc_cwd",
                crate::agent::pty::PtyEvent::PromptMark(_) => "pty_osc_prompt",
                crate::agent::pty::PtyEvent::CursorVisibilityChanged(_) => "pty_osc_cursor",
                _ => "pty_osc_event",
            },
            Self::PtyProcessExited { .. } => "pty_process_exited",
            Self::PtyOutputObserved { .. } => "pty_output_observed",
            Self::SessionIoBatch(_) => "session_io_batch",
            Self::SessionIo(_) => "session_io",
            Self::DropPendingSessionIoSnapshot { .. } => "drop_pending_session_io_snapshot",
            Self::ClientWorkerControl(_) => "client_worker_control",
            Self::DcOpened { .. } => "dc_opened",
            Self::WebRtcIngressBackpressure { .. } => "webrtc_ingress_backpressure",
            Self::TimerFired { .. } => "timer_fired",
            Self::AcChannelMessage { .. } => "ac_channel_message",
            Self::WebRtcMessage { .. } => "webrtc_message",
            Self::WebRtcPtyInput(_) => "webrtc_pty_input",
            Self::WebRtcFileInput(_) => "webrtc_file_input",
            Self::WebRtcOutgoingSignal(_) => "webrtc_outgoing_signal",
            Self::WebRtcClientWorkerEgress { .. } => "webrtc_client_worker_egress",
            Self::WebRtcStreamFrame(_) => "webrtc_stream_frame",
            Self::UserFileWatch { .. } => "user_file_watch",
            Self::CleanupTick => "cleanup_tick",
            Self::WebRtcSend(_) => "webrtc_send",
            Self::TuiSend(_) => "tui_send",
            Self::LuaPtyRequest(_) => "lua_pty_request",
            Self::LuaHubRequest(_) => "lua_hub_request",
            Self::LuaConnectionRequest(_) => "lua_connection_request",
            Self::LuaWorktreeRequest(_) => "lua_worktree_request",
            Self::LuaActionCableRequest(_) => "lua_action_cable_request",
            Self::LuaHubClientRequest(_) => "lua_hub_client_request",
            Self::HubClientMessage { .. } => "hub_client_message",
            Self::HubClientDisconnected { .. } => "hub_client_disconnected",
            Self::LuaPushRequest { .. } => "lua_push_request",
            Self::BrowserPushControl { .. } => "browser_push_control",
            Self::PushSubscriptionsExpired { .. } => "push_subscriptions_expired",
            Self::UrlProbeReady { .. } => "url_probe_ready",
            Self::PluginCommandPrepared { .. } => "plugin_command_prepared",
            Self::CommandGateCompleted { .. } => "command_gate_completed",
            Self::SocketClientConnected { .. } => "socket_client_connected",
            Self::SocketClientDisconnected { .. } => "socket_client_disconnected",
            Self::SocketMessage { .. } => "socket_message",
            Self::SocketPtyInput { .. } => "socket_pty_input",
            Self::SocketSend(_) => "socket_send",
            Self::MessageDelivered { .. } => "message_delivered",
            Self::SessionProcessExited { .. } => "session_process_exited",
            Self::SessionReconnectReady { .. } => "session_reconnect_ready",
            Self::SessionUnregistered { .. } => "session_unregistered",
            Self::WorktreeDeleteCompleted { .. } => "worktree_delete_completed",
            Self::WebRtcOfferNegotiated(_) => "webrtc_offer_negotiated",
            Self::WebRtcRecoverySnapshotReady { .. } => "webrtc_recovery_snapshot_ready",
        }
    }

    #[must_use]
    pub(crate) fn approx_size_bytes(&self) -> usize {
        const BASE: usize = 32;
        match self {
            Self::WebRtcMessage {
                browser_identity,
                payload,
            } => BASE + browser_identity.len() + payload.len(),
            Self::WebRtcPtyInput(input) => BASE + input.session_uuid.len() + input.data.len(),
            Self::WebRtcFileInput(file) => {
                BASE + file.session_uuid.len() + file.filename.len() + file.data.len()
            }
            Self::WebRtcClientWorkerEgress {
                browser_identity,
                egress,
            } => BASE + browser_identity.len() + format!("{egress:?}").len(),
            Self::WebRtcStreamFrame(frame) => {
                BASE + frame.browser_identity.len() + frame.payload.len()
            }
            Self::SocketPtyInput {
                client_id, data, ..
            } => BASE + client_id.len() + data.len(),
            Self::SocketMessage { client_id, msg } => {
                BASE + client_id.len() + msg.to_string().len()
            }
            Self::HubClientMessage {
                connection_id,
                message,
            } => BASE + connection_id.len() + message.to_string().len(),
            Self::AcChannelMessage {
                channel_id,
                message,
            } => BASE + channel_id.len() + message.to_string().len(),
            Self::PtyOutputObserved { session_uuid, data } => {
                BASE + session_uuid.len() + data.len()
            }
            Self::SessionIoBatch(batch) => {
                BASE + batch.session_uuid.len() + batch.output.as_ref().map_or(0, Vec::len)
            }
            Self::UserFileWatch { watch_id, events } => BASE + watch_id.len() + (events.len() * 48),
            Self::PushSubscriptionsExpired { identities } => {
                BASE + identities
                    .iter()
                    .map(std::string::String::len)
                    .sum::<usize>()
            }
            Self::LuaPushRequest { payload } => BASE + payload.to_string().len(),
            _ => BASE,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct HubEventTypeSnapshot {
    pub enqueue_ok: u64,
    pub enqueue_failed: u64,
    pub dequeue: u64,
    pub pending: usize,
    pub pending_high_water: usize,
    pub bytes_pending: usize,
    pub bytes_high_water: usize,
    pub handler_time_total_ns: u64,
    pub handler_time_max_ns: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct HubEventMetricsSnapshot {
    pub enqueue_ok_total: u64,
    pub enqueue_failed_total: u64,
    pub dequeue_total: u64,
    pub pending_total: usize,
    pub pending_high_water_total: usize,
    pub bytes_pending_total: usize,
    pub bytes_high_water_total: usize,
    pub handler_time_total_ns: u64,
    pub handler_time_max_ns: u64,
    pub by_type: BTreeMap<&'static str, HubEventTypeSnapshot>,
    pub counters: BTreeMap<&'static str, u64>,
    pub spans: BTreeMap<&'static str, HubEventSpanSnapshot>,
    pub slow_samples: Vec<HubEventSlowSample>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct HubEventSpanSnapshot {
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub slow_count: u64,
    pub bytes_total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HubEventSlowSample {
    pub span: &'static str,
    pub elapsed_us: u64,
    pub bytes: usize,
    pub label: String,
}

#[derive(Debug, Default)]
struct HubEventTypeMetrics {
    enqueue_ok: u64,
    enqueue_failed: u64,
    dequeue: u64,
    pending: usize,
    pending_high_water: usize,
    bytes_pending: usize,
    bytes_high_water: usize,
    handler_time_total_ns: u64,
    handler_time_max_ns: u64,
}

#[derive(Debug, Default)]
struct HubEventSpanMetrics {
    count: u64,
    total_ns: u64,
    max_ns: u64,
    slow_count: u64,
    bytes_total: u64,
}

#[derive(Debug, Default)]
pub(crate) struct HubEventMetrics {
    enqueue_ok_total: AtomicU64,
    enqueue_failed_total: AtomicU64,
    dequeue_total: AtomicU64,
    pending_total: AtomicUsize,
    pending_high_water_total: AtomicUsize,
    bytes_pending_total: AtomicUsize,
    bytes_high_water_total: AtomicUsize,
    handler_time_total_ns: AtomicU64,
    handler_time_max_ns: AtomicU64,
    by_type: Mutex<BTreeMap<&'static str, HubEventTypeMetrics>>,
    counters: Mutex<BTreeMap<&'static str, u64>>,
    spans: Mutex<BTreeMap<&'static str, HubEventSpanMetrics>>,
    slow_samples: Mutex<VecDeque<HubEventSlowSample>>,
}

impl HubEventMetrics {
    pub(crate) const SLOW_SAMPLE_LIMIT: usize = 32;
    pub(crate) const SLOW_SAMPLE_LOG_LIMIT: usize = 8;
    const SLOW_LABEL_LIMIT: usize = 24;

    fn bump_high_water(atom: &AtomicUsize, value: usize) {
        let mut current = atom.load(Ordering::Relaxed);
        while value > current {
            match atom.compare_exchange(current, value, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(updated) => current = updated,
            }
        }
    }

    fn bump_high_water_u64(atom: &AtomicU64, value: u64) {
        let mut current = atom.load(Ordering::Relaxed);
        while value > current {
            match atom.compare_exchange(current, value, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(updated) => current = updated,
            }
        }
    }

    pub(crate) fn record_enqueue(&self, kind: &'static str, bytes: usize) {
        self.enqueue_ok_total.fetch_add(1, Ordering::Relaxed);
        let pending = self.pending_total.fetch_add(1, Ordering::Relaxed) + 1;
        Self::bump_high_water(&self.pending_high_water_total, pending);

        let bytes_pending = self.bytes_pending_total.fetch_add(bytes, Ordering::Relaxed) + bytes;
        Self::bump_high_water(&self.bytes_high_water_total, bytes_pending);

        if let Ok(mut map) = self.by_type.lock() {
            let entry = map.entry(kind).or_default();
            entry.enqueue_ok += 1;
            entry.pending += 1;
            entry.pending_high_water = entry.pending_high_water.max(entry.pending);
            entry.bytes_pending += bytes;
            entry.bytes_high_water = entry.bytes_high_water.max(entry.bytes_pending);
        }
    }

    pub(crate) fn record_enqueue_failed(&self, kind: &'static str, bytes: usize) {
        self.enqueue_failed_total.fetch_add(1, Ordering::Relaxed);
        self.pending_total.fetch_sub(1, Ordering::Relaxed);
        self.bytes_pending_total.fetch_sub(bytes, Ordering::Relaxed);

        if let Ok(mut map) = self.by_type.lock() {
            let entry = map.entry(kind).or_default();
            entry.enqueue_failed += 1;
            entry.pending = entry.pending.saturating_sub(1);
            entry.bytes_pending = entry.bytes_pending.saturating_sub(bytes);
        }
    }

    pub(crate) fn record_dequeue(&self, kind: &'static str, bytes: usize) {
        self.dequeue_total.fetch_add(1, Ordering::Relaxed);
        self.pending_total.fetch_sub(1, Ordering::Relaxed);
        self.bytes_pending_total.fetch_sub(bytes, Ordering::Relaxed);

        if let Ok(mut map) = self.by_type.lock() {
            let entry = map.entry(kind).or_default();
            entry.dequeue += 1;
            entry.pending = entry.pending.saturating_sub(1);
            entry.bytes_pending = entry.bytes_pending.saturating_sub(bytes);
        }
    }

    pub(crate) fn record_handler_time(&self, kind: &'static str, elapsed: std::time::Duration) {
        let nanos = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        self.handler_time_total_ns
            .fetch_add(nanos, Ordering::Relaxed);
        Self::bump_high_water_u64(&self.handler_time_max_ns, nanos);

        if let Ok(mut map) = self.by_type.lock() {
            let entry = map.entry(kind).or_default();
            entry.handler_time_total_ns = entry.handler_time_total_ns.saturating_add(nanos);
            entry.handler_time_max_ns = entry.handler_time_max_ns.max(nanos);
        }
    }

    pub(crate) fn record_counter(&self, name: &'static str, amount: u64) {
        if let Ok(mut map) = self.counters.lock() {
            let entry = map.entry(name).or_default();
            *entry = entry.saturating_add(amount);
        }
    }

    pub(crate) fn record_high_water(&self, name: &'static str, value: u64) {
        if let Ok(mut map) = self.counters.lock() {
            let entry = map.entry(name).or_default();
            *entry = (*entry).max(value);
        }
    }

    pub(crate) fn record_span(
        &self,
        span: &'static str,
        elapsed: std::time::Duration,
        bytes: usize,
    ) {
        self.record_span_labeled(span, elapsed, bytes, "");
    }

    pub(crate) fn record_span_labeled(
        &self,
        span: &'static str,
        elapsed: std::time::Duration,
        bytes: usize,
        label: &str,
    ) {
        self.record_span_with_threshold(span, elapsed, bytes, std::time::Duration::MAX, label);
    }

    pub(crate) fn record_span_with_threshold(
        &self,
        span: &'static str,
        elapsed: std::time::Duration,
        bytes: usize,
        slow_threshold: std::time::Duration,
        label: &str,
    ) {
        let nanos = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        let is_slow = elapsed >= slow_threshold;
        if let Ok(mut map) = self.spans.lock() {
            let entry = map.entry(span).or_default();
            entry.count = entry.count.saturating_add(1);
            entry.total_ns = entry.total_ns.saturating_add(nanos);
            entry.max_ns = entry.max_ns.max(nanos);
            entry.bytes_total = entry.bytes_total.saturating_add(bytes as u64);
            if is_slow {
                entry.slow_count = entry.slow_count.saturating_add(1);
            }
        }
        if is_slow {
            self.record_slow_sample(span, elapsed, bytes, label);
        }
    }

    fn record_slow_sample(
        &self,
        span: &'static str,
        elapsed: std::time::Duration,
        bytes: usize,
        label: &str,
    ) {
        let mut capped = label
            .chars()
            .take(Self::SLOW_LABEL_LIMIT)
            .collect::<String>();
        if capped.is_empty() {
            capped = "-".to_string();
        }
        let sample = HubEventSlowSample {
            span,
            elapsed_us: elapsed.as_micros().min(u64::MAX as u128) as u64,
            bytes,
            label: capped,
        };
        if let Ok(mut samples) = self.slow_samples.lock() {
            if samples.len() >= Self::SLOW_SAMPLE_LIMIT {
                samples.pop_front();
            }
            samples.push_back(sample);
        }
    }

    #[must_use]
    pub(crate) fn snapshot(&self) -> HubEventMetricsSnapshot {
        let by_type = if let Ok(map) = self.by_type.lock() {
            map.iter()
                .map(|(k, v)| {
                    (
                        *k,
                        HubEventTypeSnapshot {
                            enqueue_ok: v.enqueue_ok,
                            enqueue_failed: v.enqueue_failed,
                            dequeue: v.dequeue,
                            pending: v.pending,
                            pending_high_water: v.pending_high_water,
                            bytes_pending: v.bytes_pending,
                            bytes_high_water: v.bytes_high_water,
                            handler_time_total_ns: v.handler_time_total_ns,
                            handler_time_max_ns: v.handler_time_max_ns,
                        },
                    )
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        let counters = self
            .counters
            .lock()
            .map(|map| map.clone())
            .unwrap_or_default();
        let spans = self
            .spans
            .lock()
            .map(|map| {
                map.iter()
                    .map(|(k, v)| {
                        (
                            *k,
                            HubEventSpanSnapshot {
                                count: v.count,
                                total_ns: v.total_ns,
                                max_ns: v.max_ns,
                                slow_count: v.slow_count,
                                bytes_total: v.bytes_total,
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut slow_samples = self
            .slow_samples
            .lock()
            .map(|samples| samples.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        slow_samples.sort_by(|a, b| b.elapsed_us.cmp(&a.elapsed_us));
        slow_samples.truncate(Self::SLOW_SAMPLE_LOG_LIMIT);

        HubEventMetricsSnapshot {
            enqueue_ok_total: self.enqueue_ok_total.load(Ordering::Relaxed),
            enqueue_failed_total: self.enqueue_failed_total.load(Ordering::Relaxed),
            dequeue_total: self.dequeue_total.load(Ordering::Relaxed),
            pending_total: self.pending_total.load(Ordering::Relaxed),
            pending_high_water_total: self.pending_high_water_total.load(Ordering::Relaxed),
            bytes_pending_total: self.bytes_pending_total.load(Ordering::Relaxed),
            bytes_high_water_total: self.bytes_high_water_total.load(Ordering::Relaxed),
            handler_time_total_ns: self.handler_time_total_ns.load(Ordering::Relaxed),
            handler_time_max_ns: self.handler_time_max_ns.load(Ordering::Relaxed),
            by_type,
            counters,
            spans,
            slow_samples,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HubEventTx {
    inner: mpsc::UnboundedSender<HubEvent>,
    metrics: Arc<HubEventMetrics>,
}

impl HubEventTx {
    #[must_use]
    pub(crate) fn new(
        inner: mpsc::UnboundedSender<HubEvent>,
        metrics: Arc<HubEventMetrics>,
    ) -> Self {
        Self { inner, metrics }
    }

    pub(crate) fn send(&self, event: HubEvent) -> Result<(), mpsc::error::SendError<HubEvent>> {
        let kind = event.kind();
        let bytes = event.approx_size_bytes();
        self.metrics.record_enqueue(kind, bytes);
        if let Err(e) = self.inner.send(event) {
            self.metrics.record_enqueue_failed(kind, bytes);
            return Err(e);
        }
        Ok(())
    }
}

impl From<mpsc::UnboundedSender<HubEvent>> for HubEventTx {
    fn from(inner: mpsc::UnboundedSender<HubEvent>) -> Self {
        Self::new(inner, Arc::new(HubEventMetrics::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn kind_splits_pty_osc_subtypes() {
        let title = HubEvent::PtyOscEvent {
            session_uuid: "a".to_string(),
            session_name: "s".to_string(),
            event: crate::agent::pty::PtyEvent::title_changed("hello"),
        };
        assert_eq!(title.kind(), "pty_osc_title");

        let cwd = HubEvent::PtyOscEvent {
            session_uuid: "a".to_string(),
            session_name: "s".to_string(),
            event: crate::agent::pty::PtyEvent::cwd_changed("/tmp"),
        };
        assert_eq!(cwd.kind(), "pty_osc_cwd");
    }

    #[test]
    fn metrics_snapshot_includes_handler_timing() {
        let metrics = HubEventMetrics::default();
        metrics.record_enqueue("pty_osc_title", 32);
        metrics.record_dequeue("pty_osc_title", 32);
        metrics.record_handler_time("pty_osc_title", Duration::from_micros(250));

        let snapshot = metrics.snapshot();
        let kind = snapshot.by_type.get("pty_osc_title").unwrap();
        assert_eq!(kind.dequeue, 1);
        assert_eq!(kind.handler_time_total_ns, 250_000);
        assert_eq!(kind.handler_time_max_ns, 250_000);
        assert_eq!(snapshot.handler_time_total_ns, 250_000);
        assert_eq!(snapshot.handler_time_max_ns, 250_000);
    }

    #[test]
    fn metrics_snapshot_includes_counters_spans_and_bounded_slow_samples() {
        let metrics = HubEventMetrics::default();
        metrics.record_counter("webrtc_send.queued", 2);
        metrics.record_counter("webrtc_send.queued", 3);
        metrics.record_high_water("pty_output.batch_hwm", 5);
        metrics.record_high_water("pty_output.batch_hwm", 3);

        metrics.record_span("webrtc_message.dc_ping", Duration::from_micros(200), 64);
        for i in 0..40 {
            metrics.record_span_with_threshold(
                "socket_message.lua",
                Duration::from_millis(60 + i),
                i as usize,
                Duration::from_millis(50),
                "peer-abcdefghijklmnopqrstuvwxyz",
            );
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.counters["webrtc_send.queued"], 5);
        assert_eq!(snapshot.counters["pty_output.batch_hwm"], 5);
        let span = snapshot.spans.get("socket_message.lua").unwrap();
        assert_eq!(span.count, 40);
        assert_eq!(span.slow_count, 40);
        assert_eq!(
            snapshot.slow_samples.len(),
            HubEventMetrics::SLOW_SAMPLE_LOG_LIMIT
        );
        assert!(snapshot
            .slow_samples
            .iter()
            .all(|sample| sample.label.chars().count() <= 24));
        assert_eq!(snapshot.slow_samples[0].elapsed_us, 99_000);
    }
}
