//! `$bind` / `bind_list` resolver for the TUI adapter.
//!
//! Plugin authors emit reactive wire snippets in their layouts via two
//! sentinels:
//!
//! ```text
//! ui.bind("/session/sess-abc/title")        →  { "$bind": "/session/sess-abc/title" }
//! ui.bind_list{ source = "/session",         →  { "$kind": "bind_list",
//!                item_template = ui.text{      "source": "/session",
//!                  text = ui.bind("@/title")    "item_template": { ... } }
//!                } }
//! ```
//!
//! Both sentinels are resolved here BEFORE the node tree is deserialized
//! into [`UiNodeV1`] and dispatched to a primitive renderer. That keeps the
//! existing primitive renderers ignorant of the sentinel — they only ever
//! see resolved values + cloned children.
//!
//! ## Path grammar
//!
//! | Path                       | Resolves to                                                |
//! |----------------------------|------------------------------------------------------------|
//! | `/<type>/<id>/<field>`     | scalar lookup (returns `Null` when missing)                |
//! | `/<type>/<id>`             | whole record (`Null` when missing)                         |
//! | `/<type>`                  | array of records sorted by store insertion order           |
//! | `@/<field>`                | item-relative — only valid inside a `bind_list` template   |
//!
//! The web resolver in `app/frontend/ui_contract/binding.tsx` honors the
//! same grammar; both must stay in lockstep.

use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::tui::entity_stores::TuiEntityStores;

/// Sentinel key marking a `$bind` lookup.
pub const BIND_SENTINEL_KEY: &str = "$bind";
/// Sentinel value for `bind_list` (under the shared `$kind` discriminator).
pub const BIND_LIST_KIND: &str = "bind_list";
/// Item-relative path prefix for `@/<field>` inside a `bind_list` template.
const ITEM_RELATIVE_PREFIX: &str = "@";

/// Resolve `$bind` and `bind_list` sentinels in-place across a node tree.
///
/// Walks every value in the tree:
///
/// 1. An object that is *exactly* `{ "$bind": "/<path>" }` is replaced with
///    the resolved value. The replacement happens regardless of position
///    (prop value, array element, anywhere) so a binding can supply a
///    string, number, bool, object, or array depending on what its path
///    targets.
///
/// 2. An object that is `{ "$kind": "bind_list", "source": "/<type>",
///    "item_template": { … } }` is replaced with an array of cloned
///    templates, one per record in `source`'s store. Inside each clone,
///    `@/<field>` paths are resolved against that record before further
///    recursion.
///
/// The function never errors: a missing entity / field resolves to
/// `Null`, an unrecognised path resolves to `Null`, a `bind_list` whose
/// store is empty resolves to an empty array. The TUI renderers handle
/// `Null` props gracefully (treated as "field absent").
pub fn resolve_bindings(value: &mut JsonValue, stores: &TuiEntityStores) {
    resolve_bindings_inner(value, stores, None);
}

fn resolve_bindings_inner(
    value: &mut JsonValue,
    stores: &TuiEntityStores,
    item_context: Option<&JsonValue>,
) {
    match value {
        JsonValue::Object(map) => {
            // Detect $bind sentinel — an object with exactly one key, "$bind",
            // mapped to a string path. Anything else means the object is a
            // regular UiNodeV1 / props / payload table; recurse into it.
            if let Some(replacement) = try_resolve_bind(map, stores, item_context) {
                *value = replacement;
                // After substitution the new value may itself need further
                // recursion (e.g. a whole-record bind returns an object with
                // nested values; future grammar extensions could nest binds).
                resolve_bindings_inner(value, stores, item_context);
                return;
            }
            // Detect bind_list sentinel — object with $kind == "bind_list".
            if is_bind_list(map) {
                *value = expand_bind_list(map, stores, item_context);
                return;
            }
            // Plain object — recurse.
            for child in map.values_mut() {
                resolve_bindings_inner(child, stores, item_context);
            }
        }
        JsonValue::Array(arr) => {
            // After bind_list expansion produces an array, we need to allow
            // further recursion into each element. For ordinary arrays this
            // is just a normal walk.
            for item in arr.iter_mut() {
                resolve_bindings_inner(item, stores, item_context);
            }
        }
        _ => {}
    }
}

