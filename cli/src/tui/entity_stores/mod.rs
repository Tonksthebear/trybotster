//! TUI-side entity stores for wire protocol.
//!
//! The hub ships per-entity-type wire envelopes (`entity_snapshot`,
//! `entity_upsert`, `entity_patch`, `entity_remove`) which the TUI applies
//! to local in-memory stores. Composite renderers in
//! [`crate::tui::ui_contract_adapter::primitive`] read from these stores
//! when rendering `ui.session_list{}`, `ui.workspace_list{}`, etc., and
//! the `$bind` resolver in
//! [`crate::tui::ui_contract_adapter::binding`] (added in commit 4) walks
//! the same store to resolve plugin bindings.
//!
//! ## Module layout
//!
//! Per the design brief, each built-in entity type gets its own file with
//! a named struct alias. The underlying record/dispatch logic is shared via
//! [`EntityStore`] so a new entity type only needs a one-line file. This
//! keeps the wire shape identical across all built-in types — patches
//! merge field-by-field and nested values replace wholesale (see
//! `apply_patch` for the merge rule, which matches the design brief
//! §12.4).
//!
//! ## Entity record shape
//!
//! Records are stored as untyped `serde_json::Value`. The TUI composite
//! renderers read fields dynamically (string, bool, optional nested
//! object), so adding a typed wrapper struct per entity would be pure
//! boilerplate. Plugin entity types (`<plugin>.<type>`) reuse the same
//! `EntityStore` shape with whatever record schema the plugin defines.
//!
//! ## snapshot_seq
//!
//! Each store keeps the most recent `snapshot_seq` it received. Delta frames
//! older than the current seq (out-of-order or replayed) are dropped.
//! Reconnect re-ships an `entity_snapshot` per type; snapshots replace local
//! contents only when they are at least as fresh as the current baseline.

use std::collections::HashMap;

use serde_json::Value as JsonValue;

pub mod hub_meta_store;
pub mod session_store;
pub mod spawn_target_store;
pub mod workspace_store;
pub mod worktree_store;

pub use hub_meta_store::{ConnectionCodeStore, HubMetaStore};
pub use session_store::SessionStore;
pub use spawn_target_store::SpawnTargetStore;
pub use workspace_store::WorkspaceStore;
pub use worktree_store::WorktreeStore;

/// Wire envelope types the stores recognize.
const ENTITY_FRAME_TYPES: &[&str] = &[
    "entity_snapshot",
    "entity_upsert",
    "entity_patch",
    "entity_remove",
];

/// One per entity type. Owned by [`TuiEntityStores`] (which provides
/// HashMap-keyed access) and re-exported under semantic per-type aliases
/// (`SessionStore`, `WorkspaceStore`, …) from this module.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct EntityStore {
    /// Insertion-ordered list of entity ids. Order is determined by the
    /// most recent `entity_snapshot` items array; subsequent upserts
    /// append, patches preserve position, removes shorten in place.
    pub order: Vec<String>,
    /// id → entity record (untyped JSON for renderer flexibility).
    pub by_id: HashMap<String, JsonValue>,
    /// Most recent snapshot_seq applied. Out-of-order frames (seq <= this)
    /// are dropped.
    pub snapshot_seq: u64,
}

impl EntityStore {
    /// Replace the store's contents with a fresh snapshot. Order is taken
    /// from the items array so the renderer's iteration order matches the
    /// hub's intent.
    pub fn apply_snapshot(&mut self, items: Vec<JsonValue>, id_field: &str, snapshot_seq: u64) {
        if snapshot_seq != 0 && snapshot_seq < self.snapshot_seq {
            log::debug!(
                "tui entity_stores: dropping stale snapshot (seq={snapshot_seq}, last={prev})",
                prev = self.snapshot_seq
            );
            return;
        }
        self.snapshot_seq = snapshot_seq;
        self.order.clear();
        self.by_id.clear();
        for item in items {
            let Some(id) = extract_id(&item, id_field) else {
                log::warn!(
                    "tui entity_stores: snapshot item missing id_field {id_field:?}: {item}"
                );
                continue;
            };
            self.order.push(id.clone());
            self.by_id.insert(id, item);
        }
    }

