//! Session handle for client-to-PTY access.
//!
//! `SessionHandle` provides thread-safe access to a session's single PTY.
//! It is a lightweight wrapper around a PTY handle -- session metadata
//! (repo, issue, status, etc.) is managed by Lua, not Rust.
//!
//! # How to Get Session Handles
//!
//! Clients use `HandleCache::get_session()` to read session handles directly.
//! This is non-blocking and safe from any thread:
//!
//! ```text
//! HandleCache::get_session(uuid) → Option<SessionHandle>
//! ```
//!
//! # Hierarchy
//!
//! ```text
//! SessionHandle
//!   ├── session_uuid() → &str     (primary key)
//!   ├── agent_key() → &str        (display label)
//!   ├── session_type() → SessionType
//!   └── pty() → &PtyHandle        (single PTY)
//!
//! PtyHandle
//!   ├── subscribe() → broadcast::Receiver<PtyEvent>
//!   ├── write_input(data) → sends input to PTY
//!   └── resize(rows, cols) → resizes PTY
//! ```
//!
//! Each session is exactly one PTY. No pty_index anywhere.
//! Clients call `pty.write_input()` directly rather than through Hub.

// Rust guideline compliant 2026-03

#[cfg(test)]
use std::io::Write;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use alacritty_terminal::grid::Dimensions;
use tokio::sync::broadcast;

use crate::agent::pty::{do_resize, HubEventListener, PtyEvent, SharedPtyState};
use crate::broker::SharedBrokerConnection;
use crate::terminal::{generate_ansi_snapshot, AlacrittyParser};

/// Session type distinguishing agents (AI-driven) from accessories (plain PTY).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SessionType {
    /// AI-driven agent session (Claude, Codex, etc.)
    #[default]
    Agent,
    /// Accessory session (Rails server, REPL, shell, log tail).
    Accessory,
}

impl std::fmt::Display for SessionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Agent => write!(f, "agent"),
            Self::Accessory => write!(f, "accessory"),
        }
    }
}

/// Handle for interacting with a session's single PTY.
///
/// Clients obtain this via `HandleCache::get_session()`, which reads
/// directly from the cache (non-blocking, safe from any thread).
///
/// Session metadata (repo, issue, status, etc.) is managed by Lua.
/// This handle only provides PTY access for I/O operations.
///
/// # Thread Safety
///
/// `SessionHandle` is `Clone` + `Send` + `Sync`, allowing it to be passed
/// across threads and shared between async tasks.
///
/// # Session UUID
///
/// The `session_uuid` (format: "sess-{timestamp}-{hex}") is the primary key
/// for addressing this session throughout the system. The `agent_key` is
/// retained as a human-readable display label.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    /// Session UUID — primary key for all addressing.
    session_uuid: String,

    /// Human-readable display label (e.g., "owner-repo-42").
    agent_key: String,

    /// Whether this is an agent or accessory session.
    session_type: SessionType,

    /// Optional workspace identifier for grouping sessions.
    workspace_id: Option<String>,

    /// Single PTY handle for this session.
    pty: PtyHandle,
}

impl SessionHandle {
    /// Create a new session handle.
    ///
    /// # Arguments
    ///
    /// * `session_uuid` - Stable UUID (e.g., "sess-1234567890-abcdef")
    /// * `agent_key` - Human-readable display label
    /// * `session_type` - Agent or Accessory
    /// * `workspace_id` - Optional workspace for grouping
    /// * `pty` - Single PTY handle
    #[must_use]
    pub fn new(
        session_uuid: impl Into<String>,
        agent_key: impl Into<String>,
        session_type: SessionType,
        workspace_id: Option<String>,
        pty: PtyHandle,
    ) -> Self {
        Self {
            session_uuid: session_uuid.into(),
            agent_key: agent_key.into(),
            session_type,
            workspace_id,
            pty,
        }
    }

    /// Get the session UUID (primary key).
    #[must_use]
    pub fn session_uuid(&self) -> &str {
        &self.session_uuid
    }

    /// Get the human-readable display label.
    #[must_use]
    pub fn agent_key(&self) -> &str {
        &self.agent_key
    }

    /// Get the session type.
    #[must_use]
    pub fn session_type(&self) -> SessionType {
        self.session_type
    }

