//! Persistent widget state for stateful TUI widgets.
//!
//! Widgets in the render tree are rebuilt from Lua every frame, but interactive
//! widgets (lists, inputs) need persistent state across frames for selection
//! tracking, cursor position, and text buffers.
//!
//! # Controlled vs Uncontrolled
//!
//! Widgets with an `id` prop and no explicit `selected`/`value` prop are
//! **uncontrolled** — Rust owns their mechanical state. Widgets with explicit
//! state props are **controlled** — Lua owns their state (current behavior).
//!
//! ```text
//! WidgetStateStore (owned by TuiRunner)
//! ├── lists: HashMap<String, ListWidgetState>
//! └── inputs: HashMap<String, InputWidgetState>
//! ```

// Rust guideline compliant 2026-02

use std::collections::{HashMap, HashSet};

use ratatui::widgets::ListState;
use tui_input::{Input, InputRequest};

/// Persistent state for a list widget.
///
/// Tracks a selectable index (0-based among non-header items) separately
/// from the ratatui `ListState` (which stores absolute row positions).
/// The renderer converts selectable → absolute each frame; navigation
/// operates only on the selectable index.
#[derive(Debug)]
pub struct ListWidgetState {
    /// ratatui list state (absolute row index, set by renderer each frame).
    state: ListState,
    /// Selectable index (0-based among non-header items).
    selected_index: usize,
    /// Number of selectable (non-header) items, updated each render frame.
    selectable_count: usize,
}

impl ListWidgetState {
    /// Create a new list state with selection at index 0.
    pub fn new() -> Self {
        Self {
            state: ListState::default(),
            selected_index: 0,
            selectable_count: 0,
        }
    }

    /// Move selection up by one. Returns the new selectable index.
    pub fn select_up(&mut self) -> usize {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
        self.selected_index
    }

    /// Move selection down by one. Returns the new selectable index.
    pub fn select_down(&mut self) -> usize {
        let max = self.selectable_count.saturating_sub(1);
        if self.selected_index < max {
            self.selected_index += 1;
        }
        self.selected_index
    }

    /// Set selection to a specific selectable index (clamped to bounds).
    /// Returns the actual index after clamping.
    pub fn select(&mut self, index: usize) -> usize {
        self.selected_index = index.min(self.selectable_count.saturating_sub(1));
        self.selected_index
    }

    /// Get the current selectable index (0-based among non-header items).
    pub fn selected(&self) -> usize {
        self.selected_index
    }

    /// Get a mutable reference to the underlying ratatui `ListState`.
    ///
    /// The renderer uses this to set the absolute row index each frame.
    pub fn ratatui_state_mut(&mut self) -> &mut ListState {
        &mut self.state
    }

    /// Get the current selectable item count.
    pub fn selectable_count(&self) -> usize {
        self.selectable_count
    }

    /// Update the selectable item count (called each render frame).
    pub fn set_selectable_count(&mut self, count: usize) {
        self.selectable_count = count;
        // Clamp selection if items were removed
        if count > 0 && self.selected_index >= count {
            self.selected_index = count - 1;
        }
    }

    /// Reset selection to the first item.
    pub fn reset(&mut self) {
        self.selected_index = 0;
    }
}

/// Persistent state for an input widget.
///
/// Wraps [`tui_input::Input`] which manages the text buffer, cursor position,
/// word movement, and yank buffer.
#[derive(Debug)]
pub struct InputWidgetState {
    /// Headless input state (buffer + cursor + yank).
    input: Input,
}

impl InputWidgetState {
    /// Create a new empty input state.
    pub fn new() -> Self {
        Self {
            input: Input::default(),
        }
    }

    /// Get the current text value.
    pub fn value(&self) -> &str {
        self.input.value()
    }

    /// Get the cursor position (character index).
    pub fn cursor(&self) -> usize {
        self.input.cursor()
    }

    /// Get the visual cursor position (accounting for wide characters).
    pub fn visual_cursor(&self) -> usize {
        self.input.visual_cursor()
    }

    /// Get the visual scroll offset for a given viewport width.
    pub fn visual_scroll(&self, width: usize) -> usize {
        self.input.visual_scroll(width)
    }