fn try_resolve_bind(
    map: &JsonMap<String, JsonValue>,
    stores: &TuiEntityStores,
    item_context: Option<&JsonValue>,
) -> Option<JsonValue> {
    if map.len() != 1 {
        return None;
    }
    let path = map.get(BIND_SENTINEL_KEY)?.as_str()?;
    Some(resolve_path(path, stores, item_context))
}

fn is_bind_list(map: &JsonMap<String, JsonValue>) -> bool {
    map.get("$kind").and_then(|v| v.as_str()) == Some(BIND_LIST_KIND)
}

/// Expand a `bind_list` sentinel into an array of cloned, fully resolved
/// `item_template` instances. The expansion runs through
/// [`resolve_bindings_inner`] for each clone so:
///
/// * `@/<field>` paths inside the template resolve against the per-item
///   record, not the global store.
/// * Nested non-`@` `$bind` sentinels still resolve normally.
fn expand_bind_list(
    map: &JsonMap<String, JsonValue>,
    stores: &TuiEntityStores,
    parent_item: Option<&JsonValue>,
) -> JsonValue {
    let source = map.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let template = map.get("item_template").cloned().unwrap_or(JsonValue::Null);

    let entity_type = strip_leading_slash(source);
    let Some(store) = stores.store(entity_type) else {
        return JsonValue::Array(Vec::new());
    };

    let mut out = Vec::with_capacity(store.order.len());
    for (_id, record) in store.iter() {
        let mut clone = template.clone();
        // Item-relative paths use `record` as the resolution root; the
        // outer parent_item (if any) is shadowed inside this template.
        resolve_bindings_inner(&mut clone, stores, Some(record));
        // Drop bare nulls so consumers receive a clean array. (A template
        // that itself fails to resolve would produce Null after recursion.)
        if !clone.is_null() {
            out.push(clone);
        }
        let _unused_for_clarity = parent_item; // intentionally not threaded down: bind_list shadows
    }
    JsonValue::Array(out)
}

/// Resolve a single `$bind` path. Path forms:
///
/// * `@/<field>` — read `<field>` from the item record currently in scope
///   (only valid inside a `bind_list`). Returns `Null` when no item
///   context is set.
/// * `/<type>` — list lookup; returns an array of records.
/// * `/<type>/<id>` — whole-record lookup.
/// * `/<type>/<id>/<field>` — scalar lookup.
fn resolve_path(
    path: &str,
    stores: &TuiEntityStores,
    item_context: Option<&JsonValue>,
) -> JsonValue {
    if path.starts_with(ITEM_RELATIVE_PREFIX) {
        return resolve_item_relative(path, item_context);
    }
    let parts: Vec<&str> = path
        .strip_prefix('/')
        .unwrap_or(path)
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    match parts.as_slice() {
        [] => JsonValue::Null,
        [entity_type] => resolve_list(entity_type, stores),
        [entity_type, id] => resolve_record(entity_type, id, stores),
        [entity_type, id, field] => resolve_scalar(entity_type, id, field, stores),
        _ => {
            log::debug!("binding: path {path:?} has too many segments — returning Null");
            JsonValue::Null
        }
    }
}

fn resolve_list(entity_type: &str, stores: &TuiEntityStores) -> JsonValue {
    let Some(store) = stores.store(entity_type) else {
        return JsonValue::Array(Vec::new());
    };
    let items: Vec<JsonValue> = store.iter().map(|(_id, record)| record.clone()).collect();
    JsonValue::Array(items)
}

fn resolve_record(entity_type: &str, id: &str, stores: &TuiEntityStores) -> JsonValue {
    stores
        .store(entity_type)
        .and_then(|store| store.by_id.get(id).cloned())
        .unwrap_or(JsonValue::Null)
}

fn resolve_scalar(
    entity_type: &str,
    id: &str,
    field: &str,
    stores: &TuiEntityStores,
) -> JsonValue {
    stores
        .store(entity_type)
        .map(|store| store.field(id, field))
        .unwrap_or(JsonValue::Null)
}

fn resolve_item_relative(path: &str, item_context: Option<&JsonValue>) -> JsonValue {
    let Some(item) = item_context else {
        log::debug!("binding: @-relative path {path:?} outside bind_list — Null");
        return JsonValue::Null;
    };
    // Strip the leading `@` and any leading `/`.
    let rest = path
        .strip_prefix(ITEM_RELATIVE_PREFIX)
        .unwrap_or("")
        .trim_start_matches('/');
    if rest.is_empty() {
        return item.clone();
    }
    let mut current = item;
    for segment in rest.split('/') {
        if segment.is_empty() {
            continue;
        }
        let JsonValue::Object(map) = current else {
            return JsonValue::Null;
        };
        let Some(next) = map.get(segment) else {
            return JsonValue::Null;
        };
        current = next;
    }
    current.clone()
}

