//! Core wire types — `UiNode`, `UiAction`, `UiCapabilitySet`, and the
//! `$kind`-tagged sentinels for responsive values and conditional rendering.
//!
//! These are the renderer-agnostic JSON shapes that both the TUI and web
//! renderers consume. They match the TypeScript types in
//! `docs/specs/cross-client-ui-primitives.md` byte-for-byte.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::ui_contract::viewport::{UiHeightClass, UiOrientation, UiPointer, UiWidthClass};

/// Semantic UI node. `type` names come from the shared primitive inventory
/// defined in the cross-client spec.
///
/// Children and slots use [`UiChild`] so they can hold either a regular
/// node or a `$kind`-tagged conditional wrapper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiNode {
    /// Primitive name (e.g. `"stack"`, `"tree_item"`).
    #[serde(rename = "type")]
    pub node_type: String,
    /// Stable id, used for controlled / uncontrolled state ownership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Primitive-specific props. May contain [`UiResponsive`] sentinels at
    /// any value position.
    #[serde(default, skip_serializing_if = "JsonMap::is_empty")]
    pub props: JsonMap<String, JsonValue>,
    /// Positional children.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<UiChild>,
    /// Semantic slots such as `title`, `subtitle`, `start`, `end`, `footer`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slots: BTreeMap<String, Vec<UiChild>>,
}

impl UiNode {
    /// Build a node with the given primitive type and empty everything else.
    #[must_use]
    pub fn new(node_type: impl Into<String>) -> Self {
        Self {
            node_type: node_type.into(),
            id: None,
            props: JsonMap::new(),
            children: Vec::new(),
            slots: BTreeMap::new(),
        }
    }

    /// Attach a stable id.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
}

/// What can appear in a `children` array or a slot array — either a regular
/// node or a `$kind`-tagged conditional wrapper.
///
/// Serializes as an untagged union: serde tries the conditional variants
/// first (they carry `$kind`) and falls through to the regular node shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UiChild {
    /// A `$kind`-tagged conditional wrapper (`when` or `hidden`).
    Conditional(UiConditional),
    /// A regular primitive node.
    Node(UiNode),
}

impl From<UiNode> for UiChild {
    fn from(value: UiNode) -> Self {
        Self::Node(value)
    }
}

impl From<UiConditional> for UiChild {
    fn from(value: UiConditional) -> Self {
        Self::Conditional(value)
    }
}

/// A conditional wrapper — `ui.when` renders its inner node only when the
/// condition matches; `ui.hidden` renders its inner node only when the
/// condition does not match.
///
/// Wire format:
///
/// ```json
/// { "$kind": "when",   "condition": { "width": "compact" }, "node": { "type": "..." } }
/// { "$kind": "hidden", "condition": { "width": "compact" }, "node": { "type": "..." } }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "$kind", rename_all = "lowercase")]
pub enum UiConditional {
    /// Render the inner node only when `condition` matches the viewport.
    When {
        /// Viewport predicate.
        condition: UiCondition,
        /// Wrapped node.
        node: Box<UiNode>,
    },
    /// Render the inner node only when `condition` does NOT match the viewport.
    Hidden {
        /// Viewport predicate.
        condition: UiCondition,
        /// Wrapped node.
        node: Box<UiNode>,
    },
}

/// Viewport predicate used by `ui.when` / `ui.hidden`.
///
/// Each field is optional; `None` means the field does not participate in the
/// match. A condition matches iff every populated field equals the viewport's
/// corresponding field.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiCondition {
    /// Width-class match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<UiWidthClass>,
    /// Height-class match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<UiHeightClass>,
    /// Pointer-precision match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<UiPointer>,
    /// Orientation match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<UiOrientation>,
    /// Keyboard-occlusion match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard_occluded: Option<bool>,
}

/// A responsive value — resolves to a concrete value based on the current
/// viewport's width / height class.
///
/// Wire format:
///
/// ```json
/// { "$kind": "responsive", "width": { "compact": "vertical", "expanded": "horizontal" } }
/// { "$kind": "responsive", "height": { "short": "sm", "tall": "md" } }
/// { "$kind": "responsive", "width": {...}, "height": {...} }
/// ```
///
/// Dimensions (`width` and `height`) are split into separate sub-maps because
/// the string `"regular"` is valid in both `UiWidthClass` and `UiHeightClass`;
/// a flat map would be ambiguous.
///
/// Authors create one with `ui.responsive({...})` in Lua. Renderers detect the
/// `$kind` discriminator at any prop-value slot and resolve according to the
/// fallback rules in `docs/specs/adaptive-ui-viewport-and-presentation.md`:
/// exact match, then next smaller class, then next larger class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "$kind", rename_all = "lowercase")]
pub enum UiResponsive<T> {
    /// Dimension-keyed responsive value.
    Responsive {
        /// Breakpoint values keyed by `UiWidthClass`.
        #[serde(default = "default_none", skip_serializing_if = "Option::is_none")]
        width: Option<UiResponsiveWidth<T>>,
        /// Breakpoint values keyed by `UiHeightClass`.
        #[serde(default = "default_none", skip_serializing_if = "Option::is_none")]
        height: Option<UiResponsiveHeight<T>>,
    },
}

