//! Panel pool — owns terminal panels and focus state.
//!
//! Manages the collection of [`TerminalPanel`] instances keyed by
//! `session_uuid` and tracks which panel is currently focused.
//! Methods return [`OutMessages`] instead of sending directly, keeping
//! channel I/O in the runner.

// Rust guideline compliant 2026-02

use std::collections::HashMap;

use super::render::WidgetArea;
use super::render_tree::RenderNode;
use super::terminal_panel::{PanelState, TerminalPanel};
use super::ColorCache;

/// Messages to send to Hub, collected and forwarded by the runner.
pub type OutMessages = Vec<serde_json::Value>;

/// Explicitly-targeted PTY input — carries destination session UUID so
/// the runner can send it without relying on implicit focus state.
#[derive(Debug)]
pub struct PtyInput {
    /// Session UUID to route the input to.
    pub session_uuid: String,
    /// Raw bytes (typically synthetic focus escape sequences).
    pub data: &'static [u8],
}

/// Side effects from a focus switch.
///
/// The runner applies these mechanically — no focus logic needed.
/// PTY inputs carry explicit targets so ordering is safe regardless
/// of when `PanelPool` mutated its internal state.
#[derive(Debug, Default)]
pub struct FocusEffects {
    /// PTY inputs in order: focus-out (old PTY), then focus-in (new PTY).
    pub pty_inputs: Vec<PtyInput>,
    /// Hub messages (subscribe/unsubscribe).
    pub messages: OutMessages,
    /// Whether to clear inner kitty state on `TerminalModes`.
    pub clear_kitty: bool,
}

/// Owns terminal panels and tracks which session is focused.
///
/// Pure state machine — never touches channels or stdout. All external
/// effects are returned as [`OutMessages`] for the runner to execute.
#[derive(Debug)]
pub struct PanelPool {
    /// Terminal panels keyed by `session_uuid`.
    pub(super) panels: HashMap<String, TerminalPanel>,
    /// Default terminal colors for new/rebuilt panels.
    color_cache: ColorCache,
    /// Terminal dimensions (rows, cols) — used as default for new panels.
    pub(super) terminal_dims: (u16, u16),
    /// Currently selected agent session key (agent_id).
    pub(super) selected_agent: Option<String>,
    /// Session UUID of the currently focused PTY.
    pub(super) current_session_uuid: Option<String>,
    /// Subscription ID of the focused PTY.
    pub(super) current_terminal_sub_id: Option<String>,
}

impl PanelPool {
    /// Create an empty pool with initial terminal dimensions.
    pub fn new(terminal_dims: (u16, u16)) -> Self {
        Self::new_with_color_cache(
            terminal_dims,
            std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
        )
    }

    /// Create an empty pool with an explicit terminal color cache.
    pub fn new_with_color_cache(terminal_dims: (u16, u16), color_cache: ColorCache) -> Self {
        Self {
            panels: HashMap::new(),
            color_cache,
            terminal_dims,
            selected_agent: None,
            current_session_uuid: None,
            current_terminal_sub_id: None,
        }
    }

    // === Panel Access ===

    /// Get or create a panel for the given session UUID.
    pub fn resolve_panel(&mut self, session_uuid: &str) -> &mut TerminalPanel {
        let (rows, cols) = self.terminal_dims;
        let color_cache = self.color_cache.clone();
        self.panels
            .entry(session_uuid.to_string())
            .or_insert_with(|| TerminalPanel::new_with_color_cache(rows, cols, color_cache))
    }

    /// Immutable reference to the focused panel.
    pub fn focused_panel(&self) -> Option<&TerminalPanel> {
        let uuid = self.current_session_uuid.as_ref()?;
        self.panels.get(uuid)
    }

    /// Mutable reference to the focused panel.
    pub fn focused_panel_mut(&mut self) -> Option<&mut TerminalPanel> {
        let uuid = self.current_session_uuid.as_ref()?;
        self.panels.get_mut(uuid)
    }

    /// The focused panel key, if any.
    pub fn focused_key(&self) -> Option<&str> {
        self.current_session_uuid.as_deref()
    }

