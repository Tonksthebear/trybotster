//! Resolution helpers for `$kind`-tagged responsive values and conditional
//! child wrappers in a `UiNodeV1` tree.
//!
//! Phase A guarantees that `$kind = "responsive"` only appears at
//! prop-value positions, and `$kind = "when"` / `$kind = "hidden"` only
//! appear at child / slot positions (`UiChildV1::Conditional`). This module
//! matches that split exactly.
//!
//! Fallback rules follow `docs/specs/adaptive-ui-viewport-and-presentation.md`:
//! exact class match first, then the next smaller class, then the next
//! larger class. The same order is used for both width and height.

// Rust guideline compliant 2026-04-18

#![expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "UiViewportV1 is Copy but we pass by reference deliberately for consistency with the adapter's many other fns and because the reference reads as 'the current viewport context'"
)]

use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::ui_contract::node::{UiChildV1, UiConditionV1, UiConditionalV1, UiNodeV1};
use crate::ui_contract::viewport::{UiHeightClass, UiViewportV1, UiWidthClass};

/// The `$kind` discriminator key used by Phase A.
const KIND_KEY: &str = "$kind";
/// `$kind = "responsive"`.
const KIND_RESPONSIVE: &str = "responsive";

/// Walk `props` (and any nested objects / arrays inside) and collapse every
/// `{ "$kind": "responsive", ... }` sentinel into the concrete value for the
/// given `viewport`.
///
/// Non-sentinel values are passed through unchanged. Resolution is
/// recursive because Phase A allows responsive values at any prop-value
/// slot, including nested ones (e.g. a nested action payload may carry a
/// responsive flag).
#[must_use]
pub fn resolve_props(
    props: &JsonMap<String, JsonValue>,
    viewport: &UiViewportV1,
) -> JsonMap<String, JsonValue> {
    let mut out = JsonMap::with_capacity(props.len());
    for (key, value) in props {
        out.insert(key.clone(), resolve_value(value, viewport));
    }
    out
}

