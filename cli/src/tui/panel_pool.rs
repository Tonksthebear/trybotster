//! Panel pool — owns terminal panels and focus state.
//!
//! Manages the collection of [`TerminalPanel`] instances keyed by
//! `(agent_index, pty_index)` and tracks which panel is currently focused.
//! Methods return [`OutMessages`] instead of sending directly, keeping
//! channel I/O in the runner.

// Rust guideline compliant 2026-02

use std::collections::HashMap;

use super::render_tree::RenderNode;
use super::terminal_panel::{PanelState, TerminalPanel};

/// Messages to send to Hub, collected and forwarded by the runner.
pub type OutMessages = Vec<serde_json::Value>;

/// Explicitly-targeted PTY input — carries destination indices so
/// the runner can send it without relying on implicit focus state.
#[derive(Debug)]
pub struct PtyInput {
    /// Agent index to route the input to.
    pub agent_index: usize,
    /// PTY index to route the input to.
    pub pty_index: usize,
    /// Raw bytes (typically synthetic focus escape sequences).
    pub data: &'static [u8],
}

/// Side effects from a focus switch.
///
/// The runner applies these mechanically — no focus logic needed.
/// PTY inputs carry explicit targets so ordering is safe regardless
/// of when `PanelPool` mutated its internal indices.
#[derive(Debug, Default)]
pub struct FocusEffects {
    /// PTY inputs in order: focus-out (old PTY), then focus-in (new PTY).
    pub pty_inputs: Vec<PtyInput>,
    /// Hub messages (subscribe/unsubscribe).
    pub messages: OutMessages,
    /// Whether to clear inner kitty state on `TerminalModes`.
    pub clear_kitty: bool,
}

/// Owns terminal panels and tracks which agent/PTY is focused.
///
/// Pure state machine — never touches channels or stdout. All external
/// effects are returned as [`OutMessages`] for the runner to execute.
#[derive(Debug)]
pub struct PanelPool {
    /// Terminal panels keyed by `(agent_index, pty_index)`.
    pub(super) panels: HashMap<(usize, usize), TerminalPanel>,
    /// Terminal dimensions (rows, cols) — used as default for new panels.
    pub(super) terminal_dims: (u16, u16),
    /// Currently selected agent session key.
    pub(super) selected_agent: Option<String>,
    /// Active PTY index (cycles with Ctrl+]).
    pub(super) active_pty_index: usize,
    /// Index of agent being viewed.
    pub(super) current_agent_index: Option<usize>,
    /// Index of PTY being viewed (0 = CLI, 1 = server).
    pub(super) current_pty_index: Option<usize>,
    /// Subscription ID of the focused PTY.
    pub(super) current_terminal_sub_id: Option<String>,
}

impl PanelPool {
    /// Create an empty pool with initial terminal dimensions.
    pub fn new(terminal_dims: (u16, u16)) -> Self {
        Self {
            panels: HashMap::new(),
            terminal_dims,
            selected_agent: None,
            active_pty_index: 0,
            current_agent_index: None,
            current_pty_index: None,
            current_terminal_sub_id: None,
        }
    }

    // === Panel Access ===

    /// Get or create a panel for the given agent/PTY, falling back to current focus.
    pub fn resolve_panel(
        &mut self,
        agent_index: Option<usize>,
        pty_index: Option<usize>,
    ) -> &mut TerminalPanel {
        let key = (
            agent_index.or(self.current_agent_index).unwrap_or(0),
            pty_index.or(self.current_pty_index).unwrap_or(0),
        );
        let (rows, cols) = self.terminal_dims;
        self.panels
            .entry(key)
            .or_insert_with(|| TerminalPanel::new(rows, cols))
    }

    /// Immutable reference to the focused panel.
    pub fn focused_panel(&self) -> Option<&TerminalPanel> {
        let key = (self.current_agent_index?, self.current_pty_index?);
        self.panels.get(&key)
    }

    /// Mutable reference to the focused panel.
    pub fn focused_panel_mut(&mut self) -> Option<&mut TerminalPanel> {
        let key = (self.current_agent_index?, self.current_pty_index?);
        self.panels.get_mut(&key)
    }

