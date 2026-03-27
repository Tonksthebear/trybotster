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
//!   ├── label() → &str             (display label)
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
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};

use tokio::sync::broadcast;

use crate::agent::pty::{PtyEvent, SharedPtyState};
#[cfg(test)]
use crate::agent::pty::do_resize;
use crate::terminal::TerminalParser;

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
/// for addressing this session throughout the system. The `label` is
/// a human-readable display name (e.g., "owner-repo-42").
#[derive(Debug, Clone)]
pub struct SessionHandle {
    /// Session UUID — primary key for all addressing.
    session_uuid: String,

    /// Human-readable display label (e.g., "owner-repo-42").
    pub(crate) label: String,

    /// Whether this is an agent or accessory session.
    session_type: SessionType,

    /// Optional workspace identifier for grouping sessions.
    pub(crate) workspace_id: Option<String>,

    /// Single PTY handle for this session.
    pty: PtyHandle,
}

impl SessionHandle {
    /// Create a new session handle.
    ///
    /// # Arguments
    ///
    /// * `session_uuid` - Stable UUID (e.g., "sess-1234567890-abcdef")
    /// * `label` - Human-readable display label
    /// * `session_type` - Agent or Accessory
    /// * `workspace_id` - Optional workspace for grouping
    /// * `pty` - Single PTY handle
    #[must_use]
    pub fn new(
        session_uuid: impl Into<String>,
        label: impl Into<String>,
        session_type: SessionType,
        workspace_id: Option<String>,
        pty: PtyHandle,
    ) -> Self {
        Self {
            session_uuid: session_uuid.into(),
            label: label.into(),
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
    pub fn label(&self) -> &str {
        &self.label
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
    /// `Some` for local/test PTY handles (no session process).
    /// `None` for session-backed handles — snapshots come via RPC to the session process.
    shadow_screen: Option<Arc<Mutex<TerminalParser>>>,

    /// Whether the inner PTY has kitty keyboard protocol active.
    ///
    /// Updated by session reader from `FRAME_MODE_CHANGED` events.
    /// Read by `snapshot_and_subscribe()` for reconnect metadata.
    kitty_enabled: Arc<AtomicBool>,

    /// Whether the terminal cursor is currently visible (DECTCEM).
    ///
    /// Updated by session reader from `FRAME_MODE_CHANGED` events.
    /// Readable without an RPC.
    cursor_visible: Arc<AtomicBool>,

    /// Whether a resize happened without the application redrawing yet.
    ///
    /// Set by `resize_direct()`, checked by `get_snapshot()` to avoid
    /// capturing stale visible-screen content after a resize.
    resize_pending: Arc<AtomicBool>,

    /// Epoch milliseconds of the last PTY output chunk.
    ///
    /// Updated by the session reader thread on each output delivery.
    /// Read by Lua's `session:last_output_at()` for idle detection.
    last_output_at: Arc<AtomicU64>,

    /// HTTP forwarding port for preview proxying.
    ///
    /// Used by accessory sessions running dev servers to expose the port
    /// for HTTP preview. `None` for agent sessions or if no port assigned.
    port: Option<u16>,

    /// Per-session process connection for write/resize commands.
    ///
    /// Routes I/O through the session process socket.
    session_connection: Option<crate::session::connection::SharedSessionConnection>,

}

impl std::fmt::Debug for PtyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyHandle")
            .field("port", &self.port)
            .finish()
    }
}

impl PtyHandle {
    /// Apply cached terminal colors to the shadow screen (local handles only).
    pub fn apply_color_cache(
        &self,
        cache: &std::sync::Arc<std::sync::Mutex<std::collections::HashMap<usize, crate::terminal::Rgb>>>,
    ) {
        if let Some(ref screen) = self.shadow_screen {
            if let Ok(mut screen) = screen.lock() {
                screen.apply_color_cache(cache);
            }
        }
    }

    /// Create a new local PTY handle with direct sync access.
    ///
    /// Test-only constructor for in-process PTY fixtures that do not involve
    /// the session socket transport.
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
    /// * `port` - HTTP forwarding port, or `None`
    #[must_use]
    #[cfg(test)]
    pub fn new(
        event_tx: broadcast::Sender<PtyEvent>,
        shared_state: Arc<Mutex<SharedPtyState>>,
        shadow_screen: Arc<Mutex<TerminalParser>>,
        kitty_enabled: Arc<AtomicBool>,
        cursor_visible: Arc<AtomicBool>,
        resize_pending: Arc<AtomicBool>,
        port: Option<u16>,
    ) -> Self {
        Self {
            event_tx,
            shared_state,
            shadow_screen: Some(shadow_screen),
            kitty_enabled,
            cursor_visible,
            resize_pending,
            last_output_at: Arc::new(AtomicU64::new(0)),
            port,
            session_connection: None,
        }
    }

