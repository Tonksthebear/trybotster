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

use std::io::Write;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use tokio::sync::broadcast;

use crate::agent::pty::{do_resize, HubEventListener, PtyEvent, SharedPtyState};
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
    /// Create a new PTY handle with direct sync access.
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
            // Start as Some(true): alacritty initializes with SHOW_CURSOR set, matching
            // spawn_reader_thread initialization to avoid a spurious
            // CursorVisibilityChanged(true) on the very first broker output delivery.
            last_cursor_visible: Arc::new(Mutex::new(Some(true))),
            port,
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
        let parser = self
            .shadow_screen
            .lock()
            .expect("shadow_screen lock poisoned");
        let skip_visible = self.resize_pending.swap(false, Ordering::AcqRel);
        // generate_ansi_snapshot includes kitty restore and alt-screen entry
        // sequences automatically — no manual appends needed here.
        generate_ansi_snapshot(&*parser, skip_visible)
    }

    // =========================================================================
    // Direct Sync Methods - Immediate I/O without async channel
    // =========================================================================

    /// Write input directly to the PTY.
    ///
    /// Locks the shared state mutex and writes directly to the PTY writer.
    /// This is the fastest path for sending input - no async channel hop.
    ///
    /// # Errors
    ///
    /// Returns an error if write fails.
    pub fn write_input_direct(&self, data: &[u8]) -> Result<(), String> {
        let mut state = self.shared_state
            .lock()
            .map_err(|_| "shared_state lock poisoned")?;

        // Stamp human activity for message delivery deferral.
        // Skip non-keyboard sequences (focus in/out) — a user clicking
        // through tabs to watch shouldn't defer message delivery.
        let is_keyboard = data != b"\x1b[I" && data != b"\x1b[O";
        if is_keyboard {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            state.last_human_input_ms.store(now, std::sync::atomic::Ordering::Relaxed);
        }

        if let Some(writer) = &mut state.writer {
            writer
                .write_all(data)
                .map_err(|e| format!("Failed to write PTY input: {e}"))?;
            writer
                .flush()
                .map_err(|e| format!("Failed to flush PTY writer: {e}"))?;
            Ok(())
        } else {
            Err("PTY writer not available".to_string())
        }
    }

    /// Resize the PTY directly.
    ///
    /// Unconditionally resizes the PTY and shadow screen. Lua is the trusted
    /// coordinator — client-level ownership is managed there, not in the PTY.
    pub fn resize_direct(&self, rows: u16, cols: u16) {
        do_resize(rows, cols, &self.shared_state, &self.shadow_screen, &self.event_tx, &self.resize_pending);
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

    /// Helper to create a PTY handle for testing (no port).
    fn create_test_pty() -> PtyHandle {
        create_test_pty_with_port(None)
    }

    /// Helper to create a PTY handle for testing with a specific port.
    fn create_test_pty_with_port(port: Option<u16>) -> PtyHandle {
        let pty_session = PtySession::new(24, 80);
        let (shared_state, shadow_screen, event_tx, kitty_enabled, resize_pending) = pty_session.get_direct_access();
        // Leak the session to keep the state alive for tests
        std::mem::forget(pty_session);
        // detect_notifs=true: test PTYs use agent session behavior
        PtyHandle::new(event_tx, shared_state, shadow_screen, kitty_enabled, resize_pending, true, port)
    }

    #[test]
    fn test_session_handle_creation() {
        let pty = create_test_pty();
        let handle = SessionHandle::new(
            "sess-1234-abcd",
            "agent-123",
            SessionType::Agent,
            None,
            pty,
        );

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
        let handle = SessionHandle::new(
            "sess-1234-abcd",
            "agent-123",
            SessionType::Agent,
            None,
            pty,
        );

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
}