    /// Insert a new entity or replace an existing one wholesale. Position
    /// in `order` is preserved on update; new ids append.
    pub fn apply_upsert(&mut self, id: String, entity: JsonValue, snapshot_seq: u64) {
        if !self.accept_seq(snapshot_seq, "upsert") {
            return;
        }
        if !self.by_id.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.by_id.insert(id, entity);
    }

    /// Merge a sparse patch into an existing entity. Top-level fields are
    /// merged; nested objects in the patch REPLACE the existing nested
    /// object wholesale (per design brief §12.4 — `hosted_preview` is the
    /// canonical example). No-ops gracefully when the entity is unknown
    /// — the next snapshot will reconcile.
    pub fn apply_patch(&mut self, id: &str, patch: JsonValue, snapshot_seq: u64) {
        if !self.accept_seq(snapshot_seq, "patch") {
            return;
        }
        let JsonValue::Object(patch_map) = patch else {
            log::warn!("tui entity_stores: patch for {id:?} not an object");
            return;
        };
        let Some(JsonValue::Object(existing)) = self.by_id.get_mut(id) else {
            log::debug!(
                "tui entity_stores: patch for unknown id {id:?} (will reconcile on next snapshot)"
            );
            return;
        };
        for (k, v) in patch_map {
            existing.insert(k, v);
        }
    }

    /// Remove an entity. Drops it from both `order` and `by_id`. Idempotent.
    pub fn apply_remove(&mut self, id: &str, snapshot_seq: u64) {
        if !self.accept_seq(snapshot_seq, "remove") {
            return;
        }
        self.by_id.remove(id);
        self.order.retain(|existing| existing != id);
    }

    /// Read a field from an entity, returning `serde_json::Value::Null`
    /// when the entity or field is missing. Used by the binding resolver.
    pub fn field(&self, id: &str, field: &str) -> JsonValue {
        self.by_id
            .get(id)
            .and_then(|entity| entity.get(field).cloned())
            .unwrap_or(JsonValue::Null)
    }

    /// Iterate entities in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &JsonValue)> {
        self.order
            .iter()
            .filter_map(move |id| self.by_id.get(id).map(|v| (id, v)))
    }

    fn accept_seq(&mut self, snapshot_seq: u64, op: &str) -> bool {
        // snapshot_seq == 0 is allowed for the very first delta the hub
        // ships. Subsequent deltas must strictly increase. Snapshots bypass
        // this check because subscribe/reconnect uses them as authoritative
        // resync frames, often with the same seq as the most recent delta.
        if snapshot_seq == 0 {
            self.snapshot_seq = 0;
            return true;
        }
        if snapshot_seq <= self.snapshot_seq {
            log::debug!(
                "tui entity_stores: dropping out-of-order {op} (seq={snapshot_seq}, last={prev})",
                prev = self.snapshot_seq
            );
            return false;
        }
        self.snapshot_seq = snapshot_seq;
        true
    }
}

fn extract_id(entity: &JsonValue, id_field: &str) -> Option<String> {
    entity
        .get(id_field)
        .and_then(|v| v.as_str())
        .or_else(|| entity.get("id").and_then(|v| v.as_str()))
        .map(str::to_string)
}

/// Aggregate of all built-in entity stores plus a HashMap for plugin
/// entity types. Owned by [`crate::tui::runner::TuiRunner`] and updated by
/// the wire-frame dispatcher.
#[derive(Debug, Default)]
pub struct TuiEntityStores {
    /// Stores keyed by entity_type. Built-in types (`session`,
    /// `workspace`, …) and plugin types (`<plugin>.<type>`) share this
    /// HashMap so the dispatcher needs zero per-type wiring.
    by_type: HashMap<String, EntityStore>,
}

impl TuiEntityStores {
    /// Construct an empty aggregate. Same as `Self::default()`; spelled out
    /// because [`crate::tui::runner::TuiRunner::new_with_color_cache`]
    /// reads more clearly with a named constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow the store for a specific entity type. Returns `None` when
    /// no frame for that type has yet been applied — callers (the binding
    /// resolver, composite renderers) should treat that as "no data" and
    /// render their empty state.
    pub fn store(&self, entity_type: &str) -> Option<&EntityStore> {
        self.by_type.get(entity_type)
    }

    /// Borrow-mut, creating an empty store for the type on first access.
    pub fn store_mut(&mut self, entity_type: &str) -> &mut EntityStore {
        self.by_type.entry(entity_type.to_string()).or_default()
    }