    /// Get the optional workspace ID.
    #[must_use]
    pub fn workspace_id(&self) -> Option<&str> {
        self.workspace_id.as_deref()
    }

    /// Get the PTY handle.
    #[must_use]
    pub fn pty(&self) -> &PtyHandle {
        &self.pty
    }
}

/// Handle for interacting with a PTY session.
///
/// Provides both event subscription and direct PTY interaction:
/// - `subscribe()` to receive PTY events (output, resize, exit)
/// - `write_input()` to send input to the PTY
/// - `resize()` to notify PTY of client resize
/// - `get_snapshot()` to get clean ANSI snapshot for reconnect
/// - `port()` to get the HTTP forwarding port (if assigned)
///
/// # Example
///
/// ```ignore
/// let handle = handle_cache.get_session("sess-abc123").unwrap();
/// let pty = handle.pty();
///
/// // Subscribe to output events
/// let mut rx = pty.subscribe();
///
/// // Send input directly to PTY
/// pty.write_input_direct(b"ls -la\n")?;
///
/// while let Ok(event) = rx.recv().await {
///     match event {
///         PtyEvent::Output(data) => process_output(&data),
///         PtyEvent::Resized { rows, cols } => update_size(rows, cols),
///         // ...
///     }
/// }
/// ```
#[derive(Clone)]
pub struct PtyHandle {
    /// Broadcast sender for PTY events.
    ///
    /// Clients subscribe via `subscribe()` to receive events.
    event_tx: broadcast::Sender<PtyEvent>,

    /// Direct access to shared PTY state for sync I/O.
    ///
    /// Enables immediate input/connect/resize without async channel hop.
    shared_state: Arc<Mutex<SharedPtyState>>,

    /// Shadow terminal for clean ANSI snapshots on reconnect.
    ///
    /// Shared with `PtySession` and the reader thread. On connect,
    /// `get_snapshot()` produces clean ANSI output with correct cursor and SGR state.
    shadow_screen: Arc<Mutex<AlacrittyParser<HubEventListener>>>,

    /// Whether the inner PTY has kitty keyboard protocol active.
    ///
    /// Shared with the reader thread which updates this from raw PTY output.
    /// Used by `get_snapshot()` to include the kitty push sequence.
    kitty_enabled: Arc<AtomicBool>,

    /// Whether a resize happened without the application redrawing yet.
    ///
    /// Set by `resize_direct()`, checked by `get_snapshot()` to avoid
    /// capturing stale visible-screen content after a resize.
    resize_pending: Arc<AtomicBool>,

    /// Whether OSC notification sequences should be detected and broadcast.
    ///
    /// `true` for agent PTYs, `false` for accessory PTYs.
    /// Passed directly to [`crate::agent::spawn::process_pty_bytes`] in
    /// `feed_broker_output()`.
    detect_notifs: bool,

    /// Persistent cursor visibility state for transition-only event emission.
    ///
    /// [`crate::agent::spawn::process_pty_bytes`] only fires
    /// [`PtyEvent::CursorVisibilityChanged`] when the state changes. The
    /// reader thread maintains this as a local variable; the broker path
    /// stores it here so it persists across `feed_broker_output()` calls.
    ///
    /// `Arc<Mutex<_>>` because `PtyHandle` is `Clone` and all clones must
    /// share the same state.
    last_cursor_visible: Arc<Mutex<Option<bool>>>,

    /// HTTP forwarding port for preview proxying.
    ///
    /// Used by accessory sessions running dev servers to expose the port
    /// for HTTP preview. `None` for agent sessions or if no port assigned.
    port: Option<u16>,

    /// Broker relay metadata for routing control and snapshot calls.
    ///
    /// Runtime handles must always be broker-backed. `None` exists only for
    /// test-only local PTY fixtures.
    broker_relay: Option<BrokerRelay>,
}

#[derive(Clone)]
struct BrokerRelay {
    session_id: u32,
    connection: SharedBrokerConnection,
}

impl std::fmt::Debug for PtyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyHandle")
            .field("detect_notifs", &self.detect_notifs)
            .field("port", &self.port)
            .finish()
    }
}

