import React, {
  type MouseEvent,
  type ReactElement,
  type ReactNode,
} from 'react'
import clsx from 'clsx'
import type { RenderContext } from './context'
import { IconGlyph } from './icons'
import type {
  BadgePropsV1,
  ButtonPropsV1,
  DialogPropsV1,
  EmptyStatePropsV1,
  IconButtonPropsV1,
  IconPropsV1,
  InlinePropsV1,
  PanelPropsV1,
  ScrollAreaPropsV1,
  StackPropsV1,
  StatusDotPropsV1,
  TextPropsV1,
  TreeItemPropsV1,
  UiActionV1,
  UiAlignV1,
  UiBadgeSizeV1,
  UiBadgeToneV1,
  UiButtonToneV1,
  UiButtonVariantV1,
  UiInteractionDensityV1,
  UiJustifyV1,
  UiNodeV1,
  UiPanelToneV1,
  UiPropsV1,
  UiScrollAxisV1,
  UiSizeV1,
  UiSpaceV1,
  UiStackDirectionV1,
  UiStatusDotStateV1,
  UiTextWeightV1,
  UiToneV1,
  UiValueV1,
} from './types'
import { resolveValue } from './viewport'

// ---------- Renderer signature ----------

/**
 * Args handed to every primitive renderer.
 *
 * - `props`: raw unresolved prop bag from the wire. Individual renderers cast
 *   it to their typed Props and call `resolveValue` for responsive fields.
 * - `children`: already-rendered positional children (conditionals resolved).
 * - `slots`: already-rendered slot children, keyed by slot name.
 * - `ctx`: viewport, capability set, and action dispatch.
 */
export type PrimitiveRendererArgs = {
  node: UiNodeV1
  props: UiPropsV1
  children: ReactNode[]
  slots: Record<string, ReactNode[]>
  ctx: RenderContext
}

export type PrimitiveRenderer = (args: PrimitiveRendererArgs) => ReactElement

// ---------- Tailwind token maps ----------

const SPACE_GAP: Record<UiSpaceV1, string> = {
  '0': 'gap-0',
  '1': 'gap-1',
  '2': 'gap-2',
  '3': 'gap-3',
  '4': 'gap-4',
  '6': 'gap-6',
}

const ALIGN_ITEMS: Record<UiAlignV1, string> = {
  start: 'items-start',
  center: 'items-center',
  end: 'items-end',
  stretch: 'items-stretch',
}

const JUSTIFY_CONTENT: Record<UiJustifyV1, string> = {
  start: 'justify-start',
  center: 'justify-center',
  end: 'justify-end',
  between: 'justify-between',
}

const TEXT_TONE: Record<UiToneV1, string> = {
  default: 'text-zinc-100',
  muted: 'text-zinc-400',
  accent: 'text-sky-400',
  success: 'text-emerald-400',
  warning: 'text-amber-400',
  danger: 'text-red-400',
}

const TEXT_SIZE: Record<UiSizeV1, string> = {
  xs: 'text-xs',
  sm: 'text-sm',
  md: 'text-base',
}

const TEXT_WEIGHT: Record<UiTextWeightV1, string> = {
  regular: 'font-normal',
  medium: 'font-medium',
  semibold: 'font-semibold',
}

const PANEL_TONE: Record<UiPanelToneV1, string> = {
  default: 'bg-zinc-900/60',
  muted: 'bg-zinc-800/40',
}

const BADGE_TONE: Record<UiBadgeToneV1, string> = {
  default:
    'bg-zinc-600/20 text-zinc-300 dark:bg-white/10 dark:text-zinc-300',
  accent: 'bg-sky-500/15 text-sky-600 dark:bg-sky-500/10 dark:text-sky-400',
  success:
    'bg-emerald-500/15 text-emerald-700 dark:bg-emerald-500/10 dark:text-emerald-400',
  warning:
    'bg-amber-400/20 text-amber-700 dark:bg-amber-400/10 dark:text-amber-400',
  danger: 'bg-red-500/15 text-red-700 dark:bg-red-500/10 dark:text-red-400',
}

