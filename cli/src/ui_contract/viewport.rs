//! `UiViewport` and its semantic viewport classes.
//!
//! Both renderers produce a `UiViewport` per render pass:
//!
//! - the web runtime derives it from browser viewport size / input mode / visual viewport
//! - the TUI runtime derives it from terminal columns and rows
//!
//! Authored surfaces consume it through `ctx.viewport.*` in Lua. The exact
//! pixel / column thresholds live in the renderer; the contract only exposes
//! stable semantic classes.

use serde::{Deserialize, Serialize};

/// Width class — what the content area feels like, not the device name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiWidthClass {
    /// Single column, no persistent secondary pane.
    Compact,
    /// Moderate multi-region layouts.
    Regular,
    /// Split-pane layouts, persistent nav / detail.
    Expanded,
}

/// Height class — usable rows / CSS pixels on the cross axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiHeightClass {
    /// Avoid tall overlays, shrink chrome.
    Short,
    /// Standard behavior.
    Regular,
    /// Richer overlays permitted.
    Tall,
}

/// Input pointer precision.
///
/// Supersedes the older boolean hover capability; `UiCapabilitySet.hover`
/// remains separate because some pointer devices do not expose hover events
/// (e.g., touch with stylus hover).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiPointer {
    /// No pointer input available (keyboard-only).
    None,
    /// Coarse pointer (touch / trackpad with poor precision).
    Coarse,
    /// Fine pointer (mouse).
    Fine,
}

/// Screen orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiOrientation {
    /// Taller than wide.
    Portrait,
    /// Wider than tall.
    Landscape,
}

/// Full renderer-neutral viewport context (`UiViewport`).
///
/// Renderers build one of these each render pass and hand it to authored
/// surfaces. Fields map 1:1 to the TS type in
/// `docs/specs/adaptive-ui-viewport-and-presentation.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiViewport {
    /// Content-area width class.
    pub width_class: UiWidthClass,
    /// Content-area height class.
    pub height_class: UiHeightClass,
    /// Input pointer precision.
    pub pointer: UiPointer,
    /// Screen orientation, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<UiOrientation>,
    /// Whether an on-screen keyboard currently occludes part of the viewport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard_occluded: Option<bool>,
}

impl UiViewport {
    /// Construct a viewport with no orientation / keyboard hints.
    #[must_use]
    pub const fn new(
        width_class: UiWidthClass,
        height_class: UiHeightClass,
        pointer: UiPointer,
    ) -> Self {
        Self {
            width_class,
            height_class,
            pointer,
            orientation: None,
            keyboard_occluded: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_serializes_camelcase() {
        let v = UiViewport {
            width_class: UiWidthClass::Compact,
            height_class: UiHeightClass::Short,
            pointer: UiPointer::Coarse,
            orientation: Some(UiOrientation::Portrait),
            keyboard_occluded: Some(true),
        };
        let s = serde_json::to_string(&v).expect("serialize");
        assert!(s.contains("\"widthClass\":\"compact\""), "got {s}");
        assert!(s.contains("\"heightClass\":\"short\""), "got {s}");
        assert!(s.contains("\"keyboardOccluded\":true"), "got {s}");
        let back: UiViewport = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, v);
    }

    #[test]
    fn viewport_omits_optional_fields() {
        let v = UiViewport::new(UiWidthClass::Regular, UiHeightClass::Tall, UiPointer::Fine);
        let s = serde_json::to_string(&v).expect("serialize");
        assert!(
            !s.contains("orientation"),
            "orientation should be omitted: {s}"
        );
        assert!(
            !s.contains("keyboardOccluded"),
            "keyboardOccluded should be omitted: {s}"
        );
    }
}
