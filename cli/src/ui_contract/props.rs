//! Strongly-typed Props structs for every current Lua-public primitive plus the
//! internal `Dialog`.
//!
//! **Cross-client spec is canonical.** Every field here matches
//! `docs/specs/cross-client-ui-primitives.md` exactly. Web-runtime-only
//! extensions (`Panel.padding`, `Panel.radius`, `Stack.padding`,
//! `Button.leadingIcon`, `Button.disabled`, `Tree.density`, …) are
//! intentionally *not* on these shared structs — they are renderer-internal,
//! not contract obligations, per
//! `cross-client ui should share semantic primitives and actions with renderer-specific adapters.md`.
//!
//! When a Props field needs to admit a responsive value, the field type is
//! [`UiValue`], so a renderer sees either a scalar or a `$kind="responsive"`
//! wrapper.
//!
//! Slots (e.g. `TreeItem.title`, `Dialog.body`, `Dialog.footer`) live on
//! [`crate::ui_contract::node::UiNode::slots`], not in these Props structs.

use serde::{Deserialize, Serialize};

use crate::ui_contract::node::{UiAction, UiValue};
use crate::ui_contract::tokens::{
    UiAlign, UiBadgeSize, UiBadgeTone, UiButtonTone, UiButtonVariant, UiInteractionDensity,
    UiJustify, UiPanelTone, UiPresentation, UiScrollAxis, UiSessionListGrouping, UiSize, UiSpace,
    UiStackDirection, UiStatusDotState, UiSurfaceDensity, UiTextWeight, UiTone,
};

/// `Stack` props.
///
/// Per cross-client spec, `direction` is **required** (no `?`). The TUI
/// translates `horizontal` / `vertical` into its internal `HSplit` / `VSplit`
/// render nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackProps {
    /// Stack direction — required.
    pub direction: UiValue<UiStackDirection>,
    /// Gap between children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<UiValue<UiSpace>>,
    /// Cross-axis alignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<UiValue<UiAlign>>,
    /// Main-axis distribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justify: Option<UiValue<UiJustify>>,
}

/// `Inline` props.
///
/// Cross-client lists `inline` as a layout primitive but does not specify its
/// props. We expose only the semantic fields that both renderers can honor;
/// web-only extensions (like `padding`) are left out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineProps {
    /// Gap between children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<UiValue<UiSpace>>,
    /// Cross-axis alignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<UiValue<UiAlign>>,
    /// Main-axis distribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justify: Option<UiValue<UiJustify>>,
    /// Whether to wrap onto a new line when space runs out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<bool>,
}

/// `Panel` props — matches the cross-client spec shape exactly.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelProps {
    /// Optional title text. When present, renderers typically draw a header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Panel background tone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<UiPanelTone>,
    /// Whether to render a visible border.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<bool>,
    /// Interaction density — may be responsive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_density: Option<UiValue<UiInteractionDensity>>,
}

/// `ScrollArea` props.
///
/// Cross-client does not spec explicit fields, but both renderers need to
/// know the scroll axis to render correctly.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollAreaProps {
    /// Axis along which to scroll.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub axis: Option<UiScrollAxis>,
}

/// `Text` props — matches the cross-client spec shape exactly.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextProps {
    /// Required text body.
    pub text: String,
    /// Tone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<UiTone>,
    /// Size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<UiSize>,
    /// Weight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<UiTextWeight>,
    /// Render in monospace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monospace: Option<bool>,
    /// Render italic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    /// Truncate with ellipsis when overflowing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncate: Option<bool>,
}

/// `Icon` props.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IconProps {
    /// Icon id.
    pub name: String,
    /// Icon size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<UiSize>,
    /// Icon tone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<UiTone>,
    /// Accessible label (required for icon-only buttons).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `Badge` props.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BadgeProps {
    /// Badge text.
    pub text: String,
    /// Badge tone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<UiBadgeTone>,
    /// Badge size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<UiBadgeSize>,
}