const BADGE_SIZE: Record<UiBadgeSizeV1, string> = {
  sm: 'px-1.5 py-0.5 text-[10px]',
  md: 'px-2 py-0.5 text-xs',
}

const STATUS_DOT_STATE: Record<UiStatusDotStateV1, string> = {
  neutral: 'bg-zinc-500',
  idle: 'bg-sky-500',
  active: 'bg-emerald-400',
  success: 'bg-emerald-500',
  warning: 'bg-amber-400',
  danger: 'bg-red-500',
}

const BUTTON_TONE_SOLID: Record<UiButtonToneV1, string> = {
  default: 'bg-zinc-800 text-zinc-100 hover:bg-zinc-700',
  accent: 'bg-sky-600 text-white hover:bg-sky-500',
  danger: 'bg-red-600 text-white hover:bg-red-500',
}

const BUTTON_TONE_GHOST: Record<UiButtonToneV1, string> = {
  default: 'text-zinc-200 hover:bg-zinc-800/60',
  accent: 'text-sky-400 hover:bg-sky-500/10',
  danger: 'text-red-400 hover:bg-red-500/10',
}

const ICON_SIZE: Record<UiSizeV1, string> = {
  xs: 'size-3',
  sm: 'size-4',
  md: 'size-5',
}

// ---------- Action helpers ----------

/**
 * Mapping from well-known action ids to `data-testid` values that Rails
 * system tests and other DOM-level consumers can target. Kept in the web
 * renderer (not in the shared contract) because `data-testid` is a
 * renderer-specific affordance per the spec's "renderer hints don't belong
 * in the shared contract" rule.
 *
 * Add entries here when a new Lua-authored action needs a stable DOM anchor;
 * the Lua contract stays unchanged.
 */
const ACTION_TEST_IDS: Record<string, string> = {
  'botster.session.create.request': 'new-session-button',
}

function wrapActionClick(
  action: UiActionV1,
  ctx: RenderContext,
): (event: MouseEvent) => void {
  return (event: MouseEvent) => {
    if (action.disabled) {
      event.preventDefault()
      return
    }
    event.preventDefault()
    event.stopPropagation()
    ctx.dispatch(action, { element: event.currentTarget as Element })
  }
}

// ---------- Primitive renderers ----------

const renderStack: PrimitiveRenderer = ({ props, children, ctx }) => {
  const p = props as Partial<StackPropsV1>
  const direction =
    resolveValue<UiStackDirectionV1>(
      p.direction as UiValueV1<UiStackDirectionV1> | undefined,
      ctx.viewport,
    ) ?? 'vertical'
  const gap = resolveValue<UiSpaceV1>(p.gap, ctx.viewport)
  const align = resolveValue<UiAlignV1>(p.align, ctx.viewport)
  const justify = resolveValue<UiJustifyV1>(p.justify, ctx.viewport)
  return (
    <div
      className={clsx(
        'flex',
        direction === 'vertical' ? 'flex-col' : 'flex-row',
        gap && SPACE_GAP[gap],
        align && ALIGN_ITEMS[align],
        justify && JUSTIFY_CONTENT[justify],
      )}
    >
      {children}
    </div>
  )
}

const renderInline: PrimitiveRenderer = ({ props, children, ctx }) => {
  const p = props as Partial<InlinePropsV1>
  const gap = resolveValue<UiSpaceV1>(p.gap, ctx.viewport)
  const align = resolveValue<UiAlignV1>(p.align, ctx.viewport)
  const justify = resolveValue<UiJustifyV1>(p.justify, ctx.viewport)
  return (
    <div
      className={clsx(
        'flex flex-row',
        gap && SPACE_GAP[gap],
        align && ALIGN_ITEMS[align],
        justify && JUSTIFY_CONTENT[justify],
        p.wrap && 'flex-wrap',
      )}
    >
      {children}
    </div>
  )
}