    /// Whether a message targets the currently focused panel.
    pub fn is_focused(&self, session_uuid: &str) -> bool {
        self.current_session_uuid.as_deref() == Some(session_uuid)
    }

    /// Direct access to panels map (for rendering).
    pub fn panels(&self) -> &HashMap<String, TerminalPanel> {
        &self.panels
    }

    /// Reapply the shared terminal color cache to all live panels.
    pub fn refresh_panel_colors(&mut self) {
        for panel in self.panels.values_mut() {
            panel.refresh_color_cache();
        }
    }

    /// Check if a panel exists for the given key.
    pub fn contains_key(&self, key: &str) -> bool {
        self.panels.contains_key(key)
    }

    // === Sync ===

    /// Sync PTY subscriptions to match the render tree.
    ///
    /// Connects idle panels for bindings in the tree, disconnects panels
    /// no longer needed. Returns messages for Hub.
    pub fn sync_subscriptions(
        &mut self,
        tree: &RenderNode,
        areas: &HashMap<String, WidgetArea>,
    ) -> OutMessages {
        let mut msgs = OutMessages::new();

        let default_uuid = self.current_session_uuid.clone().unwrap_or_default();

        let desired = super::render_tree::collect_terminal_bindings(tree, &default_uuid);

        // Connect panels for new bindings
        for uuid in &desired {
            let Some(area) = areas.get(uuid) else {
                // Subscription geometry must come from the actual rendered widget
                // area. Skip connect for this frame if the area is missing.
                log::debug!(
                    "sync_subscriptions: skipping connect for {} (no rendered area yet)",
                    uuid
                );
                continue;
            };
            let (rows, cols) = (area.rect.height, area.rect.width);
            let color_cache = self.color_cache.clone();
            let panel = self
                .panels
                .entry(uuid.clone())
                .or_insert_with(|| TerminalPanel::new_with_color_cache(rows, cols, color_cache));

            // Critical on reconnect: stale idle panels may keep old cached dims
            // from the pre-restart frame. Refresh parser/panel dims before
            // connect so subscribe carries current render-area rows/cols.
            if panel.state() == PanelState::Idle {
                let _ = panel.resize(rows, cols, uuid);
            }

            if panel.state() == PanelState::Idle {
                if let Some(msg) = panel.connect(uuid) {
                    msgs.push(msg);
                }
            }
        }

        // Disconnect panels not in desired set (skip the focused panel)
        let focused = self.current_session_uuid.clone().unwrap_or_default();
        let to_disconnect: Vec<String> = self
            .panels
            .keys()
            .filter(|k| !desired.contains(*k) && **k != focused)
            .cloned()
            .collect();

        for uuid in to_disconnect {
            if let Some(panel) = self.panels.get_mut(&uuid) {
                if let Some(msg) = panel.disconnect(&uuid) {
                    msgs.push(msg);
                }
            }
            self.panels.remove(&uuid);
        }

        msgs
    }

    /// Resize panels to match rendered widget areas.
    ///
    /// Returns resize messages for panels whose dimensions changed.
    pub fn sync_widget_dims(&mut self, areas: &HashMap<String, WidgetArea>) -> OutMessages {
        let mut msgs = OutMessages::new();

        for (uuid, area) in areas {
            let (rows, cols) = (area.rect.height, area.rect.width);
            if let Some(panel) = self.panels.get_mut(uuid) {
                if let Some(msg) = panel.resize(rows, cols, uuid) {
                    msgs.push(msg);
                }
            }
        }

        // Remove panels for bindings no longer rendered (except focused)
        let focused = self.current_session_uuid.clone().unwrap_or_default();
        self.panels
            .retain(|k, _| areas.contains_key(k) || *k == focused);

        msgs
    }

    /// Handle terminal resize — invalidate all panel dims.
    pub fn handle_resize(&mut self, rows: u16, cols: u16) {
        self.terminal_dims = (rows, cols);
        for panel in self.panels.values_mut() {
            panel.invalidate_dims();
        }
    }