    /// Returns the entity_type names with active stores, sorted for
    /// deterministic test output.
    pub fn registered_types(&self) -> Vec<String> {
        let mut names: Vec<String> = self.by_type.keys().cloned().collect();
        names.sort();
        names
    }

    /// Returns true iff the given message looks like one of the four
    /// entity envelope types. Used by the wire dispatcher to short-circuit
    /// before deserialising.
    pub fn handles_frame(msg_type: &str) -> bool {
        ENTITY_FRAME_TYPES.contains(&msg_type)
    }

    /// Apply one entity envelope. Returns true when the frame was
    /// recognised and applied (or rejected as out-of-order); returns false
    /// when the frame was unhandled (the caller should forward it to Lua).
    ///
    /// Built-in entity types use `id_field` defaults that match the
    /// design brief §4.1; plugin types fall back to the literal `"id"`
    /// field, which is the spec for plugin entity wire shape.
    pub fn apply_frame(&mut self, frame: &JsonValue) -> bool {
        let Some(msg_type) = frame.get("type").and_then(|v| v.as_str()) else {
            return false;
        };
        if !Self::handles_frame(msg_type) {
            return false;
        }
        let Some(entity_type) = frame.get("entity_type").and_then(|v| v.as_str()) else {
            log::warn!("tui entity_stores: {msg_type} missing entity_type");
            return true;
        };
        let snapshot_seq = frame
            .get("snapshot_seq")
            .and_then(JsonValue::as_u64)
            .unwrap_or(0);
        let id_field = id_field_for(entity_type);
        let store = self.store_mut(entity_type);
        match msg_type {
            "entity_snapshot" => {
                let items = frame
                    .get("items")
                    .and_then(JsonValue::as_array)
                    .cloned()
                    .unwrap_or_default();
                store.apply_snapshot(items, id_field, snapshot_seq);
            }
            "entity_upsert" => {
                let Some(id) = frame.get("id").and_then(|v| v.as_str()).map(str::to_string) else {
                    log::warn!("tui entity_stores: entity_upsert missing id");
                    return true;
                };
                let entity = frame.get("entity").cloned().unwrap_or(JsonValue::Null);
                store.apply_upsert(id, entity, snapshot_seq);
            }
            "entity_patch" => {
                let Some(id) = frame.get("id").and_then(|v| v.as_str()) else {
                    log::warn!("tui entity_stores: entity_patch missing id");
                    return true;
                };
                let patch = frame.get("patch").cloned().unwrap_or(JsonValue::Null);
                store.apply_patch(id, patch, snapshot_seq);
            }
            "entity_remove" => {
                let Some(id) = frame.get("id").and_then(|v| v.as_str()) else {
                    log::warn!("tui entity_stores: entity_remove missing id");
                    return true;
                };
                store.apply_remove(id, snapshot_seq);
            }
            _ => unreachable!("handles_frame already filtered"),
        }
        true
    }
}

