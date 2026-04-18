//! Action-envelope plumbing for Phase B's TUI adapter.
//!
//! # Why actions live in two shapes
//!
//! The existing TUI action-dispatch path reads a `Vec<String>` of action
//! ids via [`crate::tui::render_tree::extract_list_actions`] — selection
//! index maps to a string. The Phase A contract uses a richer
//! [`UiActionV1`] envelope with id + payload + disabled.
//!
//! To avoid disturbing existing callers, the adapter keeps both shapes:
//!
//! - rendered [`ListItemProps`] on the TUI tree carry ONLY the string id,
//!   so the legacy extractor continues to work unchanged;
//! - an [`ActionTable`] built alongside the render pass preserves the full
//!   envelope (id + payload + disabled) AND a unique per-row key, so
//!   payload-aware dispatch can resolve the exact row that was activated
//!   even when several rows share the same action id.
//!
//! Phase B does NOT touch the existing dispatch path. The table is
//! deliberately a standalone lookup so payload routing can be layered in
//! later without changing how `extract_list_actions` behaves today.
//!
//! # Keying
//!
//! Each entry has a **unique, walk-order-stable key** — never the action
//! id. Keys are:
//!
//! - `id:<node.id>` when the source [`UiNodeV1`] has an explicit stable
//!   [`UiNodeV1::id`], or
//! - `anon:<n>` where `n` is a monotonic counter incremented every time
//!   the adapter records an anonymous action.
//!
//! This guarantees that a list of 10 rows all dispatching the same action
//! id (e.g. `botster.session.select`) keeps 10 distinct envelopes, one per
//! row, rather than collapsing to the last row's payload.
//!
//! The bare action id is still available on each entry for diagnostics
//! and for dispatchers that want to group by intent.
//!
//! [`UiActionV1`]: crate::ui_contract::node::UiActionV1
//! [`UiNodeV1`]: crate::ui_contract::node::UiNodeV1
//! [`UiNodeV1::id`]: crate::ui_contract::node::UiNodeV1::id
//! [`ListItemProps`]: crate::tui::render_tree::ListItemProps

// Rust guideline compliant 2026-04-18

use crate::ui_contract::node::UiActionV1;

/// A single recorded action — the envelope plus a unique per-row key and
/// the source node's stable id, if any.
#[derive(Debug, Clone)]
pub struct ActionEntry {
    /// Walk-order-unique key. `id:<node.id>` when the source node has an
    /// explicit id; `anon:<n>` otherwise. Never collides across rows in
    /// the same render pass.
    pub key: String,
    /// The source node's stable id, mirrored from [`UiNodeV1::id`] when
    /// present. Separate from `key` so callers can correlate against
    /// their own node-id selectors without parsing the key string.
    ///
    /// [`UiNodeV1::id`]: crate::ui_contract::node::UiNodeV1::id
    pub node_id: Option<String>,
    /// Full [`UiActionV1`] envelope — id, payload, disabled flag.
    pub action: UiActionV1,
}

/// Per-render lookup table of every [`UiActionV1`] envelope produced by
/// the adapter during a single render pass.
///
/// Entries are recorded in walk order — the order they appear in the
/// `RenderNode` tree matches the order they were inserted here. Keys are
/// guaranteed unique even for rows that share an action id. See the
/// module doc for the keying scheme.
#[derive(Debug, Clone, Default)]
pub struct ActionTable {
    entries: Vec<ActionEntry>,
    next_anon: usize,
}

impl ActionTable {
    /// Construct an empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an action envelope.
    ///
    /// When `node_id` is `Some`, the entry's key is
    /// `"id:<node_id>"`; otherwise the adapter mints a fresh
    /// `"anon:<n>"` key so same-id rows do not collide.
    ///
    /// Returns the newly inserted entry's key so the caller can correlate
    /// it back to the rendered row if needed.
    pub fn insert(&mut self, node_id: Option<&str>, action: UiActionV1) -> String {
        let key = if let Some(id) = node_id {
            format!("id:{id}")
        } else {
            let n = self.next_anon;
            self.next_anon = self.next_anon.wrapping_add(1);
            format!("anon:{n}")
        };
        let entry = ActionEntry {
            key: key.clone(),
            node_id: node_id.map(std::string::ToString::to_string),
            action,
        };
        self.entries.push(entry);
        key
    }

