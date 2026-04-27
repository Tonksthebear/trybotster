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

export type UiTone =
  | 'default'
  | 'muted'
  | 'accent'
  | 'success'
  | 'warning'
  | 'danger'

export type UiAlign = 'start' | 'center' | 'end' | 'stretch'
export type UiJustify = 'start' | 'center' | 'end' | 'between'
export type UiInteractionDensity = 'compact' | 'comfortable'
export type UiSize = 'xs' | 'sm' | 'md'
export type UiSpace = '0' | '1' | '2' | '3' | '4' | '6'
export type UiTextWeight = 'regular' | 'medium' | 'semibold'
export type UiStackDirection = 'vertical' | 'horizontal'
export type UiScrollAxis = 'y' | 'x' | 'both'
export type UiPanelTone = 'default' | 'muted'
export type UiBadgeTone =
  | 'default'
  | 'accent'
  | 'success'
  | 'warning'
  | 'danger'
export type UiBadgeSize = 'sm' | 'md'
export type UiStatusDotState =
  | 'neutral'
  | 'idle'
  | 'active'
  | 'success'
  | 'warning'
  | 'danger'
export type UiButtonVariant = 'solid' | 'ghost'
export type UiButtonTone = 'default' | 'accent' | 'danger'
export type UiPresentation =
  | 'auto'
  | 'inline'
  | 'overlay'
  | 'sheet'
  | 'fullscreen'

// ---------- Viewport classes ----------

export type UiWidthClass = 'compact' | 'regular' | 'expanded'
export type UiHeightClass = 'short' | 'regular' | 'tall'
export type UiPointer = 'none' | 'coarse' | 'fine'
export type UiOrientation = 'portrait' | 'landscape'

export type UiViewport = {
  widthClass: UiWidthClass
  heightClass: UiHeightClass
  pointer: UiPointer
  orientation?: UiOrientation
  keyboardOccluded?: boolean
}

// ---------- Responsive + conditional wire sentinels ----------

export type UiResponsiveWidth<T> = {
  compact?: T
  regular?: T
  expanded?: T
}

export type UiResponsiveHeight<T> = {
  short?: T
  regular?: T
  tall?: T
}

export type UiResponsive<T> = {
  $kind: 'responsive'
  width?: UiResponsiveWidth<T>
  height?: UiResponsiveHeight<T>
}

/** Either a concrete `T` or a `$kind: "responsive"` sentinel. */
export type UiValue<T> = T | UiResponsive<T>

export type UiCondition = {
  width?: UiWidthClass
  height?: UiHeightClass
  pointer?: UiPointer
  orientation?: UiOrientation
  keyboardOccluded?: boolean
}

export type UiConditional =
  | { $kind: 'when'; condition: UiCondition; node: UiNode }
  | { $kind: 'hidden'; condition: UiCondition; node: UiNode }

// ---------- Core node + action + capabilities ----------

/** Arbitrary prop bag — primitive-specific Props types narrow this further. */
export type UiProps = Record<string, unknown>

export type UiNode = {
  type: string
  id?: string
  props?: UiProps
  children?: UiChild[]
  slots?: Record<string, UiChild[]>
}

/** What may appear in `children` / slots: a node or a conditional wrapper. */
export type UiChild = UiNode | UiConditional

export type UiAction = {
  id: string
  payload?: Record<string, unknown>
  disabled?: boolean
}

export type UiCapabilitySet = {
  hover: boolean
  dialog: boolean
  tooltip: boolean
  externalLinks: boolean
  binaryTerminalSnapshots: boolean
}

// ---------- Strongly-typed Props per primitive ----------
//
// These are the browser-side mirrors of the Rust `*Props` structs. Every
// field matches the cross-client spec exactly.

export type StackProps = {
  direction: UiValue<UiStackDirection>
  gap?: UiValue<UiSpace>
  align?: UiValue<UiAlign>
  justify?: UiValue<UiJustify>
}

export type InlineProps = {
  gap?: UiValue<UiSpace>
  align?: UiValue<UiAlign>
  justify?: UiValue<UiJustify>
  wrap?: boolean
}

export type PanelProps = {
  title?: string
  tone?: UiPanelTone
  border?: boolean
  interactionDensity?: UiValue<UiInteractionDensity>
}

export type ScrollAreaProps = {
  axis?: UiScrollAxis
}