    /// The focused panel key, if any.
    pub fn focused_key(&self) -> Option<(usize, usize)> {
        Some((self.current_agent_index?, self.current_pty_index?))
    }

    /// Whether a message targets the currently focused panel.
    pub fn is_focused(&self, agent_index: Option<usize>, pty_index: Option<usize>) -> bool {
        agent_index == self.current_agent_index && pty_index == self.current_pty_index
    }

    /// Direct access to panels map (for rendering).
    pub fn panels(&self) -> &HashMap<(usize, usize), TerminalPanel> {
        &self.panels
    }

    /// Check if a panel exists for the given key.
    pub fn contains_key(&self, key: &(usize, usize)) -> bool {
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
        areas: &HashMap<(usize, usize), (u16, u16)>,
    ) -> OutMessages {
        let mut msgs = OutMessages::new();

        let default_agent = self.current_agent_index.unwrap_or(0);
        let default_pty = self.current_pty_index.unwrap_or(0);

        let desired =
            super::render_tree::collect_terminal_bindings(tree, default_agent, default_pty);

        // Connect panels for new bindings
        for &(agent_idx, pty_idx) in &desired {
            let (rows, cols) = areas
                .get(&(agent_idx, pty_idx))
                .copied()
                .unwrap_or(self.terminal_dims);
            let panel = self
                .panels
                .entry((agent_idx, pty_idx))
                .or_insert_with(|| TerminalPanel::new(rows, cols));

            if panel.state() == PanelState::Idle {
                if let Some(msg) = panel.connect(agent_idx, pty_idx) {
                    msgs.push(msg);
                }
            }
        }

        // Disconnect panels not in desired set (skip the focused panel)
        let focused = (
            self.current_agent_index.unwrap_or(usize::MAX),
            self.current_pty_index.unwrap_or(usize::MAX),
        );
        let to_disconnect: Vec<(usize, usize)> = self
            .panels
            .keys()
            .filter(|k| !desired.contains(k) && **k != focused)
            .copied()
            .collect();

        for (ai, pi) in to_disconnect {
            if let Some(panel) = self.panels.get_mut(&(ai, pi)) {
                if let Some(msg) = panel.disconnect(ai, pi) {
                    msgs.push(msg);
                }
            }
            self.panels.remove(&(ai, pi));
        }

        msgs
    }

    /// Resize panels to match rendered widget areas.
    ///
    /// Returns resize messages for panels whose dimensions changed.
    pub fn sync_widget_dims(
        &mut self,
        areas: &HashMap<(usize, usize), (u16, u16)>,
    ) -> OutMessages {
        let mut msgs = OutMessages::new();

        for (&(agent_idx, pty_idx), &(rows, cols)) in areas {
            if let Some(panel) = self.panels.get_mut(&(agent_idx, pty_idx)) {
                if let Some(msg) = panel.resize(rows, cols, agent_idx, pty_idx) {
                    msgs.push(msg);
                }
            }
        }

        // Remove panels for bindings no longer rendered (except focused)
        let focused = (
            self.current_agent_index.unwrap_or(usize::MAX),
            self.current_pty_index.unwrap_or(usize::MAX),
        );
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
            .filter_map(|(&(ai, pi), panel)| panel.disconnect(ai, pi))
            .collect();
        self.panels.clear();
        self.current_terminal_sub_id = None;
        self.current_agent_index = None;
        self.current_pty_index = None;
        self.selected_agent = None;
        msgs
    }

    // === Focus ===

