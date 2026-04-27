//! Conversions from shared UI tokens to the TUI's local
//! [`crate::tui::render_tree::SpanStyle`] / [`StyledContent`] types.
//!
//! The shared vocabulary (`UiTone`, `UiSize`, `UiTextWeight`, …) has
//! coarser semantics than ratatui styling — e.g. there is no `xs` font in
//! a terminal and italic rendering varies. This module picks reasonable
//! terminal approximations and keeps them in one place so the mapping is
//! easy to audit.

// Rust guideline compliant 2026-04-18

use crate::tui::render_tree::{SpanColor, SpanStyle, StyledContent, StyledSpan};
use crate::ui_contract::tokens::{
    UiBadgeTone, UiButtonTone, UiStatusDotState, UiTextWeight, UiTone,
};

/// Glyph used as a [`UiStatusDotState`] indicator in the TUI.
///
/// The web renderer draws a real colored dot; in a terminal we use a
/// filled bullet so the tone color dominates. One glyph per state keeps
/// the visual vocabulary uniform.
const STATUS_DOT_GLYPH: &str = "\u{25CF}";

/// Prefix glyph used in front of focused buttons / action items to give
/// keyboard users a visible selection cue.
///
/// Chosen to match the existing TUI highlight style used elsewhere in the
/// codebase.
pub const BUTTON_HIGHLIGHT_SYMBOL: &str = "› ";

/// Convert a shared [`UiTone`] to the TUI's [`SpanColor`].
///
/// `default` maps to `None` so the terminal's default foreground shows
/// through; explicit tones map to named colors whose meaning matches the
/// cross-client spec.
#[must_use]
pub fn tone_color(tone: UiTone) -> Option<SpanColor> {
    match tone {
        UiTone::Default => None,
        UiTone::Muted => Some(SpanColor::Gray),
        UiTone::Accent => Some(SpanColor::Cyan),
        UiTone::Success => Some(SpanColor::Green),
        UiTone::Warning => Some(SpanColor::Yellow),
        UiTone::Danger => Some(SpanColor::Red),
    }
}

/// Convert a [`UiBadgeTone`] to a [`SpanColor`].
///
/// Shape matches [`tone_color`] — badges use the same palette minus
/// `muted`.
#[must_use]
pub fn badge_tone_color(tone: UiBadgeTone) -> Option<SpanColor> {
    match tone {
        UiBadgeTone::Default => None,
        UiBadgeTone::Accent => Some(SpanColor::Cyan),
        UiBadgeTone::Success => Some(SpanColor::Green),
        UiBadgeTone::Warning => Some(SpanColor::Yellow),
        UiBadgeTone::Danger => Some(SpanColor::Red),
    }
}

/// Convert a [`UiButtonTone`] to a [`SpanColor`].
#[must_use]
pub fn button_tone_color(tone: UiButtonTone) -> Option<SpanColor> {
    match tone {
        UiButtonTone::Default => None,
        UiButtonTone::Accent => Some(SpanColor::Cyan),
        UiButtonTone::Danger => Some(SpanColor::Red),
    }
}

/// Convert [`UiStatusDotState`] to a `(glyph, color)` pair.
///
/// The glyph is always the same filled bullet; the color carries the
/// semantic state.
#[must_use]
pub fn status_dot_color(state: UiStatusDotState) -> (&'static str, Option<SpanColor>) {
    let color = match state {
        UiStatusDotState::Neutral => Some(SpanColor::Gray),
        UiStatusDotState::Idle => Some(SpanColor::Blue),
        UiStatusDotState::Active => Some(SpanColor::Cyan),
        UiStatusDotState::Success => Some(SpanColor::Green),
        UiStatusDotState::Warning => Some(SpanColor::Yellow),
        UiStatusDotState::Danger => Some(SpanColor::Red),
    };
    (STATUS_DOT_GLYPH, color)
}

/// Build a [`SpanStyle`] for a run of text given the semantic text props.
///
/// - `tone` drives the foreground color (see [`tone_color`]).
/// - `weight` of `medium` / `semibold` sets bold.
/// - `italic` sets the italic modifier.
/// - `monospace` is a no-op in a terminal (everything is monospace) but
///   accepted so the Lua author's intent survives.
#[must_use]
pub fn text_span_style(
    tone: Option<UiTone>,
    weight: Option<UiTextWeight>,
    italic: bool,
) -> SpanStyle {
    let mut style = SpanStyle::default();
    if let Some(t) = tone {
        style.fg = tone_color(t);
    }
    if matches!(weight, Some(UiTextWeight::Medium | UiTextWeight::Semibold)) {
        style.bold = true;
    }
    if italic {
        style.italic = true;
    }
    style
}

/// Build a single styled span with the given text and style.
#[must_use]
pub fn single_span(text: impl Into<String>, style: SpanStyle) -> StyledContent {
    StyledContent::Styled(vec![StyledSpan {
        text: text.into(),
        style,
    }])
}

/// Concatenate multiple styled spans into a single styled line.
#[must_use]
pub fn join_spans(spans: Vec<StyledSpan>) -> StyledContent {
    StyledContent::Styled(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_default_returns_none() {
        assert!(tone_color(UiTone::Default).is_none());
    }

    #[test]
    fn tone_danger_is_red() {
        assert!(matches!(tone_color(UiTone::Danger), Some(SpanColor::Red)));
    }

    #[test]
    fn status_dot_always_uses_filled_bullet() {
        let (glyph, _) = status_dot_color(UiStatusDotState::Active);
        assert_eq!(glyph, STATUS_DOT_GLYPH);
    }

    #[test]
    fn text_style_weight_medium_sets_bold() {
        let s = text_span_style(None, Some(UiTextWeight::Medium), false);
        assert!(s.bold);
        assert!(!s.italic);
    }

    #[test]
    fn text_style_italic_flag_propagates() {
        let s = text_span_style(None, None, true);
        assert!(s.italic);
    }

    #[test]
    fn single_span_builds_styled_content() {
        let content = single_span("hi", SpanStyle::default());
        match content {
            StyledContent::Styled(spans) => {
                assert_eq!(spans.len(), 1);
                assert_eq!(spans[0].text, "hi");
            }
            StyledContent::Plain(_) => panic!("expected Styled, got Plain"),
        }
    }
}
