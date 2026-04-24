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

// ---------- Wire protocol v2 — surface tokens ----------

/**
 * Surface-density token for v2 composites. Distinct from
 * `UiInteractionDensityV1` (compact / comfortable hit targets) — this is the
 * public sidebar / panel variant from the Phase-1 web layout.
 */
export type UiSurfaceDensityV1 = 'sidebar' | 'panel'

/** Grouping mode for `session_list`. */
export type UiSessionListGroupingV1 = 'workspace' | 'flat'

// ---------- Wire protocol v2 — composite primitive Props ----------

export type SessionListPropsV1 = {
  density?: UiValueV1<UiSurfaceDensityV1>
  grouping?: UiSessionListGroupingV1
  showNavEntries?: boolean
}

export type WorkspaceListPropsV1 = {
  density?: UiValueV1<UiSurfaceDensityV1>
}

export type SpawnTargetListPropsV1 = {
  onSelect?: UiActionV1
  onRemove?: UiActionV1
}

export type WorktreeListPropsV1 = {
  targetId: string
}

export type SessionRowPropsV1 = {
  sessionUuid: string
  density?: UiValueV1<UiSurfaceDensityV1>
}

export type HubRecoveryStatePropsV1 = Record<string, never>
export type ConnectionCodePropsV1 = Record<string, never>

export type NewSessionButtonPropsV1 = {
  action: UiActionV1
}

// ---------- Wire protocol v2 — `$bind` sentinel ----------

/**
 * Wire shape of a `$bind` sentinel. May appear at any prop-value position.
 * Resolved client-side against the per-entity-type stores before primitive
 * dispatch. See `app/frontend/ui_contract/binding.tsx`.
 */
export type UiBindV1 = { $bind: string }

/** Wire shape of a `$kind = "bind_list"` envelope. */
export type UiBindListV1 = {
  $kind: 'bind_list'
  source: string
  item_template: UiNodeV1
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
  // Wire protocol v2 composites
  | 'session_list'
  | 'workspace_list'
  | 'spawn_target_list'
  | 'worktree_list'
  | 'session_row'
  | 'hub_recovery_state'
  | 'connection_code'
  | 'new_session_button'

// ---------- Narrow child-kind guards ----------

export function isConditional(child: UiChildV1): child is UiConditionalV1 {
  return (child as UiConditionalV1).$kind === 'when' ||
    (child as UiConditionalV1).$kind === 'hidden'
}

/** Returns true when `value` is a `$bind` sentinel object. */
export function isBindSentinel(value: unknown): value is UiBindV1 {
  if (value === null || typeof value !== 'object') return false
  const v = value as Record<string, unknown>
  return Object.keys(v).length === 1 && typeof v.$bind === 'string'
}

/** Returns true when `value` is a `$kind = "bind_list"` envelope. */
export function isBindList(value: unknown): value is UiBindListV1 {
  if (value === null || typeof value !== 'object') return false
  const v = value as Record<string, unknown>
  return v.$kind === 'bind_list' && typeof v.source === 'string'
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