    /// Create a session-process-backed PTY handle.
    ///
    /// No shadow screen — the session process owns the terminal parser.
    /// Snapshots are fetched via RPC (`FRAME_GET_SNAPSHOT`).
    /// The reader thread broadcasts output and structured events directly.
    #[must_use]
    pub fn new_with_session(
        event_tx: broadcast::Sender<PtyEvent>,
        kitty_enabled: Arc<AtomicBool>,
        cursor_visible: Arc<AtomicBool>,
        resize_pending: Arc<AtomicBool>,
        port: Option<u16>,
        session_connection: crate::session::connection::SharedSessionConnection,
        last_output_at: Arc<AtomicU64>,
        last_human_input_ms: Arc<std::sync::atomic::AtomicI64>,
        initial_rows: u16,
        initial_cols: u16,
    ) -> Self {
        Self {
            event_tx,
            shared_state: Arc::new(Mutex::new(SharedPtyState {
                master_pty: None,
                writer: None,
                dimensions: (initial_rows, initial_cols),
                last_human_input_ms,
            })),
            shadow_screen: None,
            kitty_enabled,
            cursor_visible,
            resize_pending,
            last_output_at,
            port,
            session_connection: Some(session_connection),
        }
    }

    /// Whether this handle is session-process-backed.
    #[must_use]
    pub fn is_session_backed(&self) -> bool {
        self.session_connection.is_some()
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

    /// Subscribe and capture a snapshot for client attach.
    ///
    /// Subscribes first, then generates a snapshot. For session-backed handles,
    /// this is an RPC to the session process. Subscribe-before-snapshot ordering
    /// ensures no output gap: bytes emitted during RPC are captured by the
    /// subscription (and are also in the session's snapshot, so duplicates are
    /// harmless — terminal parsers are idempotent).
    #[must_use]
    pub fn snapshot_and_subscribe(
        &self,
    ) -> (Vec<u8>, bool, u16, u16, broadcast::Receiver<PtyEvent>) {
        let kitty_enabled = self.kitty_enabled.load(Ordering::Relaxed);
        let (rows, cols) = self.dims();

        // Subscribe FIRST so no output is lost during snapshot generation.
        let rx = self.event_tx.subscribe();
        let snapshot = self.get_snapshot();
        (snapshot, kitty_enabled, rows, cols, rx)
    }

    /// Broadcast a `ProcessExited` event on this handle's channel.
    ///
    /// Used by session lifecycle handlers to bridge process exit detection
    /// into the PtyHandle broadcast channel.
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

    /// Get an ANSI snapshot of the current terminal state.
    ///
    /// - **Local handles** (tests): generates from the local shadow screen.
    /// - **Session-backed handles** (production): RPC to the session process.
    #[must_use]
    pub fn get_snapshot(&self) -> Vec<u8> {
        // Local shadow screen path (test-only handles)
        if let Some(ref screen) = self.shadow_screen {
            let has_local_master = self
                .shared_state
                .lock()
                .map(|state| state.master_pty.is_some())
                .unwrap_or(false);
            let skip_visible =
                self.resize_pending.swap(false, Ordering::AcqRel) && has_local_master;

            let parser = screen.lock().expect("shadow_screen lock poisoned");
            return crate::terminal::generate_snapshot(&*parser, skip_visible);
        }

        // Session-process RPC path (production)
        if let Some(ref conn) = self.session_connection {
            if let Ok(mut guard) = conn.lock() {
                if let Some(session) = guard.as_mut() {
                    match session.get_snapshot() {
                        Ok(snapshot) => return snapshot,
                        Err(e) => {
                            log::warn!("Failed to get snapshot via session RPC: {e}");
                        }
                    }
                }
            }
        }

        Vec::new()
    }

    // =========================================================================
    // Direct Sync Methods - Immediate I/O without async channel
    // =========================================================================

    /// Write input directly to the PTY.
    ///
    /// Session-backed handles route writes through the session process socket.
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

        // Session-process path: write directly to session socket
        if let Some(ref conn) = self.session_connection {
            drop(state);
            let mut guard = conn
                .lock()
                .map_err(|_| "session connection lock poisoned".to_string())?;
            let session = guard
                .as_mut()
                .ok_or_else(|| "Session connection not available".to_string())?;
            session
                .write_input(data)
                .map_err(|e| format!("Failed to write PTY input via session: {e}"))?;
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

        Err("session connection not available".to_string())
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

    /// Current PTY dimensions `(rows, cols)`.
    ///
    /// Reads from `SharedPtyState` which is the canonical write target for
    /// resize operations. This avoids depending on the shadow screen.
    #[must_use]
    pub fn dims(&self) -> (u16, u16) {
        self.shared_state
            .lock()
            .map(|s| s.dimensions)
            .unwrap_or((24, 80))
    }

    /// Resize the PTY directly.
    ///
    /// Unconditionally resizes the PTY and shadow screen. Lua is the trusted
    /// coordinator — client-level ownership is managed there, not in the PTY.
    pub fn resize_direct(&self, rows: u16, cols: u16) {
        // Session-process path: send resize to session, resize shadow screen
        // in-place (reflow, don't wipe), update dimensions.
        if let Some(ref conn) = self.session_connection {
            if let Ok(mut guard) = conn.lock() {
                if let Some(session) = guard.as_mut() {
                    if let Err(e) = session.resize(rows, cols) {
                        log::warn!("Failed to resize PTY via session: {e}");
                        return;
                    }
                }
            }
            // Update shared dimensions
            if let Ok(mut state) = self.shared_state.lock() {
                state.dimensions = (rows, cols);
            }
            self.resize_pending.store(true, Ordering::Release);
            let _ = self.event_tx.send(PtyEvent::resized(rows, cols));
            return;
        }

        // Test-only local PTY path (has shadow screen)
        #[cfg(test)]
        {
            if let Some(ref screen) = self.shadow_screen {
                do_resize(
                    rows,
                    cols,
                    &self.shared_state,
                    screen,
                    &self.event_tx,
                    &self.resize_pending,
                );
            }
            return;
        }

        #[cfg(not(test))]
        {
            log::warn!("resize_direct: no session connection available");
        }
    }

    /// Epoch milliseconds of the last PTY output, or 0 if no output yet.
    #[must_use]
    pub fn last_output_at(&self) -> u64 {
        self.last_output_at.load(Ordering::Relaxed)
    }

    /// Shared atomic for last output timestamp.
    ///
    /// Used by `PtySessionHandle` to share the same atomic with Lua.
    #[must_use]
    pub fn last_output_at_atomic(&self) -> &Arc<AtomicU64> {
        &self.last_output_at
    }

    /// Arc accessor for shadow screen (local handles only; None for session-backed).
    #[must_use]
    pub fn shadow_screen(&self) -> Option<Arc<Mutex<TerminalParser>>> {
        self.shadow_screen.as_ref().map(Arc::clone)
    }

    /// Clone of the shadow screen's event listener for the session reader.
    ///
    /// Clone the event broadcast sender.
    #[must_use]
    pub fn event_tx_clone(&self) -> broadcast::Sender<PtyEvent> {
        self.event_tx.clone()
    }

    /// Arc accessor for kitty_enabled flag.
    #[must_use]
    pub fn kitty_enabled_arc(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.kitty_enabled)
    }

    /// Arc accessor for cursor_visible flag.
    #[must_use]
    pub fn cursor_visible_arc(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cursor_visible)
    }

    /// Arc accessor for resize_pending flag.
    #[must_use]
    pub fn resize_pending_arc(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.resize_pending)
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
        let (shared_state, shadow_screen, event_tx, kitty_enabled, cursor_visible, resize_pending) =
            pty_session.get_direct_access();
        // Leak the session to keep the state alive for tests
        std::mem::forget(pty_session);
        PtyHandle::new(
            event_tx,
            shared_state,
            shadow_screen,
            kitty_enabled,
            cursor_visible,
            resize_pending,
            port,
        )
    }

    #[test]
    fn test_session_handle_creation() {
        let pty = create_test_pty();
        let handle =
            SessionHandle::new("sess-1234-abcd", "agent-123", SessionType::Agent, None, pty);

        assert_eq!(handle.session_uuid(), "sess-1234-abcd");
        assert_eq!(handle.label(), "agent-123");
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