    /// Handle an input request (insert char, delete, cursor movement, etc.).
    pub fn handle(&mut self, req: InputRequest) {
        self.input.handle(req);
    }

    /// Reset the input to empty.
    pub fn reset(&mut self) {
        self.input.reset();
    }
}

/// Store for all persistent widget states, keyed by widget ID.
///
/// Owned by `TuiRunner`. Widgets declare their ID via an `id` prop in the
/// Lua render tree. State is created lazily on first access and garbage
/// collected when widgets disappear from the tree.
#[derive(Debug, Default)]
pub struct WidgetStateStore {
    /// List widget states keyed by widget ID.
    lists: HashMap<String, ListWidgetState>,
    /// Input widget states keyed by widget ID.
    inputs: HashMap<String, InputWidgetState>,
}

impl WidgetStateStore {
    /// Create a new empty state store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create list state for a widget ID.
    pub fn list_state(&mut self, id: &str) -> &mut ListWidgetState {
        self.lists
            .entry(id.to_string())
            .or_insert_with(ListWidgetState::new)
    }

    /// Get or create input state for a widget ID.
    pub fn input_state(&mut self, id: &str) -> &mut InputWidgetState {
        self.inputs
            .entry(id.to_string())
            .or_insert_with(InputWidgetState::new)
    }

    /// Reset all widget states (called on mode transitions).
    pub fn reset_all(&mut self) {
        for state in self.lists.values_mut() {
            state.reset();
        }
        for state in self.inputs.values_mut() {
            state.reset();
        }
    }