    /// Disconnect all panels (channel closed).
    pub fn disconnect_all(&mut self) -> OutMessages {
        let msgs: Vec<_> = self
            .panels
            .iter_mut()
            .filter_map(|(uuid, panel)| panel.disconnect(uuid))
            .collect();
        self.panels.clear();
        self.current_terminal_sub_id = None;
        self.current_session_uuid = None;
        self.selected_agent = None;
        msgs
    }

    /// Reset to a fresh attach-like state after bridge reconnect.
    ///
    /// Unlike [`disconnect_all`], this emits no unsubscribe messages because
    /// the old socket is already gone. Clears all panel/session state so the
    /// next render+event cycle rebuilds subscriptions from scratch.
    pub fn reset_for_reattach(&mut self) {
        self.panels.clear();
        self.current_terminal_sub_id = None;
        self.current_session_uuid = None;
        self.selected_agent = None;
    }

    // === Focus ===

    /// Switch focus to a specific agent and session.
    ///
    /// Encapsulates the full focus lifecycle: synthetic focus-out on the
    /// old PTY, disconnect, state mutation, connect, and synthetic focus-in
    /// on the new PTY. All side effects are returned in [`FocusEffects`]
    /// for the runner to apply.
    ///
    /// Pass `agent_id = None` to clear focus entirely.
    pub fn focus_terminal(
        &mut self,
        agent_id: Option<&str>,
        session_uuid: Option<&str>,
        terminal_focused: bool,
    ) -> FocusEffects {
        let mut effects = FocusEffects::default();

        // Clear selection if no agent_id
        let Some(agent_id) = agent_id else {
            if terminal_focused && self.current_session_uuid.is_some() {
                if let Some(uuid) = self.current_session_uuid.clone() {
                    log::debug!("[FOCUS] synthetic focus-out on clear, session={uuid}");
                    effects.pty_inputs.push(PtyInput {
                        session_uuid: uuid,
                        data: b"\x1b[O",
                    });
                }
            }
            if let Some(uuid) = self.current_session_uuid.clone() {
                if let Some(panel) = self.panels.get_mut(&uuid) {
                    if let Some(msg) = panel.disconnect(&uuid) {
                        effects.messages.push(msg);
                    }
                }
            }
            self.selected_agent = None;
            self.current_session_uuid = None;
            self.current_terminal_sub_id = None;
            effects.clear_kitty = true;
            return effects;
        };

        let Some(uuid) = session_uuid else {
            log::warn!("focus_terminal: missing session_uuid for agent {agent_id}");
            return effects;
        };

        if self.current_session_uuid.as_deref() == Some(uuid)
            && self.selected_agent.as_deref() == Some(agent_id)
        {
            if let Some(panel) = self.panels.get_mut(uuid) {
                match panel.state() {
                    PanelState::Connected | PanelState::Connecting => {
                        log::debug!(
                            "focus_terminal: already focused on agent {agent_id} session {uuid} ({:?})",
                            panel.state()
                        );
                        return effects;
                    }
                    PanelState::Idle => {
                        log::debug!(
                            "focus_terminal: re-subscribing focused agent {agent_id} session {uuid} after unavailable terminal state {:?}",
                            panel.state()
                        );
                        panel.mark_transport_disconnected();
                        self.current_terminal_sub_id = Some(format!("tui:{uuid}"));
                        return effects;
                    }
                }
            }
        }

        // Synthetic focus-out BEFORE changing state (targets old PTY)
        if terminal_focused && self.current_session_uuid.is_some() {
            if let Some(old_uuid) = self.current_session_uuid.clone() {
                log::debug!("[FOCUS] synthetic focus-out on switch, old_session={old_uuid}");
                effects.pty_inputs.push(PtyInput {
                    session_uuid: old_uuid,
                    data: b"\x1b[O",
                });
            }
        }

        // Disconnect old panel
        if let Some(old_uuid) = self.current_session_uuid.clone() {
            if let Some(panel) = self.panels.get_mut(&old_uuid) {
                if let Some(msg) = panel.disconnect(&old_uuid) {
                    effects.messages.push(msg);
                }
            }
        }

        // Discard stale panel if agent changed
        if self.selected_agent.as_deref() != Some(agent_id) {
            self.panels.remove(uuid);
        }

        // Get or create panel, inheriting dims from outgoing panel
        let widget_dims = self
            .current_session_uuid
            .as_ref()
            .and_then(|k| self.panels.get(k))
            .map(|p| p.dims())
            .unwrap_or(self.terminal_dims);
        let color_cache = self.color_cache.clone();
        let panel = self
            .panels
            .entry(uuid.to_string())
            .or_insert_with(|| TerminalPanel::new_with_color_cache(widget_dims.0, widget_dims.1, color_cache));
        // Defer subscribe to `sync_subscriptions()`, which runs after render and
        // has accurate widget areas for this session.
        panel.mark_transport_disconnected();

        // Update focus state
        self.selected_agent = Some(agent_id.to_string());
        self.current_session_uuid = Some(uuid.to_string());
        self.current_terminal_sub_id = Some(format!("tui:{uuid}"));

        // Synthetic focus-in AFTER updating state (targets new PTY)
        if terminal_focused {
            log::debug!("[FOCUS] synthetic focus-in on switch, new_session={uuid}");
            effects.pty_inputs.push(PtyInput {
                session_uuid: uuid.to_string(),
                data: b"\x1b[I",
            });
        } else {
            log::debug!("[FOCUS] skipping focus-in on switch (terminal not focused)");
        }

        effects
    }

