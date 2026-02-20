//! Terminal panel state machine for PTY connections.
//!
//! Each `TerminalPanel` owns a vt100 parser and tracks its connection
//! lifecycle: `Idle` (not subscribed), `Connecting` (subscribe sent,
//! awaiting scrollback), and `Connected` (receiving live data).
//!
//! The panel returns JSON messages for the caller to send rather than
//! owning the transport channel, keeping it testable and free of
//! borrow conflicts with `TuiRunner`.
//!
//! # State Machine
//!
//! ```text
//! Idle ──connect()──> Connecting ──on_scrollback()──> Connected
//!  ^                       |                              |
//!  └───disconnect()────────┴──────────disconnect()────────┘
//! ```

// Rust guideline compliant 2026-02

use std::sync::{Arc, Mutex};
use vt100::Parser;

/// Default scrollback buffer size in lines.
const DEFAULT_SCROLLBACK: usize = 10_000;

/// Connection lifecycle for a terminal panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelState {
    /// Not subscribed. Parser may contain stale content.
    Idle,
    /// Subscribe sent, waiting for scrollback snapshot.
    Connecting,
    /// Receiving live PTY data.
    Connected,
}

/// Owns a vt100 parser and its connection state.
///
/// Encapsulates parser lifecycle, dimensions, and subscription
/// management that was previously scattered across `TuiRunner` fields.
/// Methods return `Option<serde_json::Value>` messages for the caller
/// to send via the transport channel.
pub struct TerminalPanel {
    parser: Arc<Mutex<Parser>>,
    state: PanelState,
    dims: (u16, u16),
}

impl std::fmt::Debug for TerminalPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalPanel")
            .field("state", &self.state)
            .field("dims", &self.dims)
            .finish_non_exhaustive()
    }
}

impl TerminalPanel {
    /// Create a panel with an empty parser at the given dimensions.
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: Arc::new(Mutex::new(Parser::new(rows, cols, DEFAULT_SCROLLBACK))),
            state: PanelState::Idle,
            dims: (rows, cols),
        }
    }

    /// Shared reference to the parser for rendering.
    pub fn parser(&self) -> &Arc<Mutex<Parser>> {
        &self.parser
    }

    /// Current connection state.
    pub fn state(&self) -> PanelState {
        self.state
    }

    /// Last known dimensions `(rows, cols)`.
    pub fn dims(&self) -> (u16, u16) {
        self.dims
    }

    /// Subscribe to a PTY, transitioning `Idle` to `Connecting`.
    ///
    /// Returns a subscribe JSON message for the caller to send.
    /// No-op if already `Connecting` or `Connected`.
    pub fn connect(&mut self, agent_idx: usize, pty_idx: usize) -> Option<serde_json::Value> {
        if self.state != PanelState::Idle {
            return None;
        }
        self.state = PanelState::Connecting;
        let (rows, cols) = self.dims;
        let sub_id = sub_id(agent_idx, pty_idx);
        Some(serde_json::json!({
            "type": "subscribe",
            "channel": "terminal",
            "subscriptionId": sub_id,
            "params": {
                "agent_index": agent_idx,
                "pty_index": pty_idx,
                "rows": rows,
                "cols": cols,
            }
        }))
    }

    /// Unsubscribe from the PTY, transitioning to `Idle`.
    ///
    /// Returns an unsubscribe JSON message. No-op if already `Idle`.
    pub fn disconnect(&mut self, agent_idx: usize, pty_idx: usize) -> Option<serde_json::Value> {
        if self.state == PanelState::Idle {
            return None;
        }
        self.state = PanelState::Idle;
        let sub_id = sub_id(agent_idx, pty_idx);
        Some(serde_json::json!({
            "type": "unsubscribe",
            "subscriptionId": sub_id,
        }))
    }

    /// Process a scrollback snapshot, transitioning to `Connected`.
    ///
    /// Clears the parser before writing the snapshot so the widget
    /// starts from a clean state. Ignored if `Idle` — a panel must
    /// have been subscribed via `connect()` before scrollback arrives.
    pub fn on_scrollback(&mut self, data: &[u8]) {
        if self.state == PanelState::Idle {
            return;
        }
        let mut p = self.parser.lock().expect("parser lock poisoned");
        p.process(b"\x1b[H\x1b[2J\x1b[0m");
        p.screen_mut().set_scrollback(0);
        p.process(data);
        self.state = PanelState::Connected;
    }

    /// Process incremental PTY output.
    ///
    /// Accepted in `Connecting` or `Connected` state. Ignored if `Idle`
    /// because we are not subscribed and data is stale.
    pub fn on_output(&mut self, data: &[u8]) {
        if self.state == PanelState::Idle {
            return;
        }
        self.parser.lock().expect("parser lock poisoned").process(data);
    }

    /// Resize the parser and notify the PTY if subscribed.
    ///
    /// Returns a resize JSON message when dimensions changed and the
    /// panel is subscribed. No message if `Idle` or dimensions match.
    pub fn resize(
        &mut self,
        rows: u16,
        cols: u16,
        agent_idx: usize,
        pty_idx: usize,
    ) -> Option<serde_json::Value> {
        if (rows, cols) == self.dims || rows < 2 || cols == 0 {
            return None;
        }
        self.dims = (rows, cols);
        self.parser
            .lock()
            .expect("parser lock poisoned")
            .screen_mut()
            .set_size(rows, cols);

        if self.state == PanelState::Idle {
            return None;
        }
        let sub_id = sub_id(agent_idx, pty_idx);
        Some(serde_json::json!({
            "subscriptionId": sub_id,
            "data": { "type": "resize", "rows": rows, "cols": cols }
        }))
    }

    /// Force-clear cached dimensions so the next `resize` call detects a change.
    ///
    /// Used after a terminal resize event to ensure all panels get
    /// resized on the next render pass.
    pub fn invalidate_dims(&mut self) {
        self.dims = (0, 0);
    }
}

