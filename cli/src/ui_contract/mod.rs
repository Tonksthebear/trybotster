//! Cross-client UI contract (current) shared by the TUI and web renderers.
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
//! 2. [`viewport`] — the `UiViewport` context produced by renderers and the
//!    enums it references (`UiWidthClass`, `UiHeightClass`, `UiPointer`,
//!    `UiOrientation`).
//! 3. [`node`] — the core wire types: [`UiNode`], [`UiAction`],
//!    [`UiCapabilitySet`], the `UiResponsive<T>` sentinel, and the
//!    `UiCondition` struct used by `ui.when` / `ui.hidden`.
//! 4. [`props`] — strongly-typed Props structs for every Lua-public current primitive
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
    UiAction, UiCapabilitySet, UiChild, UiCondition, UiConditional, UiNode, UiResponsive,
    UiResponsiveHeight, UiResponsiveWidth, UiValue,
};
pub use props::{
    BadgeProps, ButtonProps, ConnectionCodeProps, DialogProps, EmptyStateProps,
    HubRecoveryStateProps, IconButtonProps, IconProps, InlineProps, NewSessionButtonProps,
    PanelProps, ScrollAreaProps, SessionListProps, SessionRowProps, SpawnTargetListProps,
    StackProps, StatusDotProps, TextProps, TreeItemProps, WorkspaceListProps, WorktreeListProps,
};
pub use tokens::{
    UiAlign, UiBadgeSize, UiBadgeTone, UiButtonTone, UiButtonVariant, UiInteractionDensity,
    UiJustify, UiPanelTone, UiPresentation, UiScrollAxis, UiSessionListGrouping, UiSize, UiSpace,
    UiStackDirection, UiStatusDotState, UiSurfaceDensity, UiTextWeight, UiTone,
};
pub use viewport::{UiHeightClass, UiOrientation, UiPointer, UiViewport, UiWidthClass};