impl PtyHandle {
    /// Generate a snapshot from the local shadow screen only.
    ///
    /// This never performs a broker round-trip and is safe to use in UI attach
    /// flows where responsiveness is more important than re-fetching broker
    /// ring-buffer state.
    #[must_use]
    pub fn get_snapshot_cached(&self) -> Vec<u8> {
        #[cfg(not(test))]
        if self.broker_relay.is_none() {
            log::error!("get_snapshot_cached invariant violated: missing broker relay");
            return Vec::new();
        }

        // `resize_pending` means the PTY was resized and we might not have a redraw yet.
        // Test-only local PTY fixtures preserve the old behavior (blank visible grid)
        // after resize. Broker-backed sessions keep visible rows to preserve recovered
        // scrollback on attach/reconnect.
        let has_local_master = self
            .shared_state
            .lock()
            .map(|state| state.master_pty.is_some())
            .unwrap_or(false);
        let skip_visible = self.resize_pending.swap(false, Ordering::AcqRel) && has_local_master;

        let parser = self
            .shadow_screen
            .lock()
            .expect("shadow_screen lock poisoned");
        generate_ansi_snapshot(&*parser, skip_visible)
    }

    /// Create a new local PTY handle with direct sync access.
    ///
    /// Test-only constructor for in-process PTY fixtures that do not involve
    /// the broker transport.
    ///
    /// Direct access enables immediate I/O operations without async channel delays:
    /// - `write_input_direct()` - sync input, no channel hop
    /// - `resize_direct()` - sync resize, also resizes shadow screen
    /// - `get_snapshot()` - clean ANSI snapshot for reconnect
    ///
    /// # Arguments
    ///
    /// * `event_tx` - Broadcast sender for PTY events
    /// * `shared_state` - Direct access to PTY writer and state
    /// * `shadow_screen` - Shadow terminal for ANSI snapshots
    /// * `kitty_enabled` - Shared kitty keyboard protocol state flag
    /// * `resize_pending` - Cleared when output arrives after a resize
    /// * `detect_notifs` - Enable OSC notification detection
    /// * `port` - HTTP forwarding port, or `None`
    #[must_use]
    #[cfg(test)]
    pub fn new(
        event_tx: broadcast::Sender<PtyEvent>,
        shared_state: Arc<Mutex<SharedPtyState>>,
        shadow_screen: Arc<Mutex<AlacrittyParser<HubEventListener>>>,
        kitty_enabled: Arc<AtomicBool>,
        resize_pending: Arc<AtomicBool>,
        detect_notifs: bool,
        port: Option<u16>,
    ) -> Self {
        Self {
            event_tx,
            shared_state,
            shadow_screen,
            kitty_enabled,
            resize_pending,
            detect_notifs,
            port,
            last_cursor_visible: Arc::new(Mutex::new(Some(true))),
            broker_relay: None,
        }
    }

    /// Create a broker-backed PTY handle.
    #[must_use]
    pub fn new_with_broker_relay(
        event_tx: broadcast::Sender<PtyEvent>,
        shared_state: Arc<Mutex<SharedPtyState>>,
        shadow_screen: Arc<Mutex<AlacrittyParser<HubEventListener>>>,
        kitty_enabled: Arc<AtomicBool>,
        resize_pending: Arc<AtomicBool>,
        detect_notifs: bool,
        port: Option<u16>,
        broker_relay: (u32, SharedBrokerConnection),
    ) -> Self {
        let (session_id, connection) = broker_relay;
        Self {
            event_tx,
            shared_state,
            shadow_screen,
            kitty_enabled,
            resize_pending,
            detect_notifs,
            // Start as Some(true): alacritty initializes with SHOW_CURSOR set, matching
            // spawn_reader_thread initialization to avoid a spurious
            // CursorVisibilityChanged(true) on the very first broker output delivery.
            last_cursor_visible: Arc::new(Mutex::new(Some(true))),
            port,
            broker_relay: Some(BrokerRelay {
                session_id,
                connection,
            }),
        }
    }