const renderPanel: PrimitiveRenderer = ({ props, children, ctx }) => {
  const p = props as Partial<PanelPropsV1>
  const tone: UiPanelToneV1 = p.tone ?? 'default'
  const border = p.border ?? false
  const density =
    resolveValue<UiInteractionDensityV1>(p.interactionDensity, ctx.viewport) ??
    'comfortable'
  const paddingClass =
    density === 'compact' ? 'p-2' : 'p-4'
  return (
    <section
      className={clsx(
        'rounded-lg',
        PANEL_TONE[tone],
        border && 'border border-zinc-700/50',
        paddingClass,
      )}
    >
      {p.title !== undefined && (
        <header
          className={clsx(
            'mb-2 text-xs font-semibold uppercase tracking-wider text-zinc-400',
          )}
        >
          {p.title}
        </header>
      )}
      {children}
    </section>
  )
}

const SCROLL_AXIS: Record<UiScrollAxisV1, string> = {
  y: 'overflow-y-auto',
  x: 'overflow-x-auto',
  both: 'overflow-auto',
}

const renderScrollArea: PrimitiveRenderer = ({ props, children }) => {
  const p = props as Partial<ScrollAreaPropsV1>
  const axis: UiScrollAxisV1 = p.axis ?? 'y'
  return (
    <div className={clsx('min-h-0 min-w-0', SCROLL_AXIS[axis])}>{children}</div>
  )
}

const renderText: PrimitiveRenderer = ({ props }) => {
  const p = props as Partial<TextPropsV1>
  const text = p.text ?? ''
  const tone = p.tone ?? 'default'
  const size = p.size ?? 'sm'
  return (
    <span
      className={clsx(
        TEXT_TONE[tone],
        TEXT_SIZE[size],
        p.weight && TEXT_WEIGHT[p.weight],
        p.italic && 'italic',
        p.monospace && 'font-mono',
        p.truncate && 'truncate block min-w-0',
      )}
    >
      {text}
    </span>
  )
}

const renderIcon: PrimitiveRenderer = ({ props }) => {
  const p = props as Partial<IconPropsV1>
  const name = p.name ?? ''
  const size = p.size ?? 'sm'
  const tone = p.tone ?? 'default'
  const glyph = (
    <IconGlyph name={name} className={clsx('h-full w-full')} />
  )
  return (
    <span
      role="img"
      aria-label={p.label ?? name}
      data-icon={name}
      className={clsx(
        'inline-flex shrink-0 items-center justify-center',
        ICON_SIZE[size],
        TEXT_TONE[tone],
      )}
    >
      {glyph}
    </span>
  )
}

const renderBadge: PrimitiveRenderer = ({ props }) => {
  const p = props as Partial<BadgePropsV1>
  const tone = p.tone ?? 'default'
  const size = p.size ?? 'md'
  return (
    <span
      className={clsx(
        'inline-flex items-center rounded-md font-medium',
        BADGE_TONE[tone],
        BADGE_SIZE[size],
      )}
    >
      {p.text ?? ''}
    </span>
  )
}

const renderStatusDot: PrimitiveRenderer = ({ props }) => {
  const p = props as Partial<StatusDotPropsV1>
  const state = p.state ?? 'neutral'
  return (
    <span
      role="status"
      aria-label={p.label ?? state}
      className={clsx('inline-block size-2 rounded-full', STATUS_DOT_STATE[state])}
    />
  )
}

const renderEmptyState: PrimitiveRenderer = ({ props, ctx }) => {
  const p = props as Partial<EmptyStatePropsV1>
  const title = p.title ?? ''
  const handlePrimary = p.primaryAction
    ? wrapActionClick(p.primaryAction, ctx)
    : undefined
  return (
    <div className="flex flex-col items-center justify-center gap-2 py-8 text-center">
      {p.icon !== undefined && (
        <span
          data-icon={p.icon}
          aria-hidden="true"
          className="mx-auto inline-flex size-12 items-center justify-center text-zinc-600"
        >
          <IconGlyph name={p.icon} className="h-full w-full" />
        </span>
      )}
      <h3 className="text-lg font-medium text-zinc-300">{title}</h3>
      {p.description !== undefined && (
        <p className="text-sm text-zinc-500">{p.description}</p>
      )}
      {p.primaryAction && handlePrimary && (
        // Phase A spec gap: EmptyStatePropsV1.primaryAction has no label
        // field, so we fall back to a generic "Get started" string. Surfaces
        // that need a specific action label should compose Stack+Button
        // directly instead of using the empty_state primitive. v2 spec
        // candidate: EmptyStatePropsV1.primaryActionLabel.
        <button
          type="button"
          onClick={handlePrimary}
          disabled={p.primaryAction.disabled === true}
          className="mt-2 inline-flex items-center gap-2 rounded-md bg-zinc-800 px-3 py-1.5 text-sm text-zinc-100 hover:bg-zinc-700 disabled:opacity-50"
        >
          Get started
        </button>
      )}
    </div>
  )
}

