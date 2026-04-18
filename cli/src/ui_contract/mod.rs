//! Cross-client UI contract (v1) shared by the TUI and web renderers.
//!
//! This module is the Rust-side source of truth for the Botster UI DSL described
//! in these specs:
//!
//! - `docs/specs/cross-client-ui-primitives.md`
//! - `docs/specs/adaptive-ui-viewport-and-presentation.md`
//! - `docs/specs/web-ui-primitives-runtime.md`
//!
//! # Layers
//!
//! 1. [`tokens`] — scalar tokens used across primitives (`UiTone`, `UiAlign`,
//!    `UiSpace`, `UiSize`, `UiInteractionDensity`, …).
//! 2. [`viewport`] — the `UiViewportV1` context produced by renderers and the
//!    enums it references (`UiWidthClass`, `UiHeightClass`, `UiPointer`,
//!    `UiOrientation`).
//! 3. [`node`] — the core wire types: [`UiNodeV1`], [`UiActionV1`],
//!    [`UiCapabilitySetV1`], the `UiResponsiveV1<T>` sentinel, and the
//!    `UiConditionV1` struct used by `ui.when` / `ui.hidden`.
//! 4. [`props`] — strongly-typed Props structs for every Lua-public v1 primitive
//!    and for the internal-only `Dialog` primitive.
//! 5. [`lua`] — Lua DSL registration. Exposes `ui.*` as a global table in both
//!    the hub `LuaRuntime` and the TUI `LayoutLua`.
//!
//! # Wire format
//!
//! All types serialize as JSON that matches the TypeScript shapes defined in
//! the specs (camelCase field names). The module is intentionally
//! renderer-agnostic: it defines the contract but does not render anything.

pub mod lua;
pub mod node;
pub mod props;
pub mod tokens;
pub mod viewport;

pub use node::{
    UiActionV1, UiCapabilitySetV1, UiChildV1, UiConditionV1, UiConditionalV1, UiNodeV1,
    UiResponsiveHeightV1, UiResponsiveV1, UiResponsiveWidthV1, UiValueV1,
};
pub use props::{
    BadgePropsV1, ButtonPropsV1, DialogPropsV1, EmptyStatePropsV1, IconButtonPropsV1, IconPropsV1,
    InlinePropsV1, PanelPropsV1, ScrollAreaPropsV1, StackPropsV1, StatusDotPropsV1, TextPropsV1,
    TreeItemPropsV1,
};
pub use tokens::{
    UiAlign, UiBadgeSize, UiBadgeTone, UiButtonTone, UiButtonVariant, UiInteractionDensity,
    UiJustify, UiPanelTone, UiPresentation, UiScrollAxis, UiSize, UiSpace, UiStackDirection,
    UiStatusDotState, UiTextWeight, UiTone,
};
pub use viewport::{UiHeightClass, UiOrientation, UiPointer, UiViewportV1, UiWidthClass};