/// `StatusDot` props.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusDotProps {
    /// Dot state.
    pub state: UiStatusDotState,
    /// Accessible label describing the state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `EmptyState` props.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyStateProps {
    /// Title text.
    pub title: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional icon id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Optional primary action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_action: Option<UiAction>,
}

/// `Button` props — matches the cross-client spec shape exactly.
///
/// There is no `disabled` field: per cross-client spec, disabled state travels
/// on the `UiAction::disabled` carried by `action`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButtonProps {
    /// Button label.
    pub label: String,
    /// Action emitted on press.
    pub action: UiAction,
    /// Visual variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<UiButtonVariant>,
    /// Tone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<UiButtonTone>,
    /// Optional icon id (cross-client spec name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// `IconButton` props.
///
/// `label` is required — icon-only buttons must always carry an accessible
/// label. There is no `disabled` field; use `action.disabled` instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IconButtonProps {
    /// Icon id.
    pub icon: String,
    /// Accessible label.
    pub label: String,
    /// Action emitted on press.
    pub action: UiAction,
    /// Tone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<UiButtonTone>,
}

// Tree has no shared props in current — the web-only `density` surface variant
// is a renderer-internal concern and is intentionally excluded from this
// contract. No `TreeProps` struct exists; renderers deserialize Tree
// nodes without a props struct.

/// `TreeItem` props.
///
/// Slots (`title` required, `subtitle` / `start` / `end` / `children`
/// optional) live on [`crate::ui_contract::node::UiNode::slots`]. The
/// stable `id` lives on [`crate::ui_contract::node::UiNode::id`] (the
/// envelope) rather than under props, per the UiNode shape in
/// `docs/specs/cross-client-ui-primitives.md`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeItemProps {
    /// Expansion state (controlled if explicit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded: Option<bool>,
    /// Selection state (controlled if explicit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    /// Notification / attention indicator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<bool>,
    /// Primary action emitted on activation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<UiAction>,
}

// =========================================================================
// Wire protocol composite primitives.
//
// These primitives are data-driven: they carry no children, no slots. Each
// reads from the client-side entity store (session, workspace, …) and
// expands into the same flat tree the current hub-rendered layout used to ship.
// Both renderers (web React, ratatui TUI) consume the same wire shape.
// =========================================================================

/// `SessionList` props — the replacement for the per-broadcast workspace
/// surface tree. Reads sessions, workspaces, and presentation state from
/// the client-side entity stores; renders the workspace-grouped tree, empty
/// state, hosted-preview indicators, and the New Session button.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListProps {
    /// Surface density (`sidebar` / `panel`). Defaults to `panel` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<UiValue<UiSurfaceDensity>>,
    /// Grouping mode. Defaults to `workspace` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouping: Option<UiSessionListGrouping>,
    /// When `true` (and density is `sidebar`), append plugin-registered nav
    /// entries after the session tree. The default is the surface-aware
    /// behaviour (`true` only for sidebar).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_nav_entries: Option<bool>,
}

/// `WorkspaceList` props — renders the bare list of workspaces (without the
/// session children join). Used by surfaces that need a workspace switcher
/// independent of the session tree.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListProps {
    /// Surface density (`sidebar` / `panel`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<UiValue<UiSurfaceDensity>>,
}

/// `SpawnTargetList` props — renders the configured spawn targets.
///
/// `on_select` and `on_remove` are **action templates**: their `id` (and
/// optionally `payload`) are emitted with the per-row `target_id` merged
/// in by the renderer. When omitted, the composite uses default action ids
/// (`botster.spawn_target.select`, `botster.spawn_target.remove`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnTargetListProps {
    /// Action template emitted when a target row is activated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_select: Option<UiAction>,
    /// Action template emitted when a target row's remove control fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_remove: Option<UiAction>,
}

/// `WorktreeList` props — renders the worktrees for one spawn target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeListProps {
    /// Required: the spawn target whose worktrees should be listed.
    pub target_id: String,
}