const renderButton: PrimitiveRenderer = ({ props, ctx }) => {
  const p = props as Partial<ButtonPropsV1>
  const label = p.label ?? ''
  const action = p.action
  const variant: UiButtonVariantV1 = p.variant ?? 'solid'
  const tone: UiButtonToneV1 = p.tone ?? 'default'
  if (!action) {
    return <button type="button" disabled>{label}</button>
  }
  const onClick = wrapActionClick(action, ctx)
  const toneClass =
    variant === 'solid' ? BUTTON_TONE_SOLID[tone] : BUTTON_TONE_GHOST[tone]
  const testId = ACTION_TEST_IDS[action.id]
  return (
    <button
      type="button"
      data-action-id={action.id}
      data-testid={testId}
      onClick={onClick}
      disabled={action.disabled === true}
      className={clsx(
        'inline-flex items-center gap-2 rounded-md px-3 py-1.5 text-sm font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-50',
        toneClass,
      )}
    >
      {p.icon !== undefined && (
        <span data-icon={p.icon} aria-hidden="true" className="inline-flex size-4 items-center justify-center">
          <IconGlyph name={p.icon} className="h-full w-full" />
        </span>
      )}
      {label}
    </button>
  )
}

const renderIconButton: PrimitiveRenderer = ({ props, ctx }) => {
  const p = props as Partial<IconButtonPropsV1>
  const icon = p.icon ?? ''
  const label = p.label ?? ''
  const action = p.action
  const tone: UiButtonToneV1 = p.tone ?? 'default'
  if (!action) {
    return <button type="button" aria-label={label} disabled />
  }
  const onClick = wrapActionClick(action, ctx)
  const testId = ACTION_TEST_IDS[action.id]
  return (
    <button
      type="button"
      aria-label={label}
      data-action-id={action.id}
      data-testid={testId}
      onClick={onClick}
      disabled={action.disabled === true}
      className={clsx(
        'inline-flex size-7 items-center justify-center rounded-md transition-colors disabled:cursor-not-allowed disabled:opacity-50',
        BUTTON_TONE_GHOST[tone],
      )}
    >
      <span data-icon={icon} aria-hidden="true" className="inline-flex size-4 items-center justify-center">
        <IconGlyph name={icon} className="h-full w-full" />
      </span>
    </button>
  )
}

const renderTree: PrimitiveRenderer = ({ children }) => {
  return (
    <ul role="tree" className="flex flex-col">
      {children}
    </ul>
  )
}