    /// Remove states for widget IDs not present in the given set.
    ///
    /// Called after each render pass to garbage collect state for widgets
    /// that are no longer in the render tree.
    pub fn retain_seen(&mut self, seen_ids: &HashSet<String>) {
        self.lists.retain(|id, _| seen_ids.contains(id));
        self.inputs.retain(|id, _| seen_ids.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_selection_bounds() {
        let mut state = ListWidgetState::new();
        state.set_selectable_count(3);

        assert_eq!(state.selected(), 0);
        assert_eq!(state.select_down(), 1);
        assert_eq!(state.select_down(), 2);
        // At max, should not go further
        assert_eq!(state.select_down(), 2);
        assert_eq!(state.select_up(), 1);
        assert_eq!(state.select_up(), 0);
        // At min, should not go further
        assert_eq!(state.select_up(), 0);
    }

    #[test]
    fn list_select_clamps() {
        let mut state = ListWidgetState::new();
        state.set_selectable_count(3);

        assert_eq!(state.select(5), 2);
        assert_eq!(state.select(1), 1);
    }

    #[test]
    fn list_count_change_clamps_selection() {
        let mut state = ListWidgetState::new();
        state.set_selectable_count(5);
        state.select(4);

        // Shrink item count — selection should clamp
        state.set_selectable_count(3);
        assert_eq!(state.selected(), 2);
    }

    #[test]
    fn list_reset() {
        let mut state = ListWidgetState::new();
        state.set_selectable_count(5);
        state.select(3);
        state.reset();
        assert_eq!(state.selected(), 0);
    }

    #[test]
    fn input_basic_operations() {
        let mut state = InputWidgetState::new();
        assert_eq!(state.value(), "");
        assert_eq!(state.cursor(), 0);

        state.handle(InputRequest::InsertChar('h'));
        state.handle(InputRequest::InsertChar('i'));
        assert_eq!(state.value(), "hi");
        assert_eq!(state.cursor(), 2);

        state.handle(InputRequest::DeletePrevChar);
        assert_eq!(state.value(), "h");
        assert_eq!(state.cursor(), 1);
    }

    #[test]
    fn input_cursor_movement() {
        let mut state = InputWidgetState::new();
        state.handle(InputRequest::InsertChar('a'));
        state.handle(InputRequest::InsertChar('b'));
        state.handle(InputRequest::InsertChar('c'));

        state.handle(InputRequest::GoToStart);
        assert_eq!(state.cursor(), 0);

        state.handle(InputRequest::GoToEnd);
        assert_eq!(state.cursor(), 3);

        state.handle(InputRequest::GoToPrevChar);
        assert_eq!(state.cursor(), 2);

        state.handle(InputRequest::GoToNextChar);
        assert_eq!(state.cursor(), 3);
    }

    #[test]
    fn input_reset() {
        let mut state = InputWidgetState::new();
        state.handle(InputRequest::InsertChar('x'));
        state.reset();
        assert_eq!(state.value(), "");
        assert_eq!(state.cursor(), 0);
    }

    /// Simulates the render→navigate→render cycle that caused the original bug.
    ///
    /// The renderer sets the ratatui `ListState` to an absolute row index
    /// (accounting for non-selectable header rows). Navigation must still
    /// operate in selectable-index space, not absolute-row space.
    #[test]
    fn list_navigation_survives_render_absolute_index() {
        let mut state = ListWidgetState::new();
        // Menu with 1 header + 2 selectable items:
        //   row 0: "── Hub ──"  (header, non-selectable)
        //   row 1: "New Agent"  (selectable index 0)
        //   row 2: "Show Code"  (selectable index 1)
        state.set_selectable_count(2);
        assert_eq!(state.selected(), 0);

        // Simulate what the renderer does: convert selectable 0 → absolute 1
        // (skipping the header at row 0) and set it on the ratatui state.
        state.ratatui_state_mut().select(Some(1)); // absolute row for "New Agent"

        // Navigation should still work in selectable-index space.
        assert_eq!(state.select_down(), 1, "should move to selectable index 1");

        // Simulate render again: selectable 1 → absolute 2
        state.ratatui_state_mut().select(Some(2));

        // Should not go past max
        assert_eq!(state.select_down(), 1, "should clamp at max selectable");

        // Should be able to go back up
        assert_eq!(state.select_up(), 0);

        // Simulate render: selectable 0 → absolute 1
        state.ratatui_state_mut().select(Some(1));

        // Should not go below 0
        assert_eq!(state.select_up(), 0, "should clamp at 0");
    }

    /// Verifies that `set_selectable_count` clamps `selected_index` independently
    /// of the ratatui state's absolute index.
    #[test]
    fn list_count_change_clamps_selected_index_not_absolute() {
        let mut state = ListWidgetState::new();
        state.set_selectable_count(5);
        state.select(4); // selectable index 4

        // Simulate render: absolute index could be higher (e.g., 6 with headers)
        state.ratatui_state_mut().select(Some(6));

        // Shrink — should clamp selected_index, not the ratatui absolute
        state.set_selectable_count(3);
        assert_eq!(state.selected(), 2, "selected_index should be clamped to 2");
    }

    #[test]
    fn store_get_or_create() {
        let mut store = WidgetStateStore::new();

        // First access creates state
        store.list_state("menu").set_selectable_count(3);
        store.list_state("menu").select(2);
        assert_eq!(store.list_state("menu").selected(), 2);

        // Different ID gets different state
        assert_eq!(store.list_state("other").selected(), 0);
    }

    #[test]
    fn store_reset_all() {
        let mut store = WidgetStateStore::new();

        store.list_state("menu").set_selectable_count(3);
        store.list_state("menu").select(2);
        store.input_state("prompt").handle(InputRequest::InsertChar('x'));

        store.reset_all();

        assert_eq!(store.list_state("menu").selected(), 0);
        assert_eq!(store.input_state("prompt").value(), "");
    }

    #[test]
    fn store_retain_seen() {
        let mut store = WidgetStateStore::new();

        store.list_state("menu").set_selectable_count(3);
        store.list_state("old_menu").set_selectable_count(5);
        store.input_state("prompt");
        store.input_state("old_prompt");

        let mut seen = HashSet::new();
        seen.insert("menu".to_string());
        seen.insert("prompt".to_string());
        store.retain_seen(&seen);

        // "old_menu" and "old_prompt" should be gone — accessing creates fresh state
        assert_eq!(store.list_state("old_menu").selected(), 0);
        assert_eq!(store.input_state("old_prompt").value(), "");
    }
}