/// Build the subscription ID string for a `(agent, pty)` pair.
fn sub_id(agent_idx: usize, pty_idx: usize) -> String {
    format!("tui:{agent_idx}:{pty_idx}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_panel_is_idle() {
        let panel = TerminalPanel::new(24, 80);
        assert_eq!(panel.state(), PanelState::Idle);
        assert_eq!(panel.dims(), (24, 80));
    }

    #[test]
    fn connect_transitions_idle_to_connecting() {
        let mut panel = TerminalPanel::new(24, 80);
        let msg = panel.connect(0, 0);
        assert!(msg.is_some());
        assert_eq!(panel.state(), PanelState::Connecting);

        let msg = msg.unwrap();
        assert_eq!(msg["type"], "subscribe");
        assert_eq!(msg["params"]["agent_index"], 0);
        assert_eq!(msg["params"]["rows"], 24);
        assert_eq!(msg["params"]["cols"], 80);
    }

    #[test]
    fn connect_is_noop_when_not_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);
        assert_eq!(panel.state(), PanelState::Connecting);

        // Second connect is a no-op
        let msg = panel.connect(0, 0);
        assert!(msg.is_none());
        assert_eq!(panel.state(), PanelState::Connecting);
    }

    #[test]
    fn on_scrollback_transitions_to_connected() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);
        panel.on_scrollback(b"Hello, World!");
        assert_eq!(panel.state(), PanelState::Connected);

        let p = panel.parser().lock().unwrap();
        let cell = p.screen().cell(0, 0).unwrap();
        assert_eq!(cell.contents(), "H");
    }

    #[test]
    fn on_output_ignored_when_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.on_output(b"should be ignored");

        let p = panel.parser().lock().unwrap();
        let cell = p.screen().cell(0, 0).unwrap();
        assert!(!cell.has_contents());
    }

    #[test]
    fn on_output_accepted_when_connecting() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);
        panel.on_output(b"data");

        let p = panel.parser().lock().unwrap();
        let cell = p.screen().cell(0, 0).unwrap();
        assert_eq!(cell.contents(), "d");
    }

    #[test]
    fn disconnect_transitions_to_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);
        panel.on_scrollback(b"data");
        assert_eq!(panel.state(), PanelState::Connected);

        let msg = panel.disconnect(0, 0);
        assert!(msg.is_some());
        assert_eq!(panel.state(), PanelState::Idle);
        assert_eq!(msg.unwrap()["type"], "unsubscribe");
    }

    #[test]
    fn disconnect_is_noop_when_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        let msg = panel.disconnect(0, 0);
        assert!(msg.is_none());
    }

    #[test]
    fn resize_sends_message_when_subscribed_and_dims_changed() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);

        let msg = panel.resize(30, 100, 0, 0);
        assert!(msg.is_some());
        assert_eq!(panel.dims(), (30, 100));

        let msg = msg.unwrap();
        assert_eq!(msg["data"]["type"], "resize");
        assert_eq!(msg["data"]["rows"], 30);
        assert_eq!(msg["data"]["cols"], 100);
    }

    #[test]
    fn resize_no_message_when_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        let msg = panel.resize(30, 100, 0, 0);
        assert!(msg.is_none());
        // Dims still update even when idle
        assert_eq!(panel.dims(), (30, 100));
    }

    #[test]
    fn resize_no_message_when_dims_unchanged() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);
        let msg = panel.resize(24, 80, 0, 0);
        assert!(msg.is_none());
    }

    #[test]
    fn resize_rejects_too_small() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);

        // rows < 2
        assert!(panel.resize(1, 80, 0, 0).is_none());
        assert_eq!(panel.dims(), (24, 80));

        // cols == 0
        assert!(panel.resize(24, 0, 0, 0).is_none());
        assert_eq!(panel.dims(), (24, 80));
    }

    #[test]
    fn invalidate_dims_forces_next_resize() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);
        panel.invalidate_dims();

        // Same original dims now detected as changed
        let msg = panel.resize(24, 80, 0, 0);
        assert!(msg.is_some());
    }

    #[test]
    fn scrollback_clears_parser_before_writing() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);
        panel.on_output(b"old content");
        panel.on_scrollback(b"new snapshot");

        let p = panel.parser().lock().unwrap();
        let cell = p.screen().cell(0, 0).unwrap();
        assert_eq!(cell.contents(), "n");
    }

    #[test]
    fn on_scrollback_ignored_when_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.on_scrollback(b"should be ignored");
        assert_eq!(panel.state(), PanelState::Idle);

        let p = panel.parser().lock().unwrap();
        let cell = p.screen().cell(0, 0).unwrap();
        assert!(!cell.has_contents());
    }

    #[test]
    fn debug_impl_does_not_leak_parser_contents() {
        let panel = TerminalPanel::new(24, 80);
        let debug = format!("{panel:?}");
        assert!(debug.contains("TerminalPanel"));
        assert!(debug.contains("Idle"));
    }
}