const renderTreeItem: PrimitiveRenderer = ({ props, slots, ctx, node }) => {
  const p = props as Partial<TreeItemPropsV1>
  const selected = p.selected ?? false
  const expanded = p.expanded
  const notification = p.notification ?? false
  const action = p.action
  const onClick = action ? wrapActionClick(action, ctx) : undefined

  const titleSlot = slots['title'] ?? []
  const subtitleSlot = slots['subtitle']
  const startSlot = slots['start']
  const endSlot = slots['end']
  const childrenSlot = slots['children']

  // When a tree_item carries a session-select action, render the primary
  // click target as an <a href> so right-click / middle-click open the
  // session in a new tab (browser navigation semantics). The end slot stays
  // OUTSIDE the anchor to avoid nested-interactive HTML (end slot often
  // contains a menu icon_button).
  const sessionUuid =
    (action?.payload as { sessionUuid?: string } | undefined)?.sessionUuid
  const sessionHref =
    action?.id === 'botster.session.select' && ctx.hubId && sessionUuid
      ? `/hubs/${ctx.hubId}/sessions/${sessionUuid}`
      : null

  const primaryClass = clsx(
    'flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1.5',
    selected
      ? 'bg-sky-500/20 text-sky-300'
      : action
        ? 'cursor-pointer text-zinc-200 hover:bg-zinc-800/50'
        : 'text-zinc-200',
  )

  const primaryContent = (
    <>
      {startSlot && <div className="shrink-0">{startSlot}</div>}
      <div className="min-w-0 flex-1">
        <div className="min-w-0 truncate">{titleSlot}</div>
        {subtitleSlot && (
          <div className="min-w-0 truncate text-xs text-zinc-500">
            {subtitleSlot}
          </div>
        )}
      </div>
    </>
  )

  return (
    <li
      role="treeitem"
      aria-selected={selected}
      aria-expanded={expanded}
      data-notification={notification || undefined}
      data-session-id={node.id}
      className={clsx(
        'flex flex-col',
        notification && 'border-l-2 border-yellow-400',
      )}
    >
      <div className="flex items-center gap-1">
        {sessionHref ? (
          <a href={sessionHref} onClick={onClick} className={primaryClass}>
            {primaryContent}
          </a>
        ) : (
          <div onClick={onClick} className={primaryClass}>
            {primaryContent}
          </div>
        )}
        {endSlot && <div className="shrink-0">{endSlot}</div>}
      </div>
      {childrenSlot && expanded !== false && (
        <ul role="group" className="ml-4 flex flex-col">
          {childrenSlot}
        </ul>
      )}
    </li>
  )
}

const renderDialog: PrimitiveRenderer = ({ props, slots, ctx }) => {
  const p = props as Partial<DialogPropsV1>
  const open = p.open ?? false
  const title = p.title ?? ''
  const presentation = p.presentation ?? 'auto'
  if (!open) return <></>

  const body = slots['body']
  const footer = slots['footer']

  // Resolve `auto` presentation against the viewport.
  const resolved =
    presentation === 'auto'
      ? ctx.viewport.widthClass === 'compact'
        ? ctx.viewport.heightClass === 'short'
          ? 'fullscreen'
          : 'sheet'
        : 'overlay'
      : presentation

  if (resolved === 'inline') {
    return (
      <section className="rounded-lg border border-zinc-700 bg-zinc-900 p-4">
        <h2 className="text-base font-semibold text-zinc-100">{title}</h2>
        {body && <div className="mt-3">{body}</div>}
        {footer && (
          <div className="mt-4 flex justify-end gap-2">{footer}</div>
        )}
      </section>
    )
  }

  const sizeClass =
    resolved === 'fullscreen'
      ? 'w-full h-full'
      : resolved === 'sheet'
        ? 'w-full max-w-lg rounded-t-2xl'
        : 'max-w-lg rounded-2xl'

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={title}
      data-presentation={resolved}
      className="fixed inset-0 z-50 flex items-center justify-center bg-zinc-950/50 p-4"
    >
      <div
        className={clsx(
          'flex flex-col bg-zinc-900 text-zinc-100 shadow-lg ring-1 ring-white/10',
          sizeClass,
        )}
      >
        <header className="border-b border-zinc-800 px-4 py-3">
          <h2 className="text-base font-semibold">{title}</h2>
        </header>
        {body && <div className="flex-1 overflow-y-auto p-4">{body}</div>}
        {footer && (
          <footer className="flex justify-end gap-2 border-t border-zinc-800 px-4 py-3">
            {footer}
          </footer>
        )}
      </div>
    </div>
  )
}

// ---------- Registry ----------

export const PRIMITIVE_REGISTRY: Record<string, PrimitiveRenderer> = {
  stack: renderStack,
  inline: renderInline,
  panel: renderPanel,
  scroll_area: renderScrollArea,
  text: renderText,
  icon: renderIcon,
  badge: renderBadge,
  status_dot: renderStatusDot,
  empty_state: renderEmptyState,
  button: renderButton,
  icon_button: renderIconButton,
  tree: renderTree,
  tree_item: renderTreeItem,
  dialog: renderDialog,
}
