//! Terminal → [`UiViewportV1`] derivation for the TUI adapter.
//!
//! The cross-client viewport spec intentionally hides raw pixels / columns
//! behind semantic classes (`compact` / `regular` / `expanded`,
//! `short` / `regular` / `tall`). The TUI renderer owns the exact column /
//! row thresholds; this module is where that ownership lives.
//!
//! Thresholds come from the spec's "Recommended Default Thresholds" section
//! in `docs/specs/adaptive-ui-viewport-and-presentation.md`:
//!
//! - width: `compact < 80`, `regular 80..=119`, `expanded >= 120`
//! - height: `short < 24`, `regular 24..=39`, `tall >= 40`

// Rust guideline compliant 2026-04-18

use crate::ui_contract::viewport::{UiHeightClass, UiPointer, UiViewportV1, UiWidthClass};

/// Column below which the terminal is considered [`UiWidthClass::Compact`].
///
/// Matches the spec's "TUI defaults" — 80 columns is the classic VT100
/// minimum, so anything narrower is treated as single-column.
const COMPACT_WIDTH_MAX: u16 = 80;

/// Column at which the terminal becomes [`UiWidthClass::Expanded`] (split
/// panes become sensible).
///
/// Matches the spec's "TUI defaults" — 120 columns fits a two-pane layout
/// with reasonable per-pane widths.
const EXPANDED_WIDTH_MIN: u16 = 120;

/// Row below which the terminal is considered [`UiHeightClass::Short`].
///
/// Matches the spec's "TUI defaults" — 24 rows is the classic VT100 minimum.
const SHORT_HEIGHT_MAX: u16 = 24;

/// Row at which the terminal becomes [`UiHeightClass::Tall`].
///
/// Matches the spec's "TUI defaults" — 40 rows leaves room for rich
/// overlays.
const TALL_HEIGHT_MIN: u16 = 40;

/// Derive a [`UiViewportV1`] from a terminal's reported dimensions.
///
/// # Arguments
///
/// * `cols` — terminal columns (width).
/// * `rows` — terminal rows (height).
/// * `supports_mouse` — whether the terminal host advertises mouse support.
///   When `true`, the viewport's `pointer` is reported as
///   [`UiPointer::Coarse`] (terminal mouse precision is coarse by nature);
///   otherwise it is [`UiPointer::None`], matching the spec's recommendation
///   that TUI pointer is usually `none`.
///
/// `orientation` and `keyboardOccluded` are intentionally omitted — neither
/// is meaningful for a terminal emulator.
#[must_use]
pub fn derive_viewport_from_terminal(
    cols: u16,
    rows: u16,
    supports_mouse: bool,
) -> UiViewportV1 {
    let width_class = width_class_for_cols(cols);
    let height_class = height_class_for_rows(rows);
    let pointer = if supports_mouse {
        UiPointer::Coarse
    } else {
        UiPointer::None
    };
    UiViewportV1::new(width_class, height_class, pointer)
}

/// Map terminal columns to [`UiWidthClass`].
#[must_use]
pub const fn width_class_for_cols(cols: u16) -> UiWidthClass {
    if cols < COMPACT_WIDTH_MAX {
        UiWidthClass::Compact
    } else if cols < EXPANDED_WIDTH_MIN {
        UiWidthClass::Regular
    } else {
        UiWidthClass::Expanded
    }
}

/// Map terminal rows to [`UiHeightClass`].
#[must_use]
pub const fn height_class_for_rows(rows: u16) -> UiHeightClass {
    if rows < SHORT_HEIGHT_MAX {
        UiHeightClass::Short
    } else if rows < TALL_HEIGHT_MIN {
        UiHeightClass::Regular
    } else {
        UiHeightClass::Tall
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_when_below_80_cols() {
        assert_eq!(width_class_for_cols(0), UiWidthClass::Compact);
        assert_eq!(width_class_for_cols(40), UiWidthClass::Compact);
        assert_eq!(width_class_for_cols(79), UiWidthClass::Compact);
    }

    #[test]
    fn regular_when_80_to_119_cols() {
        assert_eq!(width_class_for_cols(80), UiWidthClass::Regular);
        assert_eq!(width_class_for_cols(100), UiWidthClass::Regular);
        assert_eq!(width_class_for_cols(119), UiWidthClass::Regular);
    }

    #[test]
    fn expanded_when_120_plus_cols() {
        assert_eq!(width_class_for_cols(120), UiWidthClass::Expanded);
        assert_eq!(width_class_for_cols(200), UiWidthClass::Expanded);
    }

    #[test]
    fn short_when_below_24_rows() {
        assert_eq!(height_class_for_rows(0), UiHeightClass::Short);
        assert_eq!(height_class_for_rows(12), UiHeightClass::Short);
        assert_eq!(height_class_for_rows(23), UiHeightClass::Short);
    }

    #[test]
    fn regular_when_24_to_39_rows() {
        assert_eq!(height_class_for_rows(24), UiHeightClass::Regular);
        assert_eq!(height_class_for_rows(30), UiHeightClass::Regular);
        assert_eq!(height_class_for_rows(39), UiHeightClass::Regular);
    }

    #[test]
    fn tall_when_40_plus_rows() {
        assert_eq!(height_class_for_rows(40), UiHeightClass::Tall);
        assert_eq!(height_class_for_rows(80), UiHeightClass::Tall);
    }

    #[test]
    fn derive_viewport_defaults_to_pointer_none() {
        let v = derive_viewport_from_terminal(100, 30, false);
        assert_eq!(v.width_class, UiWidthClass::Regular);
        assert_eq!(v.height_class, UiHeightClass::Regular);
        assert_eq!(v.pointer, UiPointer::None);
        assert!(v.orientation.is_none());
        assert!(v.keyboard_occluded.is_none());
    }

    #[test]
    fn derive_viewport_reports_coarse_pointer_when_mouse_supported() {
        let v = derive_viewport_from_terminal(100, 30, true);
        assert_eq!(v.pointer, UiPointer::Coarse);
    }

    #[test]
    fn derive_viewport_compact_short_corner() {
        let v = derive_viewport_from_terminal(60, 20, false);
        assert_eq!(v.width_class, UiWidthClass::Compact);
        assert_eq!(v.height_class, UiHeightClass::Short);
    }

    #[test]
    fn derive_viewport_expanded_tall_corner() {
        let v = derive_viewport_from_terminal(200, 60, false);
        assert_eq!(v.width_class, UiWidthClass::Expanded);
        assert_eq!(v.height_class, UiHeightClass::Tall);
    }
}