    /// Look up an entry by its unique walk-order key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&ActionEntry> {
        self.entries.iter().find(|entry| entry.key == key)
    }

    /// Iterate every entry whose envelope carries the given semantic
    /// action id.
    ///
    /// Walk order is preserved so callers can map "the Nth item with this
    /// action id" to a specific row's envelope.
    pub fn by_action_id<'a>(
        &'a self,
        action_id: &'a str,
    ) -> impl Iterator<Item = &'a ActionEntry> + 'a {
        self.entries
            .iter()
            .filter(move |entry| entry.action.id == action_id)
    }

    /// Look up the first envelope for a given action id.
    ///
    /// Convenience for dispatchers that only care about "does *any* row
    /// with this id exist"; use [`Self::by_action_id`] when the caller
    /// needs the specific row's payload.
    #[must_use]
    pub fn first_by_action_id(&self, action_id: &str) -> Option<&UiActionV1> {
        self.entries
            .iter()
            .find(|entry| entry.action.id == action_id)
            .map(|entry| &entry.action)
    }

    /// Number of recorded entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table has no recorded entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over every recorded entry in walk order.
    pub fn iter(&self) -> impl Iterator<Item = &ActionEntry> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn insert_with_node_id_produces_id_key() {
        let mut table = ActionTable::new();
        let key = table.insert(Some("row-1"), UiActionV1::new("x"));
        assert_eq!(key, "id:row-1");
        assert_eq!(table.get("id:row-1").expect("found").action.id, "x");
    }

    #[test]
    fn insert_without_node_id_produces_unique_anon_keys() {
        let mut table = ActionTable::new();
        let k1 = table.insert(None, UiActionV1::new("same"));
        let k2 = table.insert(None, UiActionV1::new("same"));
        assert_ne!(k1, k2);
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn multiple_rows_with_same_action_id_preserve_distinct_payloads() {
        // Regression test for codex F1 — same action id across rows must
        // not collapse to the last entry's payload.
        let mut table = ActionTable::new();
        for session in ["sess-a", "sess-b", "sess-c"] {
            let mut envelope = UiActionV1::new("botster.session.select");
            envelope
                .payload
                .insert("sessionUuid".into(), json!(session));
            table.insert(Some(session), envelope);
        }
        let recorded: Vec<_> = table.by_action_id("botster.session.select").collect();
        assert_eq!(recorded.len(), 3);
        assert_eq!(
            recorded[0].action.payload.get("sessionUuid"),
            Some(&json!("sess-a"))
        );
        assert_eq!(
            recorded[1].action.payload.get("sessionUuid"),
            Some(&json!("sess-b"))
        );
        assert_eq!(
            recorded[2].action.payload.get("sessionUuid"),
            Some(&json!("sess-c"))
        );
    }

    #[test]
    fn first_by_action_id_returns_walk_order_head() {
        let mut table = ActionTable::new();
        let mut first = UiActionV1::new("foo");
        first.payload.insert("k".into(), json!(1));
        table.insert(Some("a"), first);
        let mut second = UiActionV1::new("foo");
        second.payload.insert("k".into(), json!(2));
        table.insert(Some("b"), second);
        let got = table.first_by_action_id("foo").expect("present");
        assert_eq!(got.payload.get("k"), Some(&json!(1)));
    }

    #[test]
    fn is_empty_for_fresh_table() {
        let t = ActionTable::new();
        assert!(t.is_empty());
        assert_eq!(t.len(), 0);
    }
}
