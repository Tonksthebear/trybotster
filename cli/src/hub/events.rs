//! Unified event channel for the Hub event loop.
//!
//! All background producers (HTTP threads, WebSocket threads, tokio tasks,
//! PTY watchers, timers, forwarding tasks) send events through a single
//! `mpsc::UnboundedSender<HubEvent>`. The `select!` loop receives on the
//! corresponding receiver and dispatches via `handle_hub_event()`.

// Rust guideline compliant 2026-02

use crate::file_watcher::FileEvent;
use crate::lua::primitives::connection::ConnectionRequest;
use crate::lua::primitives::http::CompletedHttpResponse;
use crate::lua::primitives::hub::HubRequest;
use crate::lua::primitives::pty::PtyRequest;
use crate::lua::primitives::tui::TuiSendRequest;
use crate::lua::primitives::webrtc::WebRtcSendRequest;
use crate::lua::primitives::websocket::WsEvent;
use crate::lua::primitives::action_cable::ActionCableRequest;
use crate::lua::primitives::worktree::WorktreeRequest;

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

    /// PTY process exited (reader thread detected EOF).
    ///
    /// Sent from the notification watcher task when it receives
    /// `PtyEvent::ProcessExited`. Triggers Lua `process_exited` event
    /// which updates agent status and broadcasts to all clients.
    PtyProcessExited {
        /// Agent key identifying which agent's PTY exited.
        agent_key: String,
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

    /// Lua hot-reload file change from a blocking forwarder task.
    ///
    /// A single forwarder reads from the `LuaFileWatcher`'s receiver
    /// and sends module names that need reloading.
    LuaFileChange {
        /// Module names in dot notation (e.g. `"hub.handlers.foo"`).
        modules: Vec<String>,
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
}