/// Default `id_field` for a built-in entity type. Plugin types fall back to
/// `"id"`. Mirrors the §4.1 wire spec.
fn id_field_for(entity_type: &str) -> &'static str {
    match entity_type {
        "session" => "session_uuid",
        "workspace" => "workspace_id",
        "spawn_target" => "target_id",
        "worktree" => "worktree_path",
        "hub" | "connection_code" => "hub_id",
        // Plugin types (`<plugin>.<type>`) supply their own id under "id".
        _ => "id",
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::missing_docs_in_private_items,
        reason = "test-code brevity"
    )]

    use super::*;
    use serde_json::json;

    fn snap_frame(items: Vec<JsonValue>, seq: u64) -> JsonValue {
        json!({
            "v": 2,
            "type": "entity_snapshot",
            "entity_type": "session",
            "items": items,
            "snapshot_seq": seq
        })
    }

    fn upsert_frame(id: &str, entity: JsonValue, seq: u64) -> JsonValue {
        json!({
            "v": 2,
            "type": "entity_upsert",
            "entity_type": "session",
            "id": id,
            "entity": entity,
            "snapshot_seq": seq
        })
    }

    fn patch_frame(id: &str, patch: JsonValue, seq: u64) -> JsonValue {
        json!({
            "v": 2,
            "type": "entity_patch",
            "entity_type": "session",
            "id": id,
            "patch": patch,
            "snapshot_seq": seq
        })
    }

    fn remove_frame(id: &str, seq: u64) -> JsonValue {
        json!({
            "v": 2,
            "type": "entity_remove",
            "entity_type": "session",
            "id": id,
            "snapshot_seq": seq
        })
    }

    #[test]
    fn snapshot_replaces_contents_and_orders_by_items() {
        let mut stores = TuiEntityStores::new();
        let frame = snap_frame(
            vec![
                json!({ "session_uuid": "sess-b", "title": "beta" }),
                json!({ "session_uuid": "sess-a", "title": "alpha" }),
            ],
            5,
        );
        assert!(stores.apply_frame(&frame));
        let store = stores.store("session").expect("session store");
        assert_eq!(store.order, vec!["sess-b", "sess-a"]);
        assert_eq!(store.snapshot_seq, 5);
        assert_eq!(store.by_id["sess-a"]["title"], json!("alpha"));
    }

    #[test]
    fn upsert_appends_new_id_preserves_position_for_updates() {
        let mut stores = TuiEntityStores::new();
        let snap = snap_frame(
            vec![
                json!({ "session_uuid": "sess-a", "title": "alpha" }),
                json!({ "session_uuid": "sess-b", "title": "beta" }),
            ],
            1,
        );
        stores.apply_frame(&snap);

        let upsert_existing = upsert_frame(
            "sess-a",
            json!({ "session_uuid": "sess-a", "title": "alpha2" }),
            2,
        );
        stores.apply_frame(&upsert_existing);

        let upsert_new = upsert_frame(
            "sess-c",
            json!({ "session_uuid": "sess-c", "title": "gamma" }),
            3,
        );
        stores.apply_frame(&upsert_new);

        let store = stores.store("session").expect("store");
        assert_eq!(store.order, vec!["sess-a", "sess-b", "sess-c"]);
        assert_eq!(store.by_id["sess-a"]["title"], json!("alpha2"));
    }

    #[test]
    fn patch_merges_top_level_fields_and_replaces_nested_objects() {
        let mut stores = TuiEntityStores::new();
        stores.apply_frame(&snap_frame(
            vec![json!({
                "session_uuid": "sess-a",
                "title": "alpha",
                "is_idle": true,
                "hosted_preview": { "status": "starting", "url": null },
            })],
            1,
        ));

        // Top-level field merge.
        stores.apply_frame(&patch_frame(
            "sess-a",
            json!({ "title": "alpha2", "is_idle": false }),
            2,
        ));
        let store = stores.store("session").expect("store");
        assert_eq!(store.by_id["sess-a"]["title"], json!("alpha2"));
        assert_eq!(store.by_id["sess-a"]["is_idle"], json!(false));

        // Nested object REPLACES wholesale (per §12.4 — even though `url`
        // was set in the prior nested object, the new patch's hosted_preview
        // is the source of truth and the `url` field disappears).
        stores.apply_frame(&patch_frame(
            "sess-a",
            json!({ "hosted_preview": { "status": "running" } }),
            3,
        ));
        let store = stores.store("session").expect("store");
        let hp = &store.by_id["sess-a"]["hosted_preview"];
        assert_eq!(hp["status"], json!("running"));
        assert!(
            hp.get("url").is_none(),
            "nested patch should replace wholesale: {hp}"
        );
    }

    #[test]
    fn remove_drops_id_from_order_and_by_id() {
        let mut stores = TuiEntityStores::new();
        stores.apply_frame(&snap_frame(
            vec![
                json!({ "session_uuid": "sess-a", "title": "a" }),
                json!({ "session_uuid": "sess-b", "title": "b" }),
            ],
            1,
        ));
        stores.apply_frame(&remove_frame("sess-a", 2));
        let store = stores.store("session").expect("store");
        assert_eq!(store.order, vec!["sess-b"]);
        assert!(!store.by_id.contains_key("sess-a"));
    }

    #[test]
    fn out_of_order_frames_are_dropped() {
        let mut stores = TuiEntityStores::new();
        stores.apply_frame(&snap_frame(
            vec![json!({ "session_uuid": "sess-a", "title": "a" })],
            5,
        ));
        // Older patch should be dropped — store stays at seq 5 with title "a".
        stores.apply_frame(&patch_frame("sess-a", json!({ "title": "stale" }), 3));
        let store = stores.store("session").expect("store");
        assert_eq!(store.snapshot_seq, 5);
        assert_eq!(store.by_id["sess-a"]["title"], json!("a"));
    }

    #[test]
    fn snapshot_with_same_seq_resyncs_store_and_lower_seq_is_dropped() {
        let mut stores = TuiEntityStores::new();
        stores.apply_frame(&snap_frame(
            vec![json!({ "session_uuid": "sess-a", "title": "stale" })],
            5,
        ));
        stores.apply_frame(&snap_frame(
            vec![json!({ "session_uuid": "sess-b", "title": "fresh" })],
            5,
        ));
        stores.apply_frame(&snap_frame(
            vec![json!({ "session_uuid": "sess-c", "title": "reset" })],
            4,
        ));

        let store = stores.store("session").expect("store");
        assert_eq!(store.order, vec!["sess-b"]);
        assert!(!store.by_id.contains_key("sess-a"));
        assert_eq!(store.by_id["sess-b"]["title"], json!("fresh"));
        assert!(!store.by_id.contains_key("sess-c"));
        assert_eq!(store.snapshot_seq, 5);
    }

    #[test]
    fn handles_frame_recognises_only_v2_entity_envelopes() {
        assert!(TuiEntityStores::handles_frame("entity_snapshot"));
        assert!(TuiEntityStores::handles_frame("entity_upsert"));
        assert!(TuiEntityStores::handles_frame("entity_patch"));
        assert!(TuiEntityStores::handles_frame("entity_remove"));
        assert!(!TuiEntityStores::handles_frame("agent_list"));
        assert!(!TuiEntityStores::handles_frame("ui_tree_snapshot"));
        assert!(!TuiEntityStores::handles_frame("transient_event"));
    }

    #[test]
    fn apply_frame_returns_false_for_legacy_envelopes() {
        let mut stores = TuiEntityStores::new();
        let legacy = json!({ "type": "agent_list", "agents": [] });
        assert!(!stores.apply_frame(&legacy));
    }

    #[test]
    fn iter_yields_entries_in_insertion_order() {
        let mut store = EntityStore::default();
        store.apply_snapshot(
            vec![
                json!({ "session_uuid": "a", "title": "A" }),
                json!({ "session_uuid": "b", "title": "B" }),
            ],
            "session_uuid",
            1,
        );
        let collected: Vec<(&String, &JsonValue)> = store.iter().collect();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].0, "a");
        assert_eq!(collected[1].0, "b");
    }

    #[test]
    fn field_returns_null_when_entity_or_field_missing() {
        let store = EntityStore::default();
        assert_eq!(store.field("missing", "title"), JsonValue::Null);

        let mut store = EntityStore::default();
        store.apply_upsert(
            "sess-a".into(),
            json!({ "session_uuid": "sess-a", "title": "alpha" }),
            1,
        );
        assert_eq!(store.field("sess-a", "title"), json!("alpha"));
        assert_eq!(store.field("sess-a", "missing_field"), JsonValue::Null);
        assert_eq!(store.field("sess-b", "title"), JsonValue::Null);
    }

    #[test]
    fn plugin_entity_type_round_trips_via_id_field() {
        let mut stores = TuiEntityStores::new();
        let frame = json!({
            "v": 2,
            "type": "entity_snapshot",
            "entity_type": "kanban.board",
            "items": [
                { "id": "board-1", "name": "Roadmap" },
                { "id": "board-2", "name": "Triage" }
            ],
            "snapshot_seq": 1
        });
        assert!(stores.apply_frame(&frame));
        let store = stores.store("kanban.board").expect("plugin store");
        assert_eq!(store.order, vec!["board-1", "board-2"]);
        assert_eq!(store.by_id["board-1"]["name"], json!("Roadmap"));
    }

    #[test]
    fn registered_types_returns_sorted_names() {
        let mut stores = TuiEntityStores::new();
        stores.store_mut("workspace");
        stores.store_mut("session");
        stores.store_mut("kanban.board");
        assert_eq!(
            stores.registered_types(),
            vec!["kanban.board", "session", "workspace"]
        );
    }
}
