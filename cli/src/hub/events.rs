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
use std::collections::BTreeMap;
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

    /// Stale push subscriptions to remove (410 Gone from push service).
    ///
    /// Sent from the async web push broadcast task when subscriptions expire.
    PushSubscriptionsExpired {
        /// Browser identity keys whose subscriptions returned 410 Gone.
        identities: Vec<String>,
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


    /// A session was removed from `HandleCache` by `hub.unregister_session()`.
    ///
    /// The Hub removes all `broker_sessions` entries whose `session_uuid` matches
    /// so the routing table does not grow without bound when sessions cycle.
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

    /// Async WebRTC offer handling completed — SDP answer is ready.
    ///
    /// Sent by the spawned task in `handle_webrtc_offer` after ICE config
    /// fetch, SDP negotiation, and answer encryption complete. The main loop
    /// re-inserts the channel and relays the encrypted answer via Lua.
    WebRtcOfferCompleted {
        /// Browser identity for channel re-insertion and signal routing.
        browser_identity: String,
        /// Offer generation captured when async processing started.
        offer_generation: u64,
        /// The WebRTC channel to re-insert into the HashMap.
        channel: crate::channel::WebRtcChannel,
        /// Encrypted answer envelope, ready for Lua relay. `None` on failure.
        encrypted_answer: Option<serde_json::Value>,
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
            Self::DcOpened { .. } => "dc_opened",
            Self::WebRtcIngressBackpressure { .. } => "webrtc_ingress_backpressure",
            Self::TimerFired { .. } => "timer_fired",
            Self::AcChannelMessage { .. } => "ac_channel_message",
            Self::WebRtcMessage { .. } => "webrtc_message",
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
            Self::PushSubscriptionsExpired { .. } => "push_subscriptions_expired",
            Self::SocketClientConnected { .. } => "socket_client_connected",
            Self::SocketClientDisconnected { .. } => "socket_client_disconnected",
            Self::SocketMessage { .. } => "socket_message",
            Self::SocketPtyInput { .. } => "socket_pty_input",
            Self::SocketSend(_) => "socket_send",
            Self::MessageDelivered { .. } => "message_delivered",
            Self::SessionProcessExited { .. } => "session_process_exited",
            Self::SessionUnregistered { .. } => "session_unregistered",
            Self::WorktreeDeleteCompleted { .. } => "worktree_delete_completed",
            Self::WebRtcOfferCompleted { .. } => "webrtc_offer_completed",
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
}

impl HubEventMetrics {
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
}