/// Width-dimension breakpoint map.
///
/// At least one field should be populated in practice; an all-`None`
/// [`UiResponsiveWidth`] carries no information.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiResponsiveWidth<T> {
    /// Value for `widthClass = compact`.
    #[serde(default = "default_none", skip_serializing_if = "Option::is_none")]
    pub compact: Option<T>,
    /// Value for `widthClass = regular`.
    #[serde(default = "default_none", skip_serializing_if = "Option::is_none")]
    pub regular: Option<T>,
    /// Value for `widthClass = expanded`.
    #[serde(default = "default_none", skip_serializing_if = "Option::is_none")]
    pub expanded: Option<T>,
}

// Hand-rolled Default so T is NOT required to implement Default (Option<T>
// is None regardless of T).
impl<T> Default for UiResponsiveWidth<T> {
    fn default() -> Self {
        Self {
            compact: None,
            regular: None,
            expanded: None,
        }
    }
}

impl<T> UiResponsiveWidth<T> {
    /// True if every breakpoint is `None`.
    pub const fn is_empty(&self) -> bool {
        self.compact.is_none() && self.regular.is_none() && self.expanded.is_none()
    }
}

/// Height-dimension breakpoint map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiResponsiveHeight<T> {
    /// Value for `heightClass = short`.
    #[serde(default = "default_none", skip_serializing_if = "Option::is_none")]
    pub short: Option<T>,
    /// Value for `heightClass = regular`.
    #[serde(default = "default_none", skip_serializing_if = "Option::is_none")]
    pub regular: Option<T>,
    /// Value for `heightClass = tall`.
    #[serde(default = "default_none", skip_serializing_if = "Option::is_none")]
    pub tall: Option<T>,
}

impl<T> Default for UiResponsiveHeight<T> {
    fn default() -> Self {
        Self {
            short: None,
            regular: None,
            tall: None,
        }
    }
}

impl<T> UiResponsiveHeight<T> {
    /// True if every breakpoint is `None`.
    pub const fn is_empty(&self) -> bool {
        self.short.is_none() && self.regular.is_none() && self.tall.is_none()
    }
}

const fn default_none<T>() -> Option<T> {
    None
}

/// Either a concrete value `T` or a `UiResponsive<T>` sentinel.
///
/// Useful when typed Props definitions need to express "this field may be
/// responsive". Serializes as an untagged union — renderers either see the
/// scalar or the `$kind: "responsive"` object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UiValue<T> {
    /// A `$kind`-tagged responsive wrapper.
    Responsive(UiResponsive<T>),
    /// A concrete value.
    Scalar(T),
}

impl<T> UiValue<T> {
    /// Wrap a scalar value.
    pub fn scalar(value: T) -> Self {
        Self::Scalar(value)
    }
}

/// Semantic action envelope (`UiAction`).
///
/// Action ids are namespaced Botster intents (e.g. `botster.session.select`).
/// Payloads carry stable domain ids (`sessionUuid`, `workspaceId`) rather than
/// renderer-local identifiers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiAction {
    /// Semantic action id.
    pub id: String,
    /// Action payload.
    #[serde(default, skip_serializing_if = "JsonMap::is_empty")]
    pub payload: JsonMap<String, JsonValue>,
    /// Whether the action is currently disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

impl UiAction {
    /// Build an action with the given id and no payload.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            payload: JsonMap::new(),
            disabled: None,
        }
    }
}