/// Resolve a single JSON value, collapsing any `$kind = "responsive"`
/// sentinels it contains.
///
/// Used by [`resolve_props`] and reused by test helpers that need a
/// standalone resolution primitive.
#[must_use]
pub fn resolve_value(value: &JsonValue, viewport: &UiViewportV1) -> JsonValue {
    match value {
        JsonValue::Object(map) => {
            if let Some(resolved) = resolve_responsive_sentinel(map, viewport) {
                return resolved;
            }
            let mut resolved_map = JsonMap::with_capacity(map.len());
            for (key, inner) in map {
                resolved_map.insert(key.clone(), resolve_value(inner, viewport));
            }
            JsonValue::Object(resolved_map)
        }
        JsonValue::Array(items) => JsonValue::Array(
            items
                .iter()
                .map(|item| resolve_value(item, viewport))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// If `map` is a responsive sentinel, resolve it for `viewport`. Returns
/// `None` for regular JSON objects so the caller can recurse into them.
fn resolve_responsive_sentinel(
    map: &JsonMap<String, JsonValue>,
    viewport: &UiViewportV1,
) -> Option<JsonValue> {
    let kind = map.get(KIND_KEY)?.as_str()?;
    if kind != KIND_RESPONSIVE {
        // `$kind = "when"` / `$kind = "hidden"` only appear at child
        // positions, never inside a prop value; Phase A enforces this at
        // the Lua layer. Treat such an object as opaque here.
        return None;
    }

    // Try width first, then height — the two dimensions are independent per
    // Phase A's wire format, and either may be missing.
    if let Some(width) = map.get("width").and_then(JsonValue::as_object) {
        if let Some(resolved) = pick_width(width, viewport.width_class) {
            return Some(resolved.clone());
        }
    }
    if let Some(height) = map.get("height").and_then(JsonValue::as_object) {
        if let Some(resolved) = pick_height(height, viewport.height_class) {
            return Some(resolved.clone());
        }
    }
    // No breakpoint matched — return JSON null so the downstream renderer
    // can treat it as "prop absent".
    Some(JsonValue::Null)
}

/// Try `target`, then the next smaller width class, then the next larger,
/// per the adaptive spec's fallback rules.
fn pick_width(
    map: &JsonMap<String, JsonValue>,
    target: UiWidthClass,
) -> Option<&JsonValue> {
    for candidate in width_fallback_order(target) {
        if let Some(value) = map.get(width_key(candidate)) {
            return Some(value);
        }
    }
    None
}

/// Try `target`, then the next smaller height class, then the next larger,
/// per the adaptive spec's fallback rules.
fn pick_height(
    map: &JsonMap<String, JsonValue>,
    target: UiHeightClass,
) -> Option<&JsonValue> {
    for candidate in height_fallback_order(target) {
        if let Some(value) = map.get(height_key(candidate)) {
            return Some(value);
        }
    }
    None
}

const fn width_key(class: UiWidthClass) -> &'static str {
    match class {
        UiWidthClass::Compact => "compact",
        UiWidthClass::Regular => "regular",
        UiWidthClass::Expanded => "expanded",
    }
}

const fn height_key(class: UiHeightClass) -> &'static str {
    match class {
        UiHeightClass::Short => "short",
        UiHeightClass::Regular => "regular",
        UiHeightClass::Tall => "tall",
    }
}

/// Fallback search order for a given target width class.
///
/// Exact match first, then next smaller, then next larger — directly from
/// the adaptive spec's "Responsive values" rules.
const fn width_fallback_order(target: UiWidthClass) -> [UiWidthClass; 3] {
    match target {
        UiWidthClass::Compact => [
            UiWidthClass::Compact,
            UiWidthClass::Regular,
            UiWidthClass::Expanded,
        ],
        UiWidthClass::Regular => [
            UiWidthClass::Regular,
            UiWidthClass::Compact,
            UiWidthClass::Expanded,
        ],
        UiWidthClass::Expanded => [
            UiWidthClass::Expanded,
            UiWidthClass::Regular,
            UiWidthClass::Compact,
        ],
    }
}

/// Fallback search order for a given target height class.
const fn height_fallback_order(target: UiHeightClass) -> [UiHeightClass; 3] {
    match target {
        UiHeightClass::Short => [
            UiHeightClass::Short,
            UiHeightClass::Regular,
            UiHeightClass::Tall,
        ],
        UiHeightClass::Regular => [
            UiHeightClass::Regular,
            UiHeightClass::Short,
            UiHeightClass::Tall,
        ],
        UiHeightClass::Tall => [
            UiHeightClass::Tall,
            UiHeightClass::Regular,
            UiHeightClass::Short,
        ],
    }
}

// =============================================================================
// Condition matching (`ui.when` / `ui.hidden`)
// =============================================================================

/// Test whether `condition` matches `viewport`.
///
/// Matching is conjunctive: every populated field of the condition must
/// equal the corresponding field in the viewport. `None` fields do not
/// participate (they match anything). Missing viewport fields
/// (`orientation`, `keyboard_occluded`) are treated as no-match when the
/// condition populates them — the condition is more specific than the
/// viewport can answer for.
#[must_use]
pub fn condition_matches(condition: &UiConditionV1, viewport: &UiViewportV1) -> bool {
    if let Some(w) = condition.width {
        if w != viewport.width_class {
            return false;
        }
    }
    if let Some(h) = condition.height {
        if h != viewport.height_class {
            return false;
        }
    }
    if let Some(p) = condition.pointer {
        if p != viewport.pointer {
            return false;
        }
    }
    if let Some(o) = condition.orientation {
        if viewport.orientation != Some(o) {
            return false;
        }
    }
    if let Some(k) = condition.keyboard_occluded {
        if viewport.keyboard_occluded != Some(k) {
            return false;
        }
    }
    true
}

/// Filter an array of children, dropping any that fail their conditional
/// wrapper's test and unwrapping the rest.
///
/// - [`UiChildV1::Node`] passes through as-is.
/// - [`UiConditionalV1::When`] is kept iff the condition matches.
/// - [`UiConditionalV1::Hidden`] is kept iff the condition does NOT match.
///
/// Returns a fresh `Vec<UiNodeV1>` of the survivors, ready for further
/// adapter work.
#[must_use]
pub fn filter_children(children: &[UiChildV1], viewport: &UiViewportV1) -> Vec<UiNodeV1> {
    children
        .iter()
        .filter_map(|child| resolve_child(child, viewport))
        .collect()
}

/// Decide whether a single child should render; returns the inner
/// [`UiNodeV1`] when it should, or `None` when the conditional elides it.
#[must_use]
pub fn resolve_child(child: &UiChildV1, viewport: &UiViewportV1) -> Option<UiNodeV1> {
    match child {
        UiChildV1::Node(node) => Some(node.clone()),
        UiChildV1::Conditional(UiConditionalV1::When { condition, node }) => {
            condition_matches(condition, viewport).then(|| (**node).clone())
        }
        UiChildV1::Conditional(UiConditionalV1::Hidden { condition, node }) => {
            (!condition_matches(condition, viewport)).then(|| (**node).clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_contract::viewport::UiPointer;
    use serde_json::json;

    fn viewport(w: UiWidthClass, h: UiHeightClass) -> UiViewportV1 {
        UiViewportV1::new(w, h, UiPointer::None)
    }

    #[test]
    fn resolve_picks_exact_width_match() {
        let raw = json!({
            "$kind": "responsive",
            "width": { "compact": "vertical", "expanded": "horizontal" }
        });
        let vp = viewport(UiWidthClass::Compact, UiHeightClass::Regular);
        assert_eq!(resolve_value(&raw, &vp), json!("vertical"));
    }

    #[test]
    fn resolve_falls_back_to_smaller_then_larger() {
        // Only `compact` and `expanded` defined; target `regular` should
        // fall back first to `compact` (smaller), not `expanded`.
        let raw = json!({
            "$kind": "responsive",
            "width": { "compact": "vertical", "expanded": "horizontal" }
        });
        let vp = viewport(UiWidthClass::Regular, UiHeightClass::Regular);
        assert_eq!(resolve_value(&raw, &vp), json!("vertical"));
    }

    #[test]
    fn resolve_expanded_falls_back_to_regular_then_compact() {
        let raw = json!({
            "$kind": "responsive",
            "width": { "regular": "r-only", "compact": "c-only" }
        });
        let vp = viewport(UiWidthClass::Expanded, UiHeightClass::Regular);
        assert_eq!(resolve_value(&raw, &vp), json!("r-only"));
    }

    #[test]
    fn resolve_height_exact_match() {
        let raw = json!({
            "$kind": "responsive",
            "height": { "short": "sm", "tall": "md" }
        });
        let vp = viewport(UiWidthClass::Regular, UiHeightClass::Short);
        assert_eq!(resolve_value(&raw, &vp), json!("sm"));
    }

    #[test]
    fn resolve_passes_through_non_sentinels() {
        let raw = json!({ "tone": "muted", "nested": { "x": 1 } });
        let vp = viewport(UiWidthClass::Regular, UiHeightClass::Regular);
        assert_eq!(resolve_value(&raw, &vp), raw);
    }

    #[test]
    fn resolve_leaves_conditional_wrappers_opaque() {
        // `$kind = "when"` is a child-position wrapper, not a prop-value
        // sentinel. It should NOT be rewritten here.
        let raw = json!({
            "$kind": "when",
            "condition": { "width": "compact" },
            "node": { "type": "stack" }
        });
        let vp = viewport(UiWidthClass::Regular, UiHeightClass::Regular);
        assert_eq!(resolve_value(&raw, &vp), raw);
    }

    #[test]
    fn resolve_returns_null_when_no_breakpoint_matches() {
        let raw = json!({ "$kind": "responsive", "width": {} });
        let vp = viewport(UiWidthClass::Regular, UiHeightClass::Regular);
        assert_eq!(resolve_value(&raw, &vp), JsonValue::Null);
    }

    #[test]
    fn condition_conjunctive_match() {
        let c = UiConditionV1 {
            width: Some(UiWidthClass::Compact),
            pointer: Some(UiPointer::None),
            ..Default::default()
        };
        let vp = viewport(UiWidthClass::Compact, UiHeightClass::Regular);
        assert!(condition_matches(&c, &vp));
    }

    #[test]
    fn condition_mismatch_on_any_field() {
        let c = UiConditionV1 {
            width: Some(UiWidthClass::Compact),
            height: Some(UiHeightClass::Tall),
            ..Default::default()
        };
        let vp = viewport(UiWidthClass::Compact, UiHeightClass::Short);
        assert!(!condition_matches(&c, &vp));
    }
}