    // === Accessors ===

    /// Currently selected agent session key.
    pub fn selected_agent(&self) -> Option<&str> {
        self.selected_agent.as_deref()
    }

    /// Session UUID of the currently focused PTY.
    pub fn current_session_uuid(&self) -> Option<&str> {
        self.current_session_uuid.as_deref()
    }

    /// Terminal dimensions (rows, cols).
    pub fn terminal_dims(&self) -> (u16, u16) {
        self.terminal_dims
    }

    /// Subscription ID of the focused PTY.
    pub fn current_terminal_sub_id(&self) -> Option<&str> {
        self.current_terminal_sub_id.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_terminal_panel_shares_pool_color_cache() {
        let color_cache: ColorCache = std::sync::Arc::new(std::sync::Mutex::new(
            HashMap::from([(257usize, crate::terminal::Rgb::new(0xF0, 0xE0, 0xD0))]),
        ));
        let mut pool = PanelPool::new_with_color_cache((24, 80), color_cache.clone());

        let _ = pool.focus_terminal(Some("agent-1"), Some("sess-1"), false);
        let panel = pool.panels.get("sess-1").expect("panel exists after focus");
        assert_eq!(
            panel.background_color_default(),
            Some(crate::terminal::Rgb::new(0xF0, 0xE0, 0xD0)),
            "panel created by focus_terminal must inherit pool color cache"
        );

        // Mutate the shared cache — panel must see the update after refresh.
        color_cache
            .lock()
            .expect("lock")
            .insert(257, crate::terminal::Rgb::new(0x10, 0x0F, 0x0F));
        pool.refresh_panel_colors();

        let panel = pool.panels.get("sess-1").expect("panel still exists");
        assert_eq!(
            panel.background_color_default(),
            Some(crate::terminal::Rgb::new(0x10, 0x0F, 0x0F)),
            "panel must see updated pool color cache after refresh"
        );
    }

    #[test]
    fn focus_same_session_while_connecting_does_not_force_resubscribe() {
        let mut pool = PanelPool::new((24, 80));
        let agent_id = "agent-1";
        let session_uuid = "sess-1";

        let _ = pool.focus_terminal(Some(agent_id), Some(session_uuid), false);
        {
            let panel = pool
                .panels
                .get_mut(session_uuid)
                .expect("panel must exist after initial focus");
            let _ = panel
                .connect(session_uuid)
                .expect("initial connect message");
            assert_eq!(panel.state(), PanelState::Connecting);
        }

        let effects = pool.focus_terminal(Some(agent_id), Some(session_uuid), false);
        assert!(effects.messages.is_empty());
        assert!(effects.pty_inputs.is_empty());
        assert_eq!(
            pool.panels
                .get(session_uuid)
                .expect("panel should remain present")
                .state(),
            PanelState::Connecting
        );
    }
}
