//! Terminal panel state machine for PTY connections.
//!
//! Each `TerminalPanel` directly owns a vt100 parser and tracks its
//! connection lifecycle: `Idle` (not subscribed), `Connecting`
//! (subscribe sent, awaiting scrollback), and `Connected` (receiving
//! live data).
//!
//! The parser is owned without `Arc<Mutex<>>` — the TUI thread is the
//! sole accessor, so no synchronization is needed.
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

use vt100::Parser;

use crate::agent::spawn::contains_clear_scrollback;

/// Default scrollback buffer size in lines.
///
/// Intentionally larger than `SHADOW_SCROLLBACK_LINES` (5000) because
/// the TUI user can scroll back interactively, while the shadow screen
/// only needs enough history for reconnect snapshots.
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
/// Encapsulates parser lifecycle, dimensions, scroll state, and
/// subscription management. The parser is directly owned (no mutex) —
/// the TUI thread is the sole accessor.
///
/// Scroll offset and scrollback depth are tracked externally rather
/// than relying on vt100's internal scrollback position, eliminating
/// the `set_scrollback(usize::MAX)` hack that caused mid-render
/// mutations.
///
/// Methods return `Option<serde_json::Value>` messages for the caller
/// to send via the transport channel.
pub struct TerminalPanel {
    parser: Parser,
    state: PanelState,
    dims: (u16, u16),
    /// Lines scrolled up from live view. Zero means at bottom (live).
    scroll_offset: usize,
    /// Total scrollback lines available in the buffer.
    scrollback_depth: usize,
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
            parser: Parser::new(rows, cols, DEFAULT_SCROLLBACK),
            state: PanelState::Idle,
            dims: (rows, cols),
            scroll_offset: 0,
            scrollback_depth: 0,
        }
    }

    /// Borrow the parser's screen for rendering.
    ///
    /// Returns the vt100 screen with the correct scrollback offset
    /// already applied — callers can render directly without mutations.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
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
        // Replace the parser entirely so the old scrollback buffer is discarded.
        // Escape sequences like \x1b[2J only clear the visible screen — they
        // leave the scrollback buffer intact, causing duplication on reconnect.
        let (rows, cols) = self.dims;
        self.parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
        self.parser.process(data);

        // Reset scroll state — reconnect starts at live view
        self.scroll_offset = 0;
        self.measure_scrollback_depth();

        self.state = PanelState::Connected;
    }

    /// Process incremental PTY output.
    ///
    /// Accepted in `Connecting` or `Connected` state. Ignored if `Idle`
    /// because we are not subscribed and data is stale.
    ///
    /// Detects CSI 3 J (clear scrollback) and replaces the parser with a
    /// fresh one seeded with the current visible screen. The vt100 crate
    /// silently ignores this sequence, so without manual handling the
    /// scrollback buffer accumulates duplicate screen content on every
    /// clear cycle.
    pub fn on_output(&mut self, data: &[u8]) {
        if self.state == PanelState::Idle {
            return;
        }

        self.parser.process(data);

        // CSI 3 J = \x1b[3J — "Erase Saved Lines" (clear scrollback).
        // vt100 ignores this, so we replace the parser to discard scrollback.
        if contains_clear_scrollback(data) {
            let (rows, cols) = self.parser.screen().size();
            let visible = self.parser.screen().contents_formatted();
            self.parser = Parser::new(rows, cols, DEFAULT_SCROLLBACK);
            self.parser.process(&visible);
            self.scroll_offset = 0;
            self.scrollback_depth = 0;
            return;
        }

        self.measure_scrollback_depth();
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
        self.parser.screen_mut().set_size(rows, cols);

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

    // === Scroll State ===

    /// Scroll up into history by `lines` lines.
    pub fn scroll_up(&mut self, lines: usize) {
        if lines == 0 {
            return;
        }
        self.scroll_offset = self.scroll_offset.saturating_add(lines).min(self.scrollback_depth);
        self.parser.screen_mut().set_scrollback(self.scroll_offset);
    }

    /// Scroll down toward live view by `lines` lines.
    pub fn scroll_down(&mut self, lines: usize) {
        if lines == 0 || self.scroll_offset == 0 {
            return;
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        self.parser.screen_mut().set_scrollback(self.scroll_offset);
    }

    /// Jump to the top of the scrollback buffer.
    pub fn scroll_to_top(&mut self) {
        if self.scrollback_depth == 0 {
            return;
        }
        self.scroll_offset = self.scrollback_depth;
        self.parser.screen_mut().set_scrollback(self.scroll_offset);
    }

    /// Jump to the bottom (return to live view).
    pub fn scroll_to_bottom(&mut self) {
        if self.scroll_offset == 0 {
            return;
        }
        self.scroll_offset = 0;
        self.parser.screen_mut().set_scrollback(0);
    }

    /// Whether the panel is scrolled up from live view.
    pub fn is_scrolled(&self) -> bool {
        self.scroll_offset > 0
    }

    /// Current scroll offset (lines up from bottom).
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Total scrollback lines available.
    pub fn scrollback_depth(&self) -> usize {
        self.scrollback_depth
    }

    /// Measure the actual scrollback depth from the parser.
    ///
    /// Uses the `set_scrollback(usize::MAX)` trick once, then restores
    /// the real offset. Called after `process()` so we know the true
    /// buffer depth.
    fn measure_scrollback_depth(&mut self) {
        self.parser.screen_mut().set_scrollback(usize::MAX);
        self.scrollback_depth = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(self.scroll_offset);
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

        let cell = panel.screen().cell(0, 0).unwrap();
        assert_eq!(cell.contents(), "H");
    }

    #[test]
    fn on_output_ignored_when_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.on_output(b"should be ignored");

        let cell = panel.screen().cell(0, 0).unwrap();
        assert!(!cell.has_contents());
    }

    #[test]
    fn on_output_accepted_when_connecting() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);
        panel.on_output(b"data");

        let cell = panel.screen().cell(0, 0).unwrap();
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

        let cell = panel.screen().cell(0, 0).unwrap();
        assert_eq!(cell.contents(), "n");
    }

    #[test]
    fn on_scrollback_ignored_when_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.on_scrollback(b"should be ignored");
        assert_eq!(panel.state(), PanelState::Idle);

        let cell = panel.screen().cell(0, 0).unwrap();
        assert!(!cell.has_contents());
    }

    #[test]
    fn on_output_clears_scrollback_on_csi_3j() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);

        // Write enough lines to create scrollback.
        for i in 0..30 {
            panel.on_output(format!("line {i}\r\n").as_bytes());
        }

        // Panel tracks scrollback depth externally.
        assert!(panel.scrollback_depth() > 0, "should have scrollback before clear");

        // Send CSI 3 J (clear scrollback).
        panel.on_output(b"\x1b[3J");

        assert_eq!(
            panel.scrollback_depth(),
            0,
            "scrollback depth should be zero after CSI 3 J"
        );
        assert_eq!(panel.scroll_offset(), 0, "scroll offset should reset on clear");
    }

    #[test]
    fn on_output_preserves_visible_screen_after_csi_3j() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);

        // Write visible content then clear scrollback.
        panel.on_output(b"visible text");
        panel.on_output(b"\x1b[3J");

        let cell = panel.screen().cell(0, 0).unwrap();
        assert_eq!(cell.contents(), "v", "visible screen should survive clear");
    }

    #[test]
    fn debug_impl_does_not_leak_parser_contents() {
        let panel = TerminalPanel::new(24, 80);
        let debug = format!("{panel:?}");
        assert!(debug.contains("TerminalPanel"));
        assert!(debug.contains("Idle"));
    }

    #[test]
    fn scroll_up_and_down() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);

        // Write enough lines to create scrollback.
        for i in 0..50 {
            panel.on_output(format!("line {i}\r\n").as_bytes());
        }
        assert!(panel.scrollback_depth() > 0);

        panel.scroll_up(10);
        assert!(panel.is_scrolled());
        assert_eq!(panel.scroll_offset(), 10);

        panel.scroll_down(5);
        assert_eq!(panel.scroll_offset(), 5);

        panel.scroll_to_bottom();
        assert!(!panel.is_scrolled());
        assert_eq!(panel.scroll_offset(), 0);
    }

    #[test]
    fn scroll_to_top_clamps_to_depth() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);

        for i in 0..50 {
            panel.on_output(format!("line {i}\r\n").as_bytes());
        }
        let depth = panel.scrollback_depth();

        panel.scroll_to_top();
        assert_eq!(panel.scroll_offset(), depth);
    }

    #[test]
    fn scroll_up_clamped_to_scrollback_depth() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);

        for i in 0..30 {
            panel.on_output(format!("line {i}\r\n").as_bytes());
        }
        let depth = panel.scrollback_depth();

        // Try to scroll way past the buffer
        panel.scroll_up(usize::MAX);
        assert_eq!(panel.scroll_offset(), depth);
    }

    #[test]
    fn scroll_down_does_not_go_negative() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);
        panel.scroll_down(100);
        assert_eq!(panel.scroll_offset(), 0);
    }

    #[test]
    fn scrollback_resets_scroll_on_reconnect() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);

        for i in 0..30 {
            panel.on_output(format!("line {i}\r\n").as_bytes());
        }
        panel.scroll_up(10);
        assert!(panel.is_scrolled());

        // Reconnect resets scroll
        panel.on_scrollback(b"fresh snapshot");
        assert!(!panel.is_scrolled());
        assert_eq!(panel.scroll_offset(), 0);
    }
}