fn strip_leading_slash(s: &str) -> &str {
    s.strip_prefix('/').unwrap_or(s)
}

/// Convenience constructor used by tests + the LayoutLua entry point that
/// wants to log the resolved tree size.
#[must_use]
pub fn count_bindings(value: &JsonValue) -> usize {
    let mut count = 0;
    walk(value, &mut |v| {
        if let JsonValue::Object(map) = v {
            if map.len() == 1 && map.contains_key(BIND_SENTINEL_KEY) {
                count += 1;
            } else if is_bind_list(map) {
                count += 1;
            }
        }
    });
    count
}

fn walk<F: FnMut(&JsonValue)>(value: &JsonValue, f: &mut F) {
    f(value);
    match value {
        JsonValue::Object(map) => {
            for v in map.values() {
                walk(v, f);
            }
        }
        JsonValue::Array(arr) => {
            for v in arr {
                walk(v, f);
            }
        }
        _ => {}
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

    fn stores_with_sessions() -> TuiEntityStores {
        let mut stores = TuiEntityStores::new();
        let store = stores.store_mut("session");
        store.apply_snapshot(
            vec![
                json!({
                    "session_uuid": "sess-a",
                    "title": "alpha",
                    "is_idle": false,
                    "hosted_preview": { "status": "running", "url": "https://x" }
                }),
                json!({
                    "session_uuid": "sess-b",
                    "title": "beta",
                    "is_idle": true
                }),
            ],
            "session_uuid",
            1,
        );
        stores
    }

    // -------------------------------------------------------------------------
    // $bind — scalar / record / list
    // -------------------------------------------------------------------------

    #[test]
    fn bind_resolves_scalar_field() {
        let stores = stores_with_sessions();
        let mut value = json!({ "$bind": "/session/sess-a/title" });
        resolve_bindings(&mut value, &stores);
        assert_eq!(value, json!("alpha"));
    }

    #[test]
    fn bind_resolves_whole_record() {
        let stores = stores_with_sessions();
        let mut value = json!({ "$bind": "/session/sess-a" });
        resolve_bindings(&mut value, &stores);
        assert_eq!(value["title"], json!("alpha"));
        assert_eq!(value["is_idle"], json!(false));
        assert_eq!(value["hosted_preview"]["status"], json!("running"));
    }

    #[test]
    fn bind_resolves_full_list() {
        let stores = stores_with_sessions();
        let mut value = json!({ "$bind": "/session" });
        resolve_bindings(&mut value, &stores);
        let arr = value.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["title"], json!("alpha"));
        assert_eq!(arr[1]["title"], json!("beta"));
    }

    #[test]
    fn bind_for_unknown_id_resolves_null() {
        let stores = stores_with_sessions();
        let mut value = json!({ "$bind": "/session/unknown/title" });
        resolve_bindings(&mut value, &stores);
        assert_eq!(value, JsonValue::Null);
    }

    #[test]
    fn bind_for_unknown_field_resolves_null() {
        let stores = stores_with_sessions();
        let mut value = json!({ "$bind": "/session/sess-a/missing_field" });
        resolve_bindings(&mut value, &stores);
        assert_eq!(value, JsonValue::Null);
    }

    #[test]
    fn bind_for_unknown_entity_type_resolves_null_or_empty_array() {
        let stores = stores_with_sessions();
        let mut scalar = json!({ "$bind": "/missing/x/title" });
        resolve_bindings(&mut scalar, &stores);
        assert_eq!(scalar, JsonValue::Null);

        let mut list = json!({ "$bind": "/missing" });
        resolve_bindings(&mut list, &stores);
        assert_eq!(list, json!([]));
    }

    // -------------------------------------------------------------------------
    // $bind — embedded inside a tree
    // -------------------------------------------------------------------------

    #[test]
    fn bind_resolves_inside_props_tree() {
        let stores = stores_with_sessions();
        let mut tree = json!({
            "type": "text",
            "props": {
                "text": { "$bind": "/session/sess-a/title" },
                "tone": "default"
            }
        });
        resolve_bindings(&mut tree, &stores);
        assert_eq!(tree["props"]["text"], json!("alpha"));
        assert_eq!(tree["props"]["tone"], json!("default"));
    }

    #[test]
    fn bind_resolves_inside_action_payload() {
        let stores = stores_with_sessions();
        let mut tree = json!({
            "type": "button",
            "props": {
                "label": "Open",
                "action": {
                    "id": "botster.session.preview.open",
                    "payload": {
                        "url": { "$bind": "/session/sess-a/hosted_preview" }
                    }
                }
            }
        });
        resolve_bindings(&mut tree, &stores);
        let url = &tree["props"]["action"]["payload"]["url"];
        assert_eq!(url["status"], json!("running"));
    }

    #[test]
    fn ordinary_two_key_object_is_not_a_bind_sentinel() {
        let stores = stores_with_sessions();
        let mut tree = json!({
            "type": "text",
            "props": { "text": "hi", "$bind": "/session/sess-a/title" }
        });
        resolve_bindings(&mut tree, &stores);
        // The props map has TWO keys (`text` AND `$bind`), so it must NOT be
        // treated as a sentinel — only an object with exactly { "$bind": ... }
        // qualifies. The `$bind` key stays as data.
        assert_eq!(tree["props"]["$bind"], json!("/session/sess-a/title"));
    }

    // -------------------------------------------------------------------------
    // bind_list expansion
    // -------------------------------------------------------------------------

    #[test]
    fn bind_list_expands_into_array_of_clones_with_item_paths_resolved() {
        let stores = stores_with_sessions();
        let mut value = json!({
            "$kind": "bind_list",
            "source": "/session",
            "item_template": {
                "type": "tree_item",
                "id": { "$bind": "@/session_uuid" },
                "props": {
                    "selected": false
                },
                "slots": {
                    "title": [
                        { "type": "text", "props": { "text": { "$bind": "@/title" } } }
                    ]
                }
            }
        });
        resolve_bindings(&mut value, &stores);
        let arr = value.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], json!("sess-a"));
        assert_eq!(arr[0]["slots"]["title"][0]["props"]["text"], json!("alpha"));
        assert_eq!(arr[1]["id"], json!("sess-b"));
        assert_eq!(arr[1]["slots"]["title"][0]["props"]["text"], json!("beta"));
    }

    #[test]
    fn bind_list_resolves_nested_non_relative_binds_against_global_store() {
        let stores = stores_with_sessions();
        let mut value = json!({
            "$kind": "bind_list",
            "source": "/session",
            "item_template": {
                "type": "text",
                "props": {
                    // Non-relative — same value for every clone.
                    "text": { "$bind": "/session/sess-a/title" }
                }
            }
        });
        resolve_bindings(&mut value, &stores);
        let arr = value.as_array().expect("array");
        for item in arr {
            assert_eq!(item["props"]["text"], json!("alpha"));
        }
    }

    #[test]
    fn bind_list_with_empty_store_yields_empty_array() {
        let stores = TuiEntityStores::new(); // no session store
        let mut value = json!({
            "$kind": "bind_list",
            "source": "/session",
            "item_template": { "type": "text", "props": { "text": "x" } }
        });
        resolve_bindings(&mut value, &stores);
        assert_eq!(value, json!([]));
    }

    #[test]
    fn at_relative_path_outside_bind_list_resolves_null() {
        let stores = stores_with_sessions();
        let mut value = json!({ "$bind": "@/title" });
        resolve_bindings(&mut value, &stores);
        assert_eq!(value, JsonValue::Null);
    }

    #[test]
    fn at_relative_path_with_no_field_returns_whole_item() {
        let stores = stores_with_sessions();
        let mut value = json!({
            "$kind": "bind_list",
            "source": "/session",
            "item_template": { "$bind": "@" }
        });
        resolve_bindings(&mut value, &stores);
        let arr = value.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["title"], json!("alpha"));
    }

    // -------------------------------------------------------------------------
    // count_bindings (introspection helper)
    // -------------------------------------------------------------------------

    #[test]
    fn count_bindings_counts_each_sentinel_once() {
        let value = json!({
            "type": "stack",
            "props": {
                "direction": "vertical"
            },
            "children": [
                { "type": "text", "props": { "text": { "$bind": "/session/x/title" } } },
                {
                    "$kind": "bind_list",
                    "source": "/session",
                    "item_template": { "type": "text", "props": { "text": { "$bind": "@/title" } } }
                }
            ]
        });
        assert_eq!(count_bindings(&value), 3);
    }
}
