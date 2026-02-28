//! Terminal panel state machine for PTY connections.
//!
//! Each `TerminalPanel` directly owns an [`AlacrittyParser`] and tracks its
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

use alacritty_terminal::term::Term;

use crate::terminal::{AlacrittyParser, NoopListener};

/// Default scrollback buffer size in lines for TUI panels.
///
/// Intentionally larger than `DEFAULT_SCROLLBACK_LINES` (5000) because
/// the TUI user can scroll back interactively, while the shadow screen
/// only needs enough history for reconnect snapshots.
const TUI_SCROLLBACK: usize = 10_000;

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

/// Owns an alacritty parser and its connection state.
///
/// Encapsulates parser lifecycle, dimensions, scroll state, and
/// subscription management. The parser is directly owned (no mutex) —
/// the TUI thread is the sole accessor.
///
/// Scroll offset is tracked independently of the parser's grid. The
/// rendering code receives `scroll_offset()` as a parameter and indexes
/// into the grid history directly.
///
/// Methods return `Option<serde_json::Value>` messages for the caller
/// to send via the transport channel.
pub struct TerminalPanel {
    parser: AlacrittyParser<NoopListener>,
    state: PanelState,
    dims: (u16, u16),
    /// Lines scrolled up from live view. Zero means at bottom (live).
    scroll_offset: usize,
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
            parser: AlacrittyParser::new_noop(rows, cols, TUI_SCROLLBACK),
            state: PanelState::Idle,
            dims: (rows, cols),
            scroll_offset: 0,
        }
    }

    /// Borrow the underlying terminal for rendering.
    ///
    /// Returns the alacritty `Term` — callers use grid indexing with
    /// `scroll_offset()` to render the correct portion of history.
    pub fn term(&self) -> &Term<NoopListener> {
        self.parser.term()
    }

    /// Whether DECCKM (application cursor keys) mode is active.
    pub fn application_cursor(&self) -> bool {
        self.parser.application_cursor()
    }

    /// Whether bracketed paste mode is active.
    pub fn bracketed_paste(&self) -> bool {
        self.parser.bracketed_paste()
    }

    /// Whether the cursor should be hidden.
    pub fn cursor_hidden(&self) -> bool {
        self.parser.cursor_hidden()
    }

    /// Current cursor style (shape + blink) from the running application.
    ///
    /// Used by [`TerminalModes`] to mirror DECSCUSR to the outer terminal so
    /// the cursor shape (beam/block/underline) is correct when focused.
    pub fn cursor_style(&self) -> alacritty_terminal::vte::ansi::CursorStyle {
        self.parser.cursor_style()
    }

    /// Extract plain-text grid contents (for tests and content checks).
    pub fn contents(&self) -> String {
        self.parser.contents()
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
        let (rows, cols) = self.dims;
        self.parser = AlacrittyParser::new_noop(rows, cols, TUI_SCROLLBACK);
        self.parser.process(data);

        // Reset scroll state — reconnect starts at live view
        self.scroll_offset = 0;

        self.state = PanelState::Connected;
    }

    /// Process incremental PTY output.
    ///
    /// Accepted in `Connecting` or `Connected` state. Ignored if `Idle`
    /// because we are not subscribed and data is stale.
    ///
    /// Unlike the old vt100 implementation, no manual CSI 3J handling is
    /// needed — alacritty_terminal processes "Erase Saved Lines" natively.
    pub fn on_output(&mut self, data: &[u8]) {
        if self.state == PanelState::Idle {
            return;
        }
        self.parser.process(data);
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
        self.parser.resize(rows, cols);

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
        let depth = self.scrollback_depth();
        self.scroll_offset = self.scroll_offset.saturating_add(lines).min(depth);
    }

    /// Scroll down toward live view by `lines` lines.
    pub fn scroll_down(&mut self, lines: usize) {
        if lines == 0 || self.scroll_offset == 0 {
            return;
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    /// Jump to the top of the scrollback buffer.
    pub fn scroll_to_top(&mut self) {
        let depth = self.scrollback_depth();
        if depth == 0 {
            return;
        }
        self.scroll_offset = depth;
    }

    /// Jump to the bottom (return to live view).
    pub fn scroll_to_bottom(&mut self) {
        if self.scroll_offset == 0 {
            return;
        }
        self.scroll_offset = 0;
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
        self.parser.history_size()
    }
}

/// Build the subscription ID string for a `(agent, pty)` pair.
fn sub_id(agent_idx: usize, pty_idx: usize) -> String {
    format!("tui:{agent_idx}:{pty_idx}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line, Point};

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

        let cell = &panel.term().grid()[Point::new(Line(0), Column(0))];
        assert_eq!(cell.c, 'H');
    }

    #[test]
    fn on_output_ignored_when_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.on_output(b"should be ignored");

        let cell = &panel.term().grid()[Point::new(Line(0), Column(0))];
        assert_eq!(cell.c, ' ');
    }

    #[test]
    fn on_output_accepted_when_connecting() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);
        panel.on_output(b"data");

        let cell = &panel.term().grid()[Point::new(Line(0), Column(0))];
        assert_eq!(cell.c, 'd');
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

        let cell = &panel.term().grid()[Point::new(Line(0), Column(0))];
        assert_eq!(cell.c, 'n');
    }

    #[test]
    fn on_scrollback_ignored_when_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.on_scrollback(b"should be ignored");
        assert_eq!(panel.state(), PanelState::Idle);

        let cell = &panel.term().grid()[Point::new(Line(0), Column(0))];
        assert_eq!(cell.c, ' ');
    }

    #[test]
    fn on_output_clears_scrollback_on_csi_3j() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect(0, 0);

        // Write enough lines to create scrollback.
        for i in 0..30 {
            panel.on_output(format!("line {i}\r\n").as_bytes());
        }

        assert!(panel.scrollback_depth() > 0, "should have scrollback before clear");

        // Send CSI 3 J (clear scrollback) — alacritty handles this natively.
        panel.on_output(b"\x1b[3J");

        assert_eq!(
            panel.scrollback_depth(),
            0,
            "scrollback depth should be zero after CSI 3 J"
        );
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
