//! Terminal panel state machine for PTY connections.
//!
//! Each `TerminalPanel` owns a [`TerminalParser`] and a [`RenderState`] and
//! tracks its connection lifecycle.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ghostty_vt::RenderState;
use crate::terminal::{CursorStyle, Rgb, TerminalParser};

use super::ColorCache;

/// Default scrollback buffer size in lines for TUI panels.
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

/// Owns a terminal parser, render state, and connection state.
///
/// The parser is directly owned (no mutex) — the TUI thread is the sole accessor.
pub struct TerminalPanel {
    parser: TerminalParser,
    render_state: RenderState,
    color_cache: ColorCache,
    state: PanelState,
    dims: (u16, u16),
    /// Lines scrolled up from live view. Zero means at bottom (live).
    scroll_offset: usize,
    /// Consecutive mouse scroll events in the current tick (for acceleration).
    scroll_accel: u32,
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
        Self::new_with_color_cache(rows, cols, Arc::new(Mutex::new(HashMap::new())))
    }

    /// Create a panel seeded with terminal default colors.
    pub fn new_with_color_cache(rows: u16, cols: u16, color_cache: ColorCache) -> Self {
        let mut parser = TerminalParser::new(rows, cols, TUI_SCROLLBACK);
        parser.apply_color_cache(&color_cache);
        let render_state = RenderState::new().expect("render state creation");
        Self {
            parser,
            render_state,
            color_cache,
            state: PanelState::Idle,
            dims: (rows, cols),
            scroll_offset: 0,
            scroll_accel: 0,
        }
    }

    /// Update the render state from the terminal. Call before each render.
    pub fn update_render_state(&mut self) {
        let _ = self.render_state.update(self.parser.terminal_mut());
    }

    /// Reapply the shared terminal color cache to this panel.
    pub fn refresh_color_cache(&mut self) {
        self.parser.apply_color_cache(&self.color_cache);
        self.update_render_state();
    }

    /// Borrow the render state for widget rendering (immutable).
    pub fn render_state(&self) -> &RenderState {
        &self.render_state
    }

    /// Borrow the render state mutably (for update).
    pub fn render_state_mut(&mut self) -> &mut RenderState {
        &mut self.render_state
    }

    /// Whether focus reporting mode is active.
    pub fn focus_reporting(&self) -> bool {
        self.parser.focus_reporting()
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

    /// Current cursor style from the render state.
    pub fn cursor_style(&self) -> CursorStyle {
        CursorStyle::from_render_state(&self.render_state)
    }

    /// Effective foreground color for the panel terminal.
    pub fn foreground_color(&self) -> Option<Rgb> {
        self.parser.foreground_color()
    }

    /// Default foreground color for the panel terminal.
    pub fn foreground_color_default(&self) -> Option<Rgb> {
        self.parser.foreground_color_default()
    }

    /// Effective background color for the panel terminal.
    pub fn background_color(&self) -> Option<Rgb> {
        self.parser.background_color()
    }

    /// Default background color for the panel terminal.
    pub fn background_color_default(&self) -> Option<Rgb> {
        self.parser.background_color_default()
    }

    /// Extract plain-text grid contents.
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
    pub fn connect(&mut self, session_uuid: &str) -> Option<serde_json::Value> {
        if self.state != PanelState::Idle {
            return None;
        }
        self.state = PanelState::Connecting;
        let (rows, cols) = self.dims;
        let sub_id = sub_id(session_uuid);
        Some(serde_json::json!({
            "type": "subscribe",
            "channel": "terminal",
            "subscriptionId": sub_id,
            "params": {
                "session_uuid": session_uuid,
                "rows": rows,
                "cols": cols,
            }
        }))
    }

    /// Unsubscribe from the PTY, transitioning to `Idle`.
    pub fn disconnect(&mut self, session_uuid: &str) -> Option<serde_json::Value> {
        if self.state == PanelState::Idle {
            return None;
        }
        self.state = PanelState::Idle;
        let sub_id = sub_id(session_uuid);
        Some(serde_json::json!({
            "type": "unsubscribe",
            "subscriptionId": sub_id,
        }))
    }

    /// Mark the transport disconnected without sending an unsubscribe.
    pub fn mark_transport_disconnected(&mut self) {
        self.state = PanelState::Idle;
    }

    /// Process an opaque terminal snapshot, transitioning to `Connected`.
    ///
    /// The data is imported directly into ghostty — no VT replay.
    pub fn on_scrollback(&mut self, data: &[u8]) {
        let (rows, cols) = self.dims;
        self.on_scrollback_with_dims(rows, cols, data);
    }

    /// Process an opaque terminal snapshot with authoritative source dimensions.
    pub fn on_scrollback_with_dims(&mut self, rows: u16, cols: u16, data: &[u8]) {
        self.dims = (rows, cols);

        // Replace the parser entirely so the old scrollback buffer is discarded.
        self.parser = TerminalParser::new(rows, cols, TUI_SCROLLBACK);

        if !data.is_empty() {
            // Single-call import: one opaque blob restores the whole terminal.
            if let Err(e) = self.parser.terminal_mut().snapshot_import(data) {
                log::error!("[terminal_panel] snapshot_import failed: {e}");
                // Don't transition to Connected — snapshot is malformed
                return;
            }
        }

        // Client-local defaults should win over the serialized session
        // defaults so each TUI attach can render with its own theme.
        self.parser.apply_color_cache(&self.color_cache);

        self.update_render_state();

        // Reset scroll state — reconnect starts at live view
        self.scroll_offset = 0;

        self.state = PanelState::Connected;
    }

    /// Process incremental PTY output.
    pub fn on_output(&mut self, data: &[u8]) {
        if self.state == PanelState::Idle {
            return;
        }
        self.parser.process(data);
        self.update_render_state();
    }

    /// Resize the parser and notify the PTY if subscribed.
    pub fn resize(
        &mut self,
        rows: u16,
        cols: u16,
        session_uuid: &str,
    ) -> Option<serde_json::Value> {
        if (rows, cols) == self.dims || rows < 2 || cols == 0 {
            return None;
        }
        self.dims = (rows, cols);
        self.parser.resize(rows, cols);
        self.update_render_state();

        if self.state == PanelState::Idle {
            return None;
        }
        let sub_id = sub_id(session_uuid);
        Some(serde_json::json!({
            "subscriptionId": sub_id,
            "data": { "type": "resize", "rows": rows, "cols": cols }
        }))
    }

    /// Force-clear cached dimensions so the next `resize` call detects a change.
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
        let new_offset = self.scroll_offset.saturating_add(lines).min(depth);
        let delta = new_offset - self.scroll_offset;
        if delta > 0 {
            self.scroll_offset = new_offset;
            self.parser
                .terminal_mut()
                .scroll_viewport_delta(-(delta as isize));
            self.update_render_state();
        }
    }

    /// Scroll down toward live view by `lines` lines.
    pub fn scroll_down(&mut self, lines: usize) {
        if lines == 0 || self.scroll_offset == 0 {
            return;
        }
        let old = self.scroll_offset;
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        let delta = old - self.scroll_offset;
        if delta > 0 {
            self.parser
                .terminal_mut()
                .scroll_viewport_delta(delta as isize);
            self.update_render_state();
        }
    }

    /// Jump to the top of the scrollback buffer.
    pub fn scroll_to_top(&mut self) {
        let depth = self.scrollback_depth();
        if depth == 0 {
            return;
        }
        self.scroll_offset = depth;
        self.parser.terminal_mut().scroll_viewport_top();
        self.update_render_state();
    }

    /// Jump to the bottom (return to live view).
    pub fn scroll_to_bottom(&mut self) {
        if self.scroll_offset == 0 {
            return;
        }
        self.scroll_offset = 0;
        self.parser.terminal_mut().scroll_viewport_bottom();
        self.update_render_state();
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

impl super::smooth_scroll::SmoothScroll for TerminalPanel {
    fn scroll_up(&mut self, lines: usize) {
        self.scroll_up(lines);
    }

    fn scroll_down(&mut self, lines: usize) {
        self.scroll_down(lines);
    }

    fn scroll_accel(&self) -> u32 {
        self.scroll_accel
    }

    fn set_scroll_accel(&mut self, val: u32) {
        self.scroll_accel = val;
    }
}

/// Build the subscription ID string for a session UUID.
fn sub_id(session_uuid: &str) -> String {
    format!("tui:{session_uuid}")
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
        let msg = panel.connect("sess-0");
        assert!(msg.is_some());
        assert_eq!(panel.state(), PanelState::Connecting);

        let msg = msg.unwrap();
        assert_eq!(msg["type"], "subscribe");
        assert_eq!(msg["params"]["session_uuid"], "sess-0");
        assert_eq!(msg["params"]["rows"], 24);
        assert_eq!(msg["params"]["cols"], 80);
    }

    #[test]
    fn connect_is_noop_when_not_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        assert_eq!(panel.state(), PanelState::Connecting);

        let msg = panel.connect("sess-0");
        assert!(msg.is_none());
        assert_eq!(panel.state(), PanelState::Connecting);
    }

    #[test]
    fn on_scrollback_transitions_to_connected() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        // Empty snapshot still transitions to Connected
        panel.on_scrollback(b"");
        assert_eq!(panel.state(), PanelState::Connected);
    }

    #[test]
    fn on_empty_scrollback_transitions_to_connected() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        panel.on_scrollback(b"");
        assert_eq!(panel.state(), PanelState::Connected);
    }

    #[test]
    fn scrollback_import_reapplies_local_default_background() {
        let color_cache = Arc::new(Mutex::new(HashMap::from([(257usize, Rgb::new(1, 2, 3))])));
        let mut panel = TerminalPanel::new_with_color_cache(24, 80, Arc::clone(&color_cache));

        let mut source = crate::terminal::TerminalParser::new(24, 80, TUI_SCROLLBACK);
        source
            .terminal_mut()
            .set_color_background(Rgb::new(240, 241, 242).into());
        let snapshot = source
            .terminal()
            .snapshot_export()
            .expect("source snapshot export");

        panel.connect("sess-0");
        panel.on_scrollback(&snapshot);

        assert_eq!(panel.background_color_default(), Some(Rgb::new(1, 2, 3)));
    }

    #[test]
    fn refresh_color_cache_reapplies_updated_shared_background() {
        let color_cache = Arc::new(Mutex::new(HashMap::from([(257usize, Rgb::new(1, 2, 3))])));
        let mut panel = TerminalPanel::new_with_color_cache(24, 80, Arc::clone(&color_cache));

        assert_eq!(panel.background_color_default(), Some(Rgb::new(1, 2, 3)));

        color_cache
            .lock()
            .expect("color cache lock")
            .insert(257usize, Rgb::new(9, 8, 7));
        panel.refresh_color_cache();

        assert_eq!(panel.background_color_default(), Some(Rgb::new(9, 8, 7)));
    }

    #[test]
    fn on_output_ignored_when_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.on_output(b"should be ignored");

        let contents = panel.contents();
        assert!(!contents.contains("should be ignored"));
    }

    #[test]
    fn on_output_accepted_when_connecting() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        panel.on_output(b"data");

        let contents = panel.contents();
        assert!(contents.contains('d'));
    }

    #[test]
    fn disconnect_transitions_to_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        panel.on_scrollback(b"");
        assert_eq!(panel.state(), PanelState::Connected);

        let msg = panel.disconnect("sess-0");
        assert!(msg.is_some());
        assert_eq!(panel.state(), PanelState::Idle);
        assert_eq!(msg.unwrap()["type"], "unsubscribe");
    }

    #[test]
    fn disconnect_is_noop_when_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        let msg = panel.disconnect("sess-0");
        assert!(msg.is_none());
    }

    #[test]
    fn resize_sends_message_when_subscribed_and_dims_changed() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");

        let msg = panel.resize(30, 100, "sess-0");
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
        let msg = panel.resize(30, 100, "sess-0");
        assert!(msg.is_none());
        assert_eq!(panel.dims(), (30, 100));
    }

    #[test]
    fn resize_no_message_when_dims_unchanged() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        let msg = panel.resize(24, 80, "sess-0");
        assert!(msg.is_none());
    }

    #[test]
    fn resize_rejects_too_small() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");

        assert!(panel.resize(1, 80, "sess-0").is_none());
        assert_eq!(panel.dims(), (24, 80));

        assert!(panel.resize(24, 0, "sess-0").is_none());
        assert_eq!(panel.dims(), (24, 80));
    }

    #[test]
    fn invalidate_dims_forces_next_resize() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        panel.invalidate_dims();

        let msg = panel.resize(24, 80, "sess-0");
        assert!(msg.is_some());
    }

    #[test]
    fn scrollback_clears_parser_before_writing() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        panel.on_output(b"old content");
        // Empty binary snapshot replaces parser, clearing old content
        panel.on_scrollback(b"");

        let contents = panel.contents();
        assert!(!contents.contains("old content"));
    }

    #[test]
    fn on_scrollback_connects_from_idle() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.on_scrollback(b"");
        assert_eq!(panel.state(), PanelState::Connected);
    }

    #[test]
    fn on_scrollback_with_invalid_blob_stays_not_connected() {
        // An invalid snapshot blob (wrong version, random bytes) should
        // cause snapshot_import to fail and NOT transition to Connected.
        let invalid_blob = vec![0xFF, 0x00, 0x01, 0x02];

        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");
        panel.on_scrollback(&invalid_blob);

        // snapshot_import fails on bad version → stays in Connecting
        assert_eq!(panel.state(), PanelState::Connecting);
    }

    #[test]
    fn on_output_clears_scrollback_on_csi_3j() {
        let mut panel = TerminalPanel::new(24, 80);
        panel.connect("sess-0");

        for i in 0..30 {
            panel.on_output(format!("line {i}\r\n").as_bytes());
        }

        assert!(
            panel.scrollback_depth() > 0,
            "should have scrollback before clear"
        );

        panel.on_output(b"\x1b[3J");

        assert_eq!(
            panel.scrollback_depth(),
            0,
            "CSI 3J should clear scrollback"
        );
    }
}