export type TextProps = {
  text: string
  tone?: UiTone
  size?: UiSize
  weight?: UiTextWeight
  monospace?: boolean
  italic?: boolean
  truncate?: boolean
}

export type IconProps = {
  name: string
  size?: UiSize
  tone?: UiTone
  label?: string
}

export type BadgeProps = {
  text: string
  tone?: UiBadgeTone
  size?: UiBadgeSize
}

export type StatusDotProps = {
  state: UiStatusDotState
  label?: string
}

export type EmptyStateProps = {
  title: string
  description?: string
  icon?: string
  primaryAction?: UiAction
}

export type ButtonProps = {
  label: string
  action: UiAction
  variant?: UiButtonVariant
  tone?: UiButtonTone
  /** Cross-client canonical name; NOT `leadingIcon`. */
  icon?: string
}

export type IconButtonProps = {
  icon: string
  label: string
  action: UiAction
  tone?: UiButtonTone
}

/**
 * Tree has no shared props in current — the web-only `density` surface variant is
 * renderer-internal. Renderers read Tree nodes without a props struct.
 */
export type TreeProps = Record<string, never>

export type TreeItemProps = {
  expanded?: boolean
  selected?: boolean
  notification?: boolean
  action?: UiAction
}

export type DialogProps = {
  open: boolean
  title: string
  /** Adaptive-spec extension; defaults to `"auto"` in the Lua constructor. */
  presentation?: UiPresentation
}

// ---------- Wire protocol — surface tokens ----------

/**
 * Surface-density token for composites. Distinct from
 * `UiInteractionDensity` (compact / comfortable hit targets) — this is the
 * public sidebar / panel variant from the Phase-1 web layout.
 */
export type UiSurfaceDensity = 'sidebar' | 'panel'

/** Grouping mode for `session_list`. */
export type UiSessionListGrouping = 'workspace' | 'flat'

// ---------- Wire protocol — composite primitive Props ----------

export type SessionListProps = {
  density?: UiValue<UiSurfaceDensity>
  grouping?: UiSessionListGrouping
  showNavEntries?: boolean
}

export type WorkspaceListProps = {
  density?: UiValue<UiSurfaceDensity>
}

export type SpawnTargetListProps = {
  onSelect?: UiAction
  onRemove?: UiAction
}

export type WorktreeListProps = {
  targetId: string
}

export type SessionRowProps = {
  sessionUuid: string
  density?: UiValue<UiSurfaceDensity>
}

export type HubRecoveryStateProps = Record<string, never>

export type NewSessionButtonProps = {
  action: UiAction
}

// ---------- Wire protocol — `$bind` sentinel ----------

/**
 * Wire shape of a `$bind` sentinel. May appear at any prop-value position.
 * Resolved client-side against the per-entity-type stores before primitive
 * dispatch. See `app/frontend/ui_contract/binding.tsx`.
 */
export type UiBind = { $bind: string }

/** Wire shape of a `$kind = "bind_list"` envelope. */
export type UiBindList = {
  $kind: 'bind_list'
  source: string
  item_template: UiNode
}

// ---------- Primitive type-name union ----------

export type UiPrimitiveType =
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
  // Wire protocol composites
  | 'session_list'
  | 'workspace_list'
  | 'spawn_target_list'
  | 'worktree_list'
  | 'session_row'
  | 'hub_recovery_state'
  | 'new_session_button'

// ---------- Narrow child-kind guards ----------

export function isConditional(child: UiChild): child is UiConditional {
  return (child as UiConditional).$kind === 'when' ||
    (child as UiConditional).$kind === 'hidden'
}

/** Returns true when `value` is a `$bind` sentinel object. */
export function isBindSentinel(value: unknown): value is UiBind {
  if (value === null || typeof value !== 'object') return false
  const v = value as Record<string, unknown>
  return Object.keys(v).length === 1 && typeof v.$bind === 'string'
}

/** Returns true when `value` is a `$kind = "bind_list"` envelope. */
export function isBindList(value: unknown): value is UiBindList {
  if (value === null || typeof value !== 'object') return false
  const v = value as Record<string, unknown>
  return v.$kind === 'bind_list' && typeof v.source === 'string'
}

export function isResponsive<T>(
  value: UiValue<T> | undefined,
): value is UiResponsive<T> {
  return (
    typeof value === 'object' &&
    value !== null &&
    (value as UiResponsive<T>).$kind === 'responsive'
  )
}