    /// Subscribe to PTY events.
    ///
    /// Returns a receiver that will receive all PTY events:
    /// - `Output(Vec<u8>)` - Terminal output data
    /// - `Resized { rows, cols }` - PTY was resized
    /// - `ProcessExited { exit_code }` - PTY process exited
    ///
    /// # Lagging
    ///
    /// If the receiver falls behind, it will receive
    /// `broadcast::error::RecvError::Lagged(n)` indicating how many
    /// events were missed. Handle this gracefully (e.g., request redraw).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<PtyEvent> {
        self.event_tx.subscribe()
    }

    /// Atomically capture a cached snapshot and install an event subscription.
    ///
    /// This prevents reconnect races where output emitted during snapshot capture
    /// is replayed twice (once in snapshot, once via buffered stream), or missed
    /// between separate snapshot/subscribe calls.
    #[must_use]
    pub fn snapshot_and_subscribe_cached(
        &self,
    ) -> (Vec<u8>, bool, u16, u16, broadcast::Receiver<PtyEvent>) {
        // Broker-backed sessions are broker-authoritative for snapshot bytes.
        if let Some(relay) = &self.broker_relay {
            let (kitty_enabled, rows, cols) = {
                let parser = self
                    .shadow_screen
                    .lock()
                    .expect("shadow_screen lock poisoned");
                let kitty_enabled = parser.kitty_enabled();
                let rows = parser.term().grid().screen_lines() as u16;
                let cols = parser.term().grid().columns() as u16;
                (kitty_enabled, rows, cols)
            };

            let snapshot = match relay.connection.lock() {
                Ok(mut guard) => {
                    if let Some(conn) = guard.as_mut() {
                        match conn.get_snapshot(relay.session_id) {
                            Ok(snapshot) => snapshot,
                            Err(e) => {
                                log::error!(
                                    "Broker-authoritative snapshot failed in snapshot_and_subscribe_cached: {e}"
                                );
                                Vec::new()
                            }
                        }
                    } else {
                        log::error!(
                            "Broker-authoritative snapshot failed in snapshot_and_subscribe_cached: broker connection not available"
                        );
                        Vec::new()
                    }
                }
                Err(_) => {
                    log::error!(
                        "Broker-authoritative snapshot failed in snapshot_and_subscribe_cached: broker connection lock poisoned"
                    );
                    Vec::new()
                }
            };

            let rx = self.event_tx.subscribe();
            return (snapshot, kitty_enabled, rows, cols, rx);
        }

        let parser = self
            .shadow_screen
            .lock()
            .expect("shadow_screen lock poisoned");
        let kitty_enabled = parser.kitty_enabled();
        let rows = parser.term().grid().screen_lines() as u16;
        let cols = parser.term().grid().columns() as u16;
        let rx = self.event_tx.subscribe();

        #[cfg(test)]
        {
            let skip_visible = self.resize_pending.swap(false, Ordering::AcqRel);
            let snapshot = generate_ansi_snapshot(&*parser, skip_visible);
            return (snapshot, kitty_enabled, rows, cols, rx);
        }

        #[cfg(not(test))]
        {
            log::error!("snapshot_and_subscribe_cached invariant violated: missing broker relay");
            return (Vec::new(), kitty_enabled, rows, cols, rx);
        }
    }

    /// Broadcast a `ProcessExited` event on this handle's channel.
    ///
    /// Used by the `BrokerPtyExited` handler to bridge broker-side exit
    /// detection into the PtyHandle broadcast channel, so the notification
    /// watcher fires `process_exited` for live broker-owned sessions.
    pub fn notify_process_exited(&self, exit_code: Option<i32>) {
        let _ = self.event_tx.send(PtyEvent::process_exited(exit_code));
    }

    /// Get the HTTP forwarding port for this PTY.
    ///
    /// Returns the port allocated for HTTP preview proxying, or `None` if
    /// no port has been assigned.
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Get a clean ANSI snapshot of the current terminal state.
    ///
    /// Whether the inner PTY has kitty keyboard protocol active.
    #[must_use]
    pub fn kitty_enabled(&self) -> bool {
        self.kitty_enabled.load(Ordering::Relaxed)
    }

    /// Locks the shadow screen and delegates to
    /// [`generate_ansi_snapshot`](crate::terminal::generate_ansi_snapshot).
    ///
    /// Alt-screen entry, kitty keyboard restore, and SGR state are all handled
    /// inside `generate_ansi_snapshot` — no post-processing needed here.
    ///
    /// When a resize is pending (shadow screen resized but app hasn't redrawn),
    /// the snapshot clears the screen; the app redraws on SIGWINCH.
    #[must_use]
    pub fn get_snapshot(&self) -> Vec<u8> {
        if let Some(relay) = &self.broker_relay {
            match relay.connection.lock() {
                Ok(mut guard) => {
                    if let Some(conn) = guard.as_mut() {
                        match conn.get_snapshot(relay.session_id) {
                            Ok(snapshot) => return snapshot,
                            Err(e) => {
                                log::error!("Broker-authoritative snapshot failed: {e}");
                                return Vec::new();
                            }
                        }
                    }
                    log::error!(
                        "Broker-authoritative snapshot failed: broker connection not available"
                    );
                    return Vec::new();
                }
                Err(_) => {
                    log::error!(
                        "Broker-authoritative snapshot failed: broker connection lock poisoned"
                    );
                    return Vec::new();
                }
            }
        }

        #[cfg(test)]
        {
            return self.get_snapshot_cached();
        }

        #[cfg(not(test))]
        {
            log::error!("get_snapshot invariant violated: missing broker relay");
            Vec::new()
        }
    }

    // =========================================================================
    // Direct Sync Methods - Immediate I/O without async channel
    // =========================================================================

    /// Write input directly to the PTY.
    ///
    /// Broker-backed sessions always route writes through the broker control
    /// connection.
    ///
    /// # Errors
    ///
    /// Returns an error if write fails.
    pub fn write_input_direct(&self, data: &[u8]) -> Result<(), String> {
        let state = self
            .shared_state
            .lock()
            .map_err(|_| "shared_state lock poisoned")?;
        #[cfg(test)]
        let mut state = state;
        #[cfg(not(test))]
        let state = state;

        // Stamp human activity for message delivery deferral.
        // Skip non-keyboard sequences (focus in/out) — a user clicking
        // through tabs to watch shouldn't defer message delivery.
        let is_keyboard = data != b"\x1b[I" && data != b"\x1b[O";
        if is_keyboard {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            state
                .last_human_input_ms
                .store(now, std::sync::atomic::Ordering::Relaxed);
        }

        if let Some(relay) = &self.broker_relay {
            drop(state);
            let mut guard = relay
                .connection
                .lock()
                .map_err(|_| "broker connection lock poisoned".to_string())?;
            let conn = guard
                .as_mut()
                .ok_or_else(|| "Broker connection not available".to_string())?;
            conn.write_pty_input(relay.session_id, data)
                .map_err(|e| format!("Failed to write PTY input via broker: {e}"))?;
            return Ok(());
        }

        #[cfg(test)]
        if let Some(writer) = &mut state.writer {
            writer
                .write_all(data)
                .map_err(|e| format!("Failed to write PTY input: {e}"))?;
            writer
                .flush()
                .map_err(|e| format!("Failed to flush PTY writer: {e}"))?;
            return Ok(());
        }

        Err("broker relay not available for session".to_string())
    }

    /// Read the last human input timestamp (milliseconds since epoch).
    ///
    /// Returns 0 if no keyboard input has been received.
    #[must_use]
    pub fn last_human_input_ms(&self) -> i64 {
        let state = self
            .shared_state
            .lock()
            .expect("shared_state lock poisoned");
        state
            .last_human_input_ms
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether this handle is broker-backed.
    #[must_use]
    pub fn is_broker_backed(&self) -> bool {
        self.broker_relay.is_some()
    }

    /// Current shadow-screen dimensions `(rows, cols)`.
    #[must_use]
    pub fn dims(&self) -> (u16, u16) {
        let parser = self
            .shadow_screen
            .lock()
            .expect("shadow_screen lock poisoned");
        (
            parser.term().grid().screen_lines() as u16,
            parser.term().grid().columns() as u16,
        )
    }

    /// Resize the PTY directly.
    ///
    /// Unconditionally resizes the PTY and shadow screen. Lua is the trusted
    /// coordinator — client-level ownership is managed there, not in the PTY.
    pub fn resize_direct(&self, rows: u16, cols: u16) {
        let Some(relay) = &self.broker_relay else {
            #[cfg(test)]
            {
                do_resize(
                    rows,
                    cols,
                    &self.shared_state,
                    &self.shadow_screen,
                    &self.event_tx,
                    &self.resize_pending,
                );
                return;
            }

            #[cfg(not(test))]
            {
                log::warn!("resize_direct invariant violated: missing broker relay");
                return;
            }
        };

        let mut guard = match relay.connection.lock() {
            Ok(guard) => guard,
            Err(_) => {
                log::warn!("Failed to resize PTY via broker: broker connection lock poisoned");
                return;
            }
        };
        let Some(conn) = guard.as_mut() else {
            log::warn!("Failed to resize PTY via broker: broker connection not available");
            return;
        };
        if let Err(e) = conn.resize_pty(relay.session_id, rows, cols) {
            log::warn!("Failed to resize PTY via broker: {e}");
            return;
        }

        do_resize(
            rows,
            cols,
            &self.shared_state,
            &self.shadow_screen,
            &self.event_tx,
            &self.resize_pending,
        );
    }

    // =========================================================================
    // Broker relay injection
    // =========================================================================

    /// Inject PTY output received from the broker into this handle.
    ///
    /// Called by `Hub::handle_hub_event` when a `HubEvent::BrokerPtyOutput`
    /// arrives for this session. Delegates entirely to
    /// [`crate::agent::spawn::process_pty_bytes`], the canonical byte
    /// processor, guaranteeing identical events and shadow-screen state
    /// regardless of the originating source.
    pub fn feed_broker_output(&self, data: &[u8]) {
        let mut lcv = self
            .last_cursor_visible
            .lock()
            .expect("last_cursor_visible lock poisoned");
        crate::agent::spawn::process_pty_bytes(
            data,
            &self.shadow_screen,
            &self.event_tx,
            &self.kitty_enabled,
            &self.resize_pending,
            self.detect_notifs,
            &mut lcv,
            "Broker",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::pty::PtySession;
    use crate::broker::connection::BrokerConnection;
    use crate::broker::protocol::{
        encode_data, frame_type, BrokerFrame, BrokerFrameDecoder, HubMessage,
    };
    use std::io::{Error, ErrorKind, Result as IoResult};
    use std::os::unix::net::UnixStream;

    /// Helper to create a PTY handle for testing (no port).
    fn create_test_pty() -> PtyHandle {
        create_test_pty_with_port(None)
    }

    /// Helper to create a PTY handle for testing with a specific port.
    fn create_test_pty_with_port(port: Option<u16>) -> PtyHandle {
        let pty_session = PtySession::new(24, 80);
        let (shared_state, shadow_screen, event_tx, kitty_enabled, resize_pending) =
            pty_session.get_direct_access();
        // Leak the session to keep the state alive for tests
        std::mem::forget(pty_session);
        // detect_notifs=true: test PTYs use agent session behavior
        PtyHandle::new(
            event_tx,
            shared_state,
            shadow_screen,
            kitty_enabled,
            resize_pending,
            true,
            port,
        )
    }

    fn create_broker_backed_test_pty() -> (PtyHandle, UnixStream) {
        let pty_session = PtySession::new(24, 80);
        let (shared_state, shadow_screen, event_tx, kitty_enabled, resize_pending) =
            pty_session.get_direct_access();
        std::mem::forget(pty_session);

        let (client_stream, server_stream) = UnixStream::pair().expect("UnixStream::pair");
        let conn = BrokerConnection::from_stream(client_stream);
        let shared = Arc::new(Mutex::new(Some(conn)));

        let handle = PtyHandle::new_with_broker_relay(
            event_tx,
            shared_state,
            shadow_screen,
            kitty_enabled,
            resize_pending,
            true,
            None,
            (42, shared),
        );

        (handle, server_stream)
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> IoResult<usize> {
            Err(Error::new(
                ErrorKind::Other,
                "local writer should not be used for broker-backed session",
            ))
        }

        fn flush(&mut self) -> IoResult<()> {
            Ok(())
        }
    }

    #[test]
    fn test_session_handle_creation() {
        let pty = create_test_pty();
        let handle =
            SessionHandle::new("sess-1234-abcd", "agent-123", SessionType::Agent, None, pty);

        assert_eq!(handle.session_uuid(), "sess-1234-abcd");
        assert_eq!(handle.agent_key(), "agent-123");
        assert_eq!(handle.session_type(), SessionType::Agent);
        assert!(handle.workspace_id().is_none());
    }

    #[test]
    fn test_session_handle_with_workspace() {
        let pty = create_test_pty();
        let handle = SessionHandle::new(
            "sess-5678-ef01",
            "agent-456",
            SessionType::Accessory,
            Some("ws-1".to_string()),
            pty,
        );

        assert_eq!(handle.session_type(), SessionType::Accessory);
        assert_eq!(handle.workspace_id(), Some("ws-1"));
    }

    #[test]
    fn test_session_handle_pty_access() {
        let pty = create_test_pty();
        let handle =
            SessionHandle::new("sess-1234-abcd", "agent-123", SessionType::Agent, None, pty);

        // PTY is always accessible
        assert!(handle.pty().port().is_none());
    }

    #[test]
    fn test_pty_handle_port() {
        // Without port
        let handle = create_test_pty();
        assert!(handle.port().is_none());

        // With port
        let handle_with_port = create_test_pty_with_port(Some(8080));
        assert_eq!(handle_with_port.port(), Some(8080));
    }

    #[test]
    fn test_session_type_display() {
        assert_eq!(SessionType::Agent.to_string(), "agent");
        assert_eq!(SessionType::Accessory.to_string(), "accessory");
    }

    #[test]
    fn test_session_type_default() {
        assert_eq!(SessionType::default(), SessionType::Agent);
    }

    #[test]
    fn test_write_input_direct_routes_via_broker_for_broker_backed_sessions() {
        let (pty, mut server_stream) = create_broker_backed_test_pty();

        pty.write_input_direct(b"hello")
            .expect("broker write should succeed");

        let mut buf = [0u8; 128];
        let n = std::io::Read::read(&mut server_stream, &mut buf).expect("read broker frame");
        let frames = BrokerFrameDecoder::new()
            .feed(&buf[..n])
            .expect("decode broker frame");
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            BrokerFrame::PtyInput(session_id, data) => {
                assert_eq!(*session_id, 42);
                assert_eq!(data, b"hello");
            }
            other => panic!("expected PtyInput frame, got {other:?}"),
        }
    }

    #[test]
    fn test_write_input_direct_prefers_broker_even_when_local_writer_exists() {
        let (pty, mut server_stream) = create_broker_backed_test_pty();
        {
            let mut state = pty.shared_state.lock().expect("shared_state lock poisoned");
            state.writer = Some(Box::new(FailingWriter));
        }

        pty.write_input_direct(b"broker-only")
            .expect("broker write should succeed without touching local writer");

        let mut buf = [0u8; 128];
        let n = std::io::Read::read(&mut server_stream, &mut buf).expect("read broker frame");
        let frames = BrokerFrameDecoder::new()
            .feed(&buf[..n])
            .expect("decode broker frame");
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            BrokerFrame::PtyInput(session_id, data) => {
                assert_eq!(*session_id, 42);
                assert_eq!(data, b"broker-only");
            }
            other => panic!("expected PtyInput frame, got {other:?}"),
        }
    }

    #[test]
    fn test_resize_direct_routes_via_broker_for_broker_backed_sessions() {
        let (pty, mut server_stream) = create_broker_backed_test_pty();

        pty.resize_direct(40, 120);

        let mut buf = [0u8; 128];
        let n = std::io::Read::read(&mut server_stream, &mut buf).expect("read broker frame");
        let frames = BrokerFrameDecoder::new()
            .feed(&buf[..n])
            .expect("decode broker frame");
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            BrokerFrame::HubControl(HubMessage::ResizePty {
                session_id,
                rows,
                cols,
            }) => {
                assert_eq!(*session_id, 42);
                assert_eq!((*rows, *cols), (40, 120));
            }
            other => panic!("expected ResizePty frame, got {other:?}"),
        }
    }

    #[test]
    fn test_get_snapshot_prefers_broker_scrollback_for_broker_backed_sessions() {
        let (pty, mut server_stream) = create_broker_backed_test_pty();

        let broker = std::thread::spawn(move || {
            use std::io::{Read, Write};

            let mut buf = [0u8; 128];
            let n = server_stream.read(&mut buf).expect("read broker frame");
            let frames = BrokerFrameDecoder::new()
                .feed(&buf[..n])
                .expect("decode broker frame");
            assert_eq!(frames.len(), 1);
            assert!(matches!(
                &frames[0],
                BrokerFrame::HubControl(HubMessage::GetSnapshot { session_id: 42 })
            ));

            let response = encode_data(frame_type::SNAPSHOT, 42, b"full scrollback");
            server_stream
                .write_all(&response)
                .expect("write broker snapshot");
        });

        let snapshot = pty.get_snapshot();
        assert_eq!(snapshot, b"full scrollback");

        broker.join().expect("broker thread should finish");
    }

    #[test]
    fn test_get_snapshot_cached_uses_local_shadow_screen_for_broker_backed_sessions() {
        let (pty, mut server_stream) = create_broker_backed_test_pty();
        pty.feed_broker_output(b"cached-output\n");

        let snapshot = pty.get_snapshot_cached();
        assert!(
            String::from_utf8_lossy(&snapshot).contains("cached-output"),
            "cached snapshot should include locally fed broker output"
        );

        server_stream
            .set_nonblocking(true)
            .expect("set_nonblocking on fake broker");
        let mut buf = [0u8; 16];
        let read_err = std::io::Read::read(&mut server_stream, &mut buf)
            .expect_err("cached snapshot should not issue broker request");
        assert!(
            matches!(
                read_err.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "unexpected fake broker read error: {read_err}"
        );
    }

    #[test]
    fn test_get_snapshot_cached_after_resize_keeps_output_for_broker_backed_sessions() {
        let (pty, mut server_stream) = create_broker_backed_test_pty();

        // Mark resize pending (attach path does this before snapshot).
        pty.resize_direct(40, 120);

        // Drain the broker resize frame from the fake server so we can verify no
        // additional control frames are emitted by cached snapshot generation.
        let mut buf = [0u8; 128];
        let n = std::io::Read::read(&mut server_stream, &mut buf).expect("read broker frame");
        let frames = BrokerFrameDecoder::new()
            .feed(&buf[..n])
            .expect("decode broker frame");
        assert_eq!(frames.len(), 1);
        assert!(matches!(
            &frames[0],
            BrokerFrame::HubControl(HubMessage::ResizePty {
                session_id: 42,
                rows: 40,
                cols: 120
            })
        ));

        pty.feed_broker_output(b"after-resize\n");
        let snapshot = pty.get_snapshot_cached();
        assert!(
            String::from_utf8_lossy(&snapshot).contains("after-resize"),
            "broker-backed cached snapshot should preserve output after resize"
        );
    }

    #[test]
    fn test_snapshot_and_subscribe_cached_prefers_broker_snapshot_for_broker_backed_sessions() {
        let (pty, mut server_stream) = create_broker_backed_test_pty();
        pty.feed_broker_output(b"local-shadow-output\n");

        let broker = std::thread::spawn(move || {
            use std::io::{Read, Write};

            let mut buf = [0u8; 128];
            let n = server_stream.read(&mut buf).expect("read broker frame");
            let frames = BrokerFrameDecoder::new()
                .feed(&buf[..n])
                .expect("decode broker frame");
            assert_eq!(frames.len(), 1);
            assert!(matches!(
                &frames[0],
                BrokerFrame::HubControl(HubMessage::GetSnapshot { session_id: 42 })
            ));

            let response = encode_data(frame_type::SNAPSHOT, 42, b"broker-authoritative-snapshot");
            server_stream
                .write_all(&response)
                .expect("write broker snapshot");
        });

        let (snapshot, _kitty, _rows, _cols, _rx) = pty.snapshot_and_subscribe_cached();
        assert_eq!(snapshot, b"broker-authoritative-snapshot");

        broker.join().expect("broker thread should finish");
    }

    #[test]
    fn test_notify_process_exited_broadcasts_to_subscribers() {
        let pty = create_test_pty();
        let mut rx = pty.subscribe();

        pty.notify_process_exited(Some(42));

        match rx.try_recv() {
            Ok(PtyEvent::ProcessExited { exit_code }) => {
                assert_eq!(exit_code, Some(42));
            }
            other => panic!("expected ProcessExited, got {other:?}"),
        }
    }

    #[test]
    fn test_notify_process_exited_none_exit_code() {
        let pty = create_test_pty();
        let mut rx = pty.subscribe();

        pty.notify_process_exited(None);

        match rx.try_recv() {
            Ok(PtyEvent::ProcessExited { exit_code }) => {
                assert_eq!(exit_code, None);
            }
            other => panic!("expected ProcessExited with None, got {other:?}"),
        }
    }
}
