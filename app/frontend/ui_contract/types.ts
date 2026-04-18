// TypeScript mirror of `cli/src/ui_contract/` (Phase A).
//
// These types MUST serialize byte-for-byte with the Rust wire format defined in
// `cli/src/ui_contract/{tokens,viewport,node,props}.rs`. They are the canonical
// TS shapes the web React renderer consumes.
//
// The cross-client spec (`docs/specs/cross-client-ui-primitives.md`) is the
// source of truth for prop shapes. Web-runtime-only extensions
// (Panel.padding/radius, Stack.padding, Button.leadingIcon/disabled, etc.)
// are intentionally NOT on these shared types.

// ---------- Scalar tokens ----------

export type UiToneV1 =
  | 'default'
  | 'muted'
  | 'accent'
  | 'success'
  | 'warning'
  | 'danger'

export type UiAlignV1 = 'start' | 'center' | 'end' | 'stretch'
export type UiJustifyV1 = 'start' | 'center' | 'end' | 'between'
export type UiInteractionDensityV1 = 'compact' | 'comfortable'
export type UiSizeV1 = 'xs' | 'sm' | 'md'
export type UiSpaceV1 = '0' | '1' | '2' | '3' | '4' | '6'
export type UiTextWeightV1 = 'regular' | 'medium' | 'semibold'
export type UiStackDirectionV1 = 'vertical' | 'horizontal'
export type UiScrollAxisV1 = 'y' | 'x' | 'both'
export type UiPanelToneV1 = 'default' | 'muted'
export type UiBadgeToneV1 =
  | 'default'
  | 'accent'
  | 'success'
  | 'warning'
  | 'danger'
export type UiBadgeSizeV1 = 'sm' | 'md'
export type UiStatusDotStateV1 =
  | 'neutral'
  | 'idle'
  | 'active'
  | 'success'
  | 'warning'
  | 'danger'
export type UiButtonVariantV1 = 'solid' | 'ghost'
export type UiButtonToneV1 = 'default' | 'accent' | 'danger'
export type UiPresentationV1 =
  | 'auto'
  | 'inline'
  | 'overlay'
  | 'sheet'
  | 'fullscreen'

// ---------- Viewport classes ----------

export type UiWidthClassV1 = 'compact' | 'regular' | 'expanded'
export type UiHeightClassV1 = 'short' | 'regular' | 'tall'
export type UiPointerV1 = 'none' | 'coarse' | 'fine'
export type UiOrientationV1 = 'portrait' | 'landscape'

export type UiViewportV1 = {
  widthClass: UiWidthClassV1
  heightClass: UiHeightClassV1
  pointer: UiPointerV1
  orientation?: UiOrientationV1
  keyboardOccluded?: boolean
}

// ---------- Responsive + conditional wire sentinels ----------

export type UiResponsiveWidthV1<T> = {
  compact?: T
  regular?: T
  expanded?: T
}

export type UiResponsiveHeightV1<T> = {
  short?: T
  regular?: T
  tall?: T
}

export type UiResponsiveV1<T> = {
  $kind: 'responsive'
  width?: UiResponsiveWidthV1<T>
  height?: UiResponsiveHeightV1<T>
}

/** Either a concrete `T` or a `$kind: "responsive"` sentinel. */
export type UiValueV1<T> = T | UiResponsiveV1<T>

export type UiConditionV1 = {
  width?: UiWidthClassV1
  height?: UiHeightClassV1
  pointer?: UiPointerV1
  orientation?: UiOrientationV1
  keyboardOccluded?: boolean
}

export type UiConditionalV1 =
  | { $kind: 'when'; condition: UiConditionV1; node: UiNodeV1 }
  | { $kind: 'hidden'; condition: UiConditionV1; node: UiNodeV1 }

// ---------- Core node + action + capabilities ----------

/** Arbitrary prop bag — primitive-specific Props types narrow this further. */
export type UiPropsV1 = Record<string, unknown>