    /// Switch focus to a specific agent and PTY.
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
        agent_index: Option<usize>,
        pty_index: usize,
        terminal_focused: bool,
    ) -> FocusEffects {
        let mut effects = FocusEffects::default();

        // Clear selection if no agent_id
        let Some(agent_id) = agent_id else {
            if terminal_focused && self.current_agent_index.is_some() {
                if let (Some(ai), Some(pi)) = (self.current_agent_index, self.current_pty_index) {
                    log::debug!("[FOCUS] synthetic focus-out on clear, agent={ai}");
                    effects.pty_inputs.push(PtyInput { agent_index: ai, pty_index: pi, data: b"\x1b[O" });
                }
            }
            if let (Some(ai), Some(pi)) = (self.current_agent_index, self.current_pty_index) {
                if let Some(panel) = self.panels.get_mut(&(ai, pi)) {
                    if let Some(msg) = panel.disconnect(ai, pi) {
                        effects.messages.push(msg);
                    }
                }
            }
            self.selected_agent = None;
            self.current_agent_index = None;
            self.current_pty_index = None;
            self.current_terminal_sub_id = None;
            effects.clear_kitty = true;
            return effects;
        };

        let Some(index) = agent_index else {
            log::warn!("focus_terminal: missing agent_index for agent {agent_id}");
            return effects;
        };

        if self.current_agent_index == Some(index)
            && self.current_pty_index == Some(pty_index)
            && self.selected_agent.as_deref() == Some(agent_id)
        {
            log::debug!("focus_terminal: already focused on agent {agent_id} pty {pty_index}");
            return effects;
        }

        // Synthetic focus-out BEFORE changing indices (targets old PTY)
        if terminal_focused && self.current_agent_index.is_some() {
            if let (Some(ai), Some(pi)) = (self.current_agent_index, self.current_pty_index) {
                log::debug!("[FOCUS] synthetic focus-out on switch, old_agent={ai}");
                effects.pty_inputs.push(PtyInput { agent_index: ai, pty_index: pi, data: b"\x1b[O" });
            }
        }

        // Disconnect old panel
        if let (Some(ai), Some(pi)) = (self.current_agent_index, self.current_pty_index) {
            if let Some(panel) = self.panels.get_mut(&(ai, pi)) {
                if let Some(msg) = panel.disconnect(ai, pi) {
                    effects.messages.push(msg);
                }
            }
        }

        // Discard stale panel if agent shifted
        if self.selected_agent.as_deref() != Some(agent_id) {
            self.panels.remove(&(index, pty_index));
        }

        // Get or create panel, inheriting dims from outgoing panel
        let old_key = (
            self.current_agent_index.unwrap_or(usize::MAX),
            self.current_pty_index.unwrap_or(usize::MAX),
        );
        let widget_dims = self.panels.get(&old_key)
            .map(|p| p.dims())
            .unwrap_or(self.terminal_dims);
        let panel = self.panels
            .entry((index, pty_index))
            .or_insert_with(|| TerminalPanel::new(widget_dims.0, widget_dims.1));

        if let Some(msg) = panel.connect(index, pty_index) {
            effects.messages.push(msg);
        }

        // Update focus state
        self.selected_agent = Some(agent_id.to_string());
        self.current_agent_index = Some(index);
        self.current_pty_index = Some(pty_index);
        self.active_pty_index = pty_index;
        self.current_terminal_sub_id = Some(format!("tui:{}:{}", index, pty_index));

        // Synthetic focus-in AFTER updating indices (targets new PTY)
        if terminal_focused {
            log::debug!("[FOCUS] synthetic focus-in on switch, new_agent={index} pty={pty_index}");
            effects.pty_inputs.push(PtyInput { agent_index: index, pty_index: pty_index, data: b"\x1b[I" });
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

    /// Index of the agent being viewed.
    pub fn current_agent_index(&self) -> Option<usize> {
        self.current_agent_index
    }

    /// Index of the PTY being viewed.
    pub fn current_pty_index(&self) -> Option<usize> {
        self.current_pty_index
    }

    /// Terminal dimensions (rows, cols).
    pub fn terminal_dims(&self) -> (u16, u16) {
        self.terminal_dims
    }

    /// Active PTY index (cycles with Ctrl+]).
    pub fn active_pty_index(&self) -> usize {
        self.active_pty_index
    }

    /// Subscription ID of the focused PTY.
    pub fn current_terminal_sub_id(&self) -> Option<&str> {
        self.current_terminal_sub_id.as_deref()
    }
}