/// `SessionRow` props — single-row variant of `SessionList` for surfaces
/// that need to render one specific session (e.g. a header row inside a
/// session-scoped surface).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRowProps {
    /// Required: the session_uuid the row should bind to.
    pub session_uuid: String,
    /// Surface density. Defaults to `panel`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<UiValue<UiSurfaceDensity>>,
}

/// `HubRecoveryState` props — renders the hub lifecycle banner. Reads the
/// `hub` singleton entity (id = hub_id) from the client store; carries no
/// props in the typical case.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubRecoveryStateProps {}

/// `ConnectionCode` props — renders the QR code + URL for hub pairing. Reads
/// the `connection_code` singleton entity from the client store.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionCodeProps {}

/// `NewSessionButton` props — the "+" button that opens the new-session
/// chooser. Lifted into its own composite so both renderers stay parity-free
/// when the chooser UX evolves (button label / icon / preset-selector
/// substitutions can land here without rebroadcasting trees).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionButtonProps {
    /// Required: the action emitted on press.
    pub action: UiAction,
}

/// `Dialog` props.
///
/// Deferred from the Lua-public current inventory per
/// `docs/specs/web-ui-primitives-runtime.md`, but the primitive is registered
/// so renderers can adopt it when Phase B / Phase C is ready.
///
/// Cross-client defines only `open` and `title`; `presentation` is an extension
/// from `docs/specs/adaptive-ui-viewport-and-presentation.md` and defaults to
/// `auto` when the Lua constructor is used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogProps {
    /// Whether the dialog is open.
    pub open: bool,
    /// Dialog title text.
    pub title: String,
    /// Presentation policy (adaptive-spec extension).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<UiPresentation>,
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::assertions_on_result_states,
        reason = "test-code brevity: `assert!(result.is_err())` is a standard negative-case idiom here"
    )]

    use super::*;
    use crate::ui_contract::node::{UiResponsive, UiResponsiveWidth};
    use serde_json::json;

    // ---------- Stack ----------

    #[test]
    fn stack_requires_direction() {
        let err = serde_json::from_value::<StackProps>(json!({ "gap": "2" }));
        assert!(err.is_err(), "Stack must require `direction`");
    }

    #[test]
    fn stack_round_trip_scalar_direction() {
        let p = StackProps {
            direction: UiValue::scalar(UiStackDirection::Vertical),
            gap: Some(UiValue::scalar(UiSpace::Two)),
            align: Some(UiValue::scalar(UiAlign::Start)),
            justify: Some(UiValue::scalar(UiJustify::Between)),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(
            v,
            json!({
                "direction": "vertical",
                "gap": "2",
                "align": "start",
                "justify": "between"
            })
        );
        let back: StackProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn stack_round_trip_responsive_direction() {
        let p = StackProps {
            direction: UiValue::Responsive(UiResponsive::Responsive {
                width: Some(UiResponsiveWidth {
                    compact: Some(UiStackDirection::Vertical),
                    expanded: Some(UiStackDirection::Horizontal),
                    ..Default::default()
                }),
                height: None,
            }),
            gap: None,
            align: None,
            justify: None,
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(
            v,
            json!({
                "direction": {
                    "$kind": "responsive",
                    "width": { "compact": "vertical", "expanded": "horizontal" }
                }
            })
        );
        let back: StackProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn stack_padding_is_not_accepted_as_a_field() {
        // Extra keys on the wire are ignored by default; verify padding does
        // NOT round-trip as a structural field on StackProps.
        let p = StackProps {
            direction: UiValue::scalar(UiStackDirection::Vertical),
            gap: None,
            align: None,
            justify: None,
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert!(
            v.get("padding").is_none(),
            "StackProps must not emit `padding`: {v}"
        );
    }

    // ---------- Inline ----------

    #[test]
    fn inline_round_trip() {
        let p = InlineProps {
            gap: Some(UiValue::scalar(UiSpace::One)),
            align: Some(UiValue::scalar(UiAlign::Center)),
            justify: Some(UiValue::scalar(UiJustify::Start)),
            wrap: Some(true),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(
            v,
            json!({ "gap": "1", "align": "center", "justify": "start", "wrap": true })
        );
        let back: InlineProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn inline_padding_is_not_accepted() {
        let p = InlineProps::default();
        let v = serde_json::to_value(&p).expect("serialize");
        assert!(v.get("padding").is_none());
    }

    // ---------- Panel ----------

    #[test]
    fn panel_round_trip_all_fields() {
        let p = PanelProps {
            title: Some("Preview error".into()),
            tone: Some(UiPanelTone::Muted),
            border: Some(true),
            interaction_density: Some(UiValue::scalar(UiInteractionDensity::Comfortable)),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(
            v,
            json!({
                "title": "Preview error",
                "tone": "muted",
                "border": true,
                "interactionDensity": "comfortable"
            })
        );
        let back: PanelProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn panel_rejects_web_only_fields_on_output() {
        let p = PanelProps {
            title: Some("x".into()),
            ..Default::default()
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert!(
            v.get("padding").is_none(),
            "Panel must not emit web-only `padding`"
        );
        assert!(
            v.get("radius").is_none(),
            "Panel must not emit web-only `radius`"
        );
    }

    // ---------- ScrollArea ----------

    #[test]
    fn scroll_area_round_trip() {
        let p = ScrollAreaProps {
            axis: Some(UiScrollAxis::Both),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(v, json!({ "axis": "both" }));
        let back: ScrollAreaProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn scroll_area_default_is_empty_object() {
        let v = serde_json::to_value(ScrollAreaProps::default()).expect("serialize");
        assert_eq!(v, json!({}));
    }

    // ---------- Text ----------

    #[test]
    fn text_requires_text() {
        let err = serde_json::from_value::<TextProps>(json!({ "tone": "accent" }));
        assert!(err.is_err());
    }

    #[test]
    fn text_round_trip() {
        let p = TextProps {
            text: "Hello".into(),
            tone: Some(UiTone::Accent),
            size: Some(UiSize::Sm),
            weight: Some(UiTextWeight::Medium),
            monospace: Some(true),
            italic: Some(false),
            truncate: Some(false),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        let back: TextProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- Icon ----------

    #[test]
    fn icon_requires_name() {
        let err = serde_json::from_value::<IconProps>(json!({}));
        assert!(err.is_err());
    }

    #[test]
    fn icon_round_trip() {
        let p = IconProps {
            name: "workspace".into(),
            size: Some(UiSize::Sm),
            tone: Some(UiTone::Muted),
            label: Some("Workspaces".into()),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        let back: IconProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- Badge ----------

    #[test]
    fn badge_requires_text() {
        let err = serde_json::from_value::<BadgeProps>(json!({}));
        assert!(err.is_err());
    }

    #[test]
    fn badge_round_trip() {
        let p = BadgeProps {
            text: "3".into(),
            tone: Some(UiBadgeTone::Warning),
            size: Some(UiBadgeSize::Sm),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(v, json!({ "text": "3", "tone": "warning", "size": "sm" }));
        let back: BadgeProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- StatusDot ----------

    #[test]
    fn status_dot_requires_state() {
        let err = serde_json::from_value::<StatusDotProps>(json!({}));
        assert!(err.is_err());
    }

    #[test]
    fn status_dot_round_trip() {
        let p = StatusDotProps {
            state: UiStatusDotState::Active,
            label: Some("Running".into()),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        let back: StatusDotProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- EmptyState ----------

    #[test]
    fn empty_state_requires_title() {
        let err = serde_json::from_value::<EmptyStateProps>(json!({}));
        assert!(err.is_err());
    }

    #[test]
    fn empty_state_round_trip() {
        let p = EmptyStateProps {
            title: "No sessions yet".into(),
            description: Some("Spawn an agent to get started.".into()),
            icon: Some("sparkle".into()),
            primary_action: Some(UiAction::new("botster.session.create.request")),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(
            v,
            json!({
                "title": "No sessions yet",
                "description": "Spawn an agent to get started.",
                "icon": "sparkle",
                "primaryAction": { "id": "botster.session.create.request" }
            })
        );
        let back: EmptyStateProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- Button ----------

    #[test]
    fn button_requires_label_and_action() {
        assert!(serde_json::from_value::<ButtonProps>(json!({ "action": { "id": "x" } })).is_err());
        assert!(serde_json::from_value::<ButtonProps>(json!({ "label": "Go" })).is_err());
    }

    #[test]
    fn button_round_trip() {
        let p = ButtonProps {
            label: "Save".into(),
            action: UiAction::new("botster.workspace.save"),
            variant: Some(UiButtonVariant::Solid),
            tone: Some(UiButtonTone::Accent),
            icon: Some("check".into()),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(
            v,
            json!({
                "label": "Save",
                "action": { "id": "botster.workspace.save" },
                "variant": "solid",
                "tone": "accent",
                "icon": "check"
            })
        );
        let back: ButtonProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn button_does_not_emit_disabled_or_leading_icon() {
        let p = ButtonProps {
            label: "x".into(),
            action: UiAction::new("x"),
            variant: None,
            tone: None,
            icon: None,
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert!(v.get("disabled").is_none(), "Button must not emit disabled");
        assert!(
            v.get("leadingIcon").is_none(),
            "Button must not emit leadingIcon"
        );
    }

    // ---------- IconButton ----------

    #[test]
    fn icon_button_requires_icon_label_action() {
        for required in ["icon", "label", "action"] {
            let mut obj = json!({
                "icon": "close",
                "label": "Close",
                "action": { "id": "botster.session.close.request" }
            });
            obj.as_object_mut().expect("object").remove(required);
            assert!(
                serde_json::from_value::<IconButtonProps>(obj).is_err(),
                "icon_button should require `{required}`"
            );
        }
    }

    #[test]
    fn icon_button_round_trip() {
        let p = IconButtonProps {
            icon: "close".into(),
            label: "Close session".into(),
            action: UiAction::new("botster.session.close.request"),
            tone: Some(UiButtonTone::Danger),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        let back: IconButtonProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn icon_button_does_not_emit_disabled() {
        let p = IconButtonProps {
            icon: "x".into(),
            label: "x".into(),
            action: UiAction::new("x"),
            tone: None,
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert!(v.get("disabled").is_none());
    }

    // ---------- Tree: intentionally no TreeProps struct ----------

    // ---------- TreeItem ----------

    #[test]
    fn tree_item_props_round_trip() {
        // id is on UiNode::id, not in TreeItemProps.
        let p = TreeItemProps {
            expanded: Some(true),
            selected: Some(false),
            notification: Some(true),
            action: Some(UiAction::new("botster.workspace.toggle")),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        let back: TreeItemProps = serde_json::from_value(v.clone()).expect("deserialize");
        assert_eq!(back, p);
        // id is NOT a Props field.
        assert!(v.get("id").is_none());
    }

    // ---------- Dialog ----------

    #[test]
    fn dialog_requires_open_and_title() {
        assert!(serde_json::from_value::<DialogProps>(json!({ "open": true })).is_err());
        assert!(serde_json::from_value::<DialogProps>(json!({ "title": "x" })).is_err());
    }

    // ---------- SessionList ----------

    #[test]
    fn session_list_default_round_trip_is_empty_object() {
        let v = serde_json::to_value(SessionListProps::default()).expect("serialize");
        assert_eq!(v, json!({}));
    }

    #[test]
    fn session_list_round_trip_all_fields() {
        let p = SessionListProps {
            density: Some(UiValue::scalar(UiSurfaceDensity::Sidebar)),
            grouping: Some(UiSessionListGrouping::Workspace),
            show_nav_entries: Some(true),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(
            v,
            json!({ "density": "sidebar", "grouping": "workspace", "showNavEntries": true })
        );
        let back: SessionListProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn session_list_density_supports_responsive() {
        let p = SessionListProps {
            density: Some(UiValue::Responsive(UiResponsive::Responsive {
                width: Some(UiResponsiveWidth {
                    compact: Some(UiSurfaceDensity::Sidebar),
                    expanded: Some(UiSurfaceDensity::Panel),
                    ..Default::default()
                }),
                height: None,
            })),
            ..Default::default()
        };
        let v = serde_json::to_value(&p).expect("serialize");
        let back: SessionListProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- WorkspaceList ----------

    #[test]
    fn workspace_list_round_trip() {
        let p = WorkspaceListProps {
            density: Some(UiValue::scalar(UiSurfaceDensity::Panel)),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(v, json!({ "density": "panel" }));
        let back: WorkspaceListProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- SpawnTargetList ----------

    #[test]
    fn spawn_target_list_default_omits_actions() {
        let v = serde_json::to_value(SpawnTargetListProps::default()).expect("serialize");
        assert_eq!(v, json!({}));
    }

    #[test]
    fn spawn_target_list_round_trip_with_action_templates() {
        let p = SpawnTargetListProps {
            on_select: Some(UiAction::new("custom.target.select")),
            on_remove: Some(UiAction::new("custom.target.remove")),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(
            v,
            json!({
                "onSelect": { "id": "custom.target.select" },
                "onRemove": { "id": "custom.target.remove" }
            })
        );
        let back: SpawnTargetListProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- WorktreeList ----------

    #[test]
    fn worktree_list_requires_target_id() {
        let err = serde_json::from_value::<WorktreeListProps>(json!({}));
        assert!(err.is_err(), "WorktreeList must require target_id");
    }

    #[test]
    fn worktree_list_round_trip() {
        let p = WorktreeListProps {
            target_id: "target-abc".into(),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(v, json!({ "targetId": "target-abc" }));
        let back: WorktreeListProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- SessionRow ----------

    #[test]
    fn session_row_requires_session_uuid() {
        let err = serde_json::from_value::<SessionRowProps>(json!({}));
        assert!(err.is_err(), "SessionRow must require session_uuid");
    }

    #[test]
    fn session_row_round_trip() {
        let p = SessionRowProps {
            session_uuid: "sess-abc".into(),
            density: Some(UiValue::scalar(UiSurfaceDensity::Sidebar)),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(
            v,
            json!({ "sessionUuid": "sess-abc", "density": "sidebar" })
        );
        let back: SessionRowProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- HubRecoveryState ----------

    #[test]
    fn hub_recovery_state_round_trip_is_empty_object() {
        let p = HubRecoveryStateProps::default();
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(v, json!({}));
        let back: HubRecoveryStateProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- ConnectionCode ----------

    #[test]
    fn connection_code_round_trip_is_empty_object() {
        let p = ConnectionCodeProps::default();
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(v, json!({}));
        let back: ConnectionCodeProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- NewSessionButton ----------

    #[test]
    fn new_session_button_requires_action() {
        let err = serde_json::from_value::<NewSessionButtonProps>(json!({}));
        assert!(err.is_err(), "NewSessionButton must require action");
    }

    #[test]
    fn new_session_button_round_trip() {
        let p = NewSessionButtonProps {
            action: UiAction::new("botster.session.create.request"),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(
            v,
            json!({ "action": { "id": "botster.session.create.request" } })
        );
        let back: NewSessionButtonProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }

    // ---------- Dialog ----------

    #[test]
    fn dialog_round_trip_with_presentation() {
        let p = DialogProps {
            open: true,
            title: "Rename Workspace".into(),
            presentation: Some(UiPresentation::Auto),
        };
        let v = serde_json::to_value(&p).expect("serialize");
        assert_eq!(
            v,
            json!({ "open": true, "title": "Rename Workspace", "presentation": "auto" })
        );
        let back: DialogProps = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back, p);
    }
}
