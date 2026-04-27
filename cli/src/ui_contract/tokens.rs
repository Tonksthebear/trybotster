//! Shared scalar tokens used across current primitives.
//!
//! Every enum in this module serializes to the exact lowercase string the spec
//! calls for, so `serde_json` output matches the TypeScript types in
//! `docs/specs/cross-client-ui-primitives.md` and
//! `docs/specs/web-ui-primitives-runtime.md` byte-for-byte.

use serde::{Deserialize, Serialize};

/// Shared tone vocabulary (`UiTone`).
///
/// Both renderers agree on what these mean semantically; visual realization is
/// renderer-specific (Tailwind palette on web, ratatui styles in the TUI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiTone {
    /// No tonal emphasis.
    Default,
    /// De-emphasized content.
    Muted,
    /// Positive / highlighted content.
    Accent,
    /// Successful state.
    Success,
    /// Caution / warning state.
    Warning,
    /// Error / destructive state.
    Danger,
}

/// Cross-axis alignment (`UiAlign`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiAlign {
    /// Align to the start of the cross axis.
    Start,
    /// Center on the cross axis.
    Center,
    /// Align to the end of the cross axis.
    End,
    /// Stretch to fill the cross axis.
    Stretch,
}

/// Main-axis distribution for `stack` / `inline`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiJustify {
    /// Pack items at the start of the axis.
    Start,
    /// Pack items in the center of the axis.
    Center,
    /// Pack items at the end of the axis.
    End,
    /// Distribute items with equal gaps between them.
    Between,
}

/// Shared interaction-density token (`UiInteractionDensity`).
///
/// Intentionally distinct from `UiDensity`, which is the web-only
/// phase-1 surface variant (`sidebar` | `panel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiInteractionDensity {
    /// Tighter typography and smaller hit targets.
    Compact,
    /// Larger hit targets suited to coarse pointers.
    Comfortable,
}

/// Text / icon size token (`xs` | `sm` | `md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiSize {
    /// Extra small.
    Xs,
    /// Small.
    Sm,
    /// Medium (default body text).
    Md,
}

/// Spacing scale used by `gap` and `padding` props (`0` | `1` | `2` | `3` | `4` | `6`).
///
/// These values are string-valued in the spec so they can round-trip cleanly
/// through JSON even on systems that coerce numeric JSON values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UiSpace {
    /// `"0"` — no gap.
    #[serde(rename = "0")]
    Zero,
    /// `"1"` — 1 unit of space.
    #[serde(rename = "1")]
    One,
    /// `"2"` — 2 units.
    #[serde(rename = "2")]
    Two,
    /// `"3"` — 3 units.
    #[serde(rename = "3")]
    Three,
    /// `"4"` — 4 units.
    #[serde(rename = "4")]
    Four,
    /// `"6"` — 6 units.
    #[serde(rename = "6")]
    Six,
}

/// Text weight token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiTextWeight {
    /// Regular weight.
    Regular,
    /// Medium weight.
    Medium,
    /// Semibold weight.
    Semibold,
}

/// Stack direction (horizontal = `inline`-like row, vertical = column).
///
/// Used by the `Stack` primitive even though `Inline` is its own type. The
/// cross-client spec deliberately exposes `stack.direction` as a shared
/// semantic so the TUI can translate into its internal `HSplit` / `VSplit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiStackDirection {
    /// Vertical (column).
    Vertical,
    /// Horizontal (row).
    Horizontal,
}

/// Scroll axis for `ScrollArea`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiScrollAxis {
    /// Vertical only.
    Y,
    /// Horizontal only.
    X,
    /// Both axes.
    Both,
}

/// Panel tone (subset of `UiTone`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiPanelTone {
    /// Default panel background.
    Default,
    /// Muted / de-emphasized panel background.
    Muted,
}

/// Badge tone (superset of `UiPanelTone`, subset of `UiTone`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiBadgeTone {
    /// Default tone.
    Default,
    /// Accent tone.
    Accent,
    /// Success tone.
    Success,
    /// Warning tone.
    Warning,
    /// Danger tone.
    Danger,
}

/// Badge size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiBadgeSize {
    /// Small badge.
    Sm,
    /// Medium badge.
    Md,
}

/// StatusDot state vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiStatusDotState {
    /// Inactive / neutral.
    Neutral,
    /// Idle but connected.
    Idle,
    /// Currently active.
    Active,
    /// Success state.
    Success,
    /// Warning state.
    Warning,
    /// Danger / error state.
    Danger,
}

/// Button visual variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiButtonVariant {
    /// Filled button.
    Solid,
    /// Ghost / transparent button.
    Ghost,
}

/// Button tone (subset of `UiTone`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiButtonTone {
    /// Default tone.
    Default,
    /// Accent tone.
    Accent,
    /// Destructive tone.
    Danger,
}

/// Surface-density token for composite primitives (`session_list`,
/// `workspace_list`, `session_row`).
///
/// Distinct from [`UiInteractionDensity`] — that one is renderer-internal
/// (compact / comfortable hit targets). This one is the public, cross-client
/// surface variant: `sidebar` (xs typography, no workspace count) versus
/// `panel` (sm typography, count visible). Both renderers honor it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiSurfaceDensity {
    /// Sidebar density — tighter typography, no count.
    Sidebar,
    /// Panel density — standard typography, count shown.
    Panel,
}

/// Grouping mode for `session_list`.
///
/// `workspace` (default) groups sessions under their workspace headers and
/// renders an "ungrouped" bucket for sessions without a workspace.
/// `flat` skips the grouping pass and renders one row per session in
/// insertion order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiSessionListGrouping {
    /// Group rows under workspace headers.
    Workspace,
    /// One row per session, no grouping.
    Flat,
}

/// Dialog presentation policy (`UiPresentation`).
///
/// `auto` lets the renderer pick the best native presentation based on the
/// current `UiViewport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiPresentation {
    /// Renderer chooses.
    Auto,
    /// Render inline at author position.
    Inline,
    /// Render as a centered overlay.
    Overlay,
    /// Render as a bottom / side sheet.
    Sheet,
    /// Render full-screen.
    Fullscreen,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&UiTone::Accent).expect("serialize"),
            "\"accent\""
        );
        assert_eq!(
            serde_json::to_string(&UiTone::Danger).expect("serialize"),
            "\"danger\""
        );
    }

    #[test]
    fn space_serializes_as_digit_string() {
        assert_eq!(
            serde_json::to_string(&UiSpace::Zero).expect("serialize"),
            "\"0\""
        );
        assert_eq!(
            serde_json::to_string(&UiSpace::Six).expect("serialize"),
            "\"6\""
        );
        let round: UiSpace = serde_json::from_str("\"3\"").expect("round-trip UiSpace from \"3\"");
        assert_eq!(round, UiSpace::Three);
    }

    #[test]
    fn stack_direction_roundtrips() {
        let s = serde_json::to_string(&UiStackDirection::Horizontal).expect("serialize");
        assert_eq!(s, "\"horizontal\"");
        let back: UiStackDirection = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, UiStackDirection::Horizontal);
    }

    #[test]
    fn presentation_roundtrips() {
        for value in [
            UiPresentation::Auto,
            UiPresentation::Inline,
            UiPresentation::Overlay,
            UiPresentation::Sheet,
            UiPresentation::Fullscreen,
        ] {
            let s = serde_json::to_string(&value).expect("serialize");
            let back: UiPresentation = serde_json::from_str(&s).expect("deserialize");
            assert_eq!(back, value);
        }
    }
}