export type UiNodeV1 = {
  type: string
  id?: string
  props?: UiPropsV1
  children?: UiChildV1[]
  slots?: Record<string, UiChildV1[]>
}

/** What may appear in `children` / slots: a node or a conditional wrapper. */
export type UiChildV1 = UiNodeV1 | UiConditionalV1

export type UiActionV1 = {
  id: string
  payload?: Record<string, unknown>
  disabled?: boolean
}

export type UiCapabilitySetV1 = {
  hover: boolean
  dialog: boolean
  tooltip: boolean
  externalLinks: boolean
  binaryTerminalSnapshots: boolean
}

// ---------- Strongly-typed Props per primitive ----------
//
// These are the browser-side mirrors of the Rust `*PropsV1` structs. Every
// field matches the cross-client spec exactly.

export type StackPropsV1 = {
  direction: UiValueV1<UiStackDirectionV1>
  gap?: UiValueV1<UiSpaceV1>
  align?: UiValueV1<UiAlignV1>
  justify?: UiValueV1<UiJustifyV1>
}

export type InlinePropsV1 = {
  gap?: UiValueV1<UiSpaceV1>
  align?: UiValueV1<UiAlignV1>
  justify?: UiValueV1<UiJustifyV1>
  wrap?: boolean
}

export type PanelPropsV1 = {
  title?: string
  tone?: UiPanelToneV1
  border?: boolean
  interactionDensity?: UiValueV1<UiInteractionDensityV1>
}

export type ScrollAreaPropsV1 = {
  axis?: UiScrollAxisV1
}

export type TextPropsV1 = {
  text: string
  tone?: UiToneV1
  size?: UiSizeV1
  weight?: UiTextWeightV1
  monospace?: boolean
  italic?: boolean
  truncate?: boolean
}

export type IconPropsV1 = {
  name: string
  size?: UiSizeV1
  tone?: UiToneV1
  label?: string
}

export type BadgePropsV1 = {
  text: string
  tone?: UiBadgeToneV1
  size?: UiBadgeSizeV1
}

export type StatusDotPropsV1 = {
  state: UiStatusDotStateV1
  label?: string
}

export type EmptyStatePropsV1 = {
  title: string
  description?: string
  icon?: string
  primaryAction?: UiActionV1
}

export type ButtonPropsV1 = {
  label: string
  action: UiActionV1
  variant?: UiButtonVariantV1
  tone?: UiButtonToneV1
  /** Cross-client canonical name; NOT `leadingIcon`. */
  icon?: string
}

export type IconButtonPropsV1 = {
  icon: string
  label: string
  action: UiActionV1
  tone?: UiButtonToneV1
}

/**
 * Tree has no shared props in v1 — the web-only `density` surface variant is
 * renderer-internal. Renderers read Tree nodes without a props struct.
 */
export type TreePropsV1 = Record<string, never>

export type TreeItemPropsV1 = {
  expanded?: boolean
  selected?: boolean
  notification?: boolean
  action?: UiActionV1
}

export type DialogPropsV1 = {
  open: boolean
  title: string
  /** Adaptive-spec extension; defaults to `"auto"` in the Lua constructor. */
  presentation?: UiPresentationV1
}

// ---------- Primitive type-name union ----------

export type UiPrimitiveTypeV1 =
  | 'stack'
  | 'inline'
  | 'panel'
  | 'scroll_area'
  | 'text'
  | 'icon'
  | 'badge'
  | 'status_dot'
  | 'empty_state'
  | 'button'
  | 'icon_button'
  | 'tree'
  | 'tree_item'
  | 'dialog'

// ---------- Narrow child-kind guards ----------

export function isConditional(child: UiChildV1): child is UiConditionalV1 {
  return (child as UiConditionalV1).$kind === 'when' ||
    (child as UiConditionalV1).$kind === 'hidden'
}

export function isResponsive<T>(
  value: UiValueV1<T> | undefined,
): value is UiResponsiveV1<T> {
  return (
    typeof value === 'object' &&
    value !== null &&
    (value as UiResponsiveV1<T>).$kind === 'responsive'
  )
}