/// Renderer capability set (`UiCapabilitySet`).
///
/// Authors use these booleans to gracefully degrade behavior (e.g. skip a
/// tooltip when `tooltip == false`). Renderers populate the set based on
/// their platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiCapabilitySet {
    /// Hover is a meaningful interaction on this client.
    pub hover: bool,
    /// Dialogs are supported natively.
    pub dialog: bool,
    /// Tooltips are supported natively.
    pub tooltip: bool,
    /// Opening external links is supported.
    pub external_links: bool,
    /// Renderer can consume binary terminal snapshots directly.
    pub binary_terminal_snapshots: bool,
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::needless_borrows_for_generic_args,
        reason = "test-code brevity"
    )]

    use super::*;
    use serde_json::json;

    #[test]
    fn uinode_minimal_serializes_type_only() {
        let node = UiNode::new("stack");
        let s = serde_json::to_value(&node).expect("serialize");
        assert_eq!(s, json!({ "type": "stack" }));
    }

    #[test]
    fn uinode_round_trip() {
        let mut node = UiNode::new("panel").with_id("preview-error");
        node.props.insert("border".into(), json!(true));
        node.children.push(UiNode::new("text").into());
        node.slots
            .insert("title".into(), vec![UiNode::new("text").into()]);

        let s = serde_json::to_string(&node).expect("serialize");
        let back: UiNode = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, node);
    }

    #[test]
    fn responsive_width_only_wire_shape() {
        let resp: UiResponsive<String> = UiResponsive::Responsive {
            width: Some(UiResponsiveWidth {
                compact: Some("vertical".to_string()),
                expanded: Some("horizontal".to_string()),
                ..Default::default()
            }),
            height: None,
        };
        let v = serde_json::to_value(&resp).expect("serialize");
        assert_eq!(
            v,
            json!({
                "$kind": "responsive",
                "width": { "compact": "vertical", "expanded": "horizontal" }
            })
        );
        let back: UiResponsive<String> = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, resp);
    }

    #[test]
    fn responsive_height_only_wire_shape() {
        let resp: UiResponsive<String> = UiResponsive::Responsive {
            width: None,
            height: Some(UiResponsiveHeight {
                short: Some("sm".to_string()),
                tall: Some("md".to_string()),
                ..Default::default()
            }),
        };
        let v = serde_json::to_value(&resp).expect("serialize");
        assert_eq!(
            v,
            json!({
                "$kind": "responsive",
                "height": { "short": "sm", "tall": "md" }
            })
        );
    }

    #[test]
    fn responsive_both_dimensions_roundtrip() {
        let resp: UiResponsive<String> = UiResponsive::Responsive {
            width: Some(UiResponsiveWidth {
                regular: Some("w-regular".to_string()),
                ..Default::default()
            }),
            height: Some(UiResponsiveHeight {
                regular: Some("h-regular".to_string()),
                ..Default::default()
            }),
        };
        let s = serde_json::to_string(&resp).expect("serialize");
        let back: UiResponsive<String> = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, resp);
    }

    #[test]
    fn when_wrapper_wire_shape() {
        let wrapper = UiConditional::When {
            condition: UiCondition {
                width: Some(UiWidthClass::Compact),
                ..Default::default()
            },
            node: Box::new(UiNode::new("stack")),
        };
        let v = serde_json::to_value(&wrapper).expect("serialize");
        assert_eq!(
            v,
            json!({
                "$kind": "when",
                "condition": { "width": "compact" },
                "node": { "type": "stack" }
            })
        );
        let back: UiConditional = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, wrapper);
    }

    #[test]
    fn hidden_wrapper_wire_shape() {
        let wrapper = UiConditional::Hidden {
            condition: UiCondition {
                pointer: Some(UiPointer::Coarse),
                keyboard_occluded: Some(true),
                ..Default::default()
            },
            node: Box::new(UiNode::new("panel")),
        };
        let v = serde_json::to_value(&wrapper).expect("serialize");
        assert_eq!(
            v,
            json!({
                "$kind": "hidden",
                "condition": { "pointer": "coarse", "keyboardOccluded": true },
                "node": { "type": "panel" }
            })
        );
        let back: UiConditional = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, wrapper);
    }

    #[test]
    fn uichild_untagged_union_parses_both() {
        let raw_node = json!({ "type": "text" });
        let from_node: UiChild = serde_json::from_value(raw_node).expect("deserialize node");
        assert!(matches!(from_node, UiChild::Node(_)));

        let raw_wrapper = json!({
            "$kind": "when",
            "condition": { "width": "compact" },
            "node": { "type": "stack" }
        });
        let from_wrapper: UiChild =
            serde_json::from_value(raw_wrapper).expect("deserialize wrapper");
        assert!(matches!(from_wrapper, UiChild::Conditional(_)));
    }

    #[test]
    fn action_envelope_minimal() {
        let action = UiAction::new("botster.session.select");
        let v = serde_json::to_value(&action).expect("serialize");
        assert_eq!(v, json!({ "id": "botster.session.select" }));
    }

    #[test]
    fn action_envelope_with_payload() {
        let mut action = UiAction::new("botster.session.select");
        action
            .payload
            .insert("sessionUuid".into(), json!("sess-123"));
        action.disabled = Some(false);
        let v = serde_json::to_value(&action).expect("serialize");
        assert_eq!(
            v,
            json!({
                "id": "botster.session.select",
                "payload": { "sessionUuid": "sess-123" },
                "disabled": false
            })
        );
    }

    #[test]
    fn capability_set_serializes_camelcase() {
        let caps = UiCapabilitySet {
            hover: true,
            dialog: true,
            tooltip: false,
            external_links: true,
            binary_terminal_snapshots: false,
        };
        let v = serde_json::to_value(&caps).expect("serialize");
        assert_eq!(
            v,
            json!({
                "hover": true,
                "dialog": true,
                "tooltip": false,
                "externalLinks": true,
                "binaryTerminalSnapshots": false
            })
        );
    }

    #[test]
    fn uivalue_untagged_parses_scalar_and_responsive() {
        let scalar: UiValue<String> =
            serde_json::from_value(json!("horizontal")).expect("parse scalar");
        assert!(matches!(scalar, UiValue::Scalar(_)));

        let resp: UiValue<String> = serde_json::from_value(json!({
            "$kind": "responsive",
            "width": { "compact": "vertical" }
        }))
        .expect("parse responsive");
        assert!(matches!(resp, UiValue::Responsive(_)));
    }
}
