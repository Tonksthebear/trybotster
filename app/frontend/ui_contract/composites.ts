// Internal composite builders — Rails-owned, NOT Lua-public.
//
// These helpers build `UiNodeV1` trees for the phase-1 workspace/session
// surface per `docs/specs/web-ui-primitives-runtime.md`. They're the migration
// bridge the orchestrator asked for: composites consume the primitive registry
// rather than raw HTML + Catalyst imports, but remain internal to the Rails
// runtime (not exposed to Lua).
//
// Per `phase one web ui composites stay internal while Lua public contract
// stops at primitives.md`, these builders must not leak into the Lua-public
// registry.
//
// Densities (`sidebar` | `panel`) are a phase-1 surface variant, separate from
// the shared cross-client `UiInteractionDensityV1` token. They live entirely
// at the composite layer — primitives do not carry a density concept.

import type { UiActionV1, UiNodeV1 } from './types'

export type Density = 'sidebar' | 'panel'

// ---------- HostedPreviewIndicator ----------

export type HostedPreviewIndicatorInput = {
  sessionId: string
  sessionUuid: string
  hubId: string
  status: 'inactive' | 'starting' | 'running' | 'error' | 'unavailable'
  url?: string | null
  error?: string | null
  density: Density
}

export type HostedPreviewIndicatorResult = {
  /** The primitive tree to render, or null when nothing should render. */
  node: UiNodeV1 | null
  /** Native-tooltip title string; renderer wraps the node with `<span title>`. */
  tooltipTitle: string | null
}

const STATUS_LABEL: Record<HostedPreviewIndicatorInput['status'], string> = {
  inactive: 'Preview',
  starting: 'Starting\u2026',
  running: 'Running',
  error: 'Error',
  unavailable: 'Unavailable',
}

const STATUS_BADGE_TONE: Record<
  HostedPreviewIndicatorInput['status'],
  'default' | 'accent' | 'success' | 'warning' | 'danger'
> = {
  inactive: 'default',
  starting: 'warning',
  running: 'success',
  error: 'danger',
  unavailable: 'default',
}

/**
 * Build the hosted preview indicator.
 *
 * Running + url renders a visible `button` (variant=ghost, label="Running")
 * rather than `icon_button`, so the label text is visible to users regardless
 * of pointer device. Non-clickable states render as a `badge`.
 */
export function hostedPreviewIndicator(
  input: HostedPreviewIndicatorInput,
): HostedPreviewIndicatorResult {
  const { status, url } = input
  if (status === 'inactive' || status === 'unavailable') {
    return { node: null, tooltipTitle: null }
  }

  const label = STATUS_LABEL[status]
  const tone = STATUS_BADGE_TONE[status]
  const tooltipTitle =
    status === 'starting'
      ? 'Cloudflare preview is starting'
      : status === 'error'
        ? input.error || 'Cloudflare preview error'
        : status === 'running'
          ? 'Open Cloudflare preview'
          : 'Cloudflare preview status'

  if (status === 'running' && url) {
    const node: UiNodeV1 = {
      type: 'button',
      props: {
        label,
        variant: 'ghost',
        // Button tone can be default|accent|danger; no 'success' — we map
        // success intent to 'default' and let styling convey success.
        tone: 'default',
        icon: 'external-link',
        action: {
          id: 'botster.session.preview.open',
          payload: {
            sessionId: input.sessionId,
            sessionUuid: input.sessionUuid,
            url,
          },
        },
      },
    }
    return { node, tooltipTitle }
  }

  const node: UiNodeV1 = {
    type: 'badge',
    props: { text: label, tone, size: input.density === 'sidebar' ? 'sm' : 'md' },
  }
  return { node, tooltipTitle }
}

// ---------- HostedPreviewError ----------

export type HostedPreviewErrorInput = {
  error: string | null | undefined
  installUrl?: string | null
  sessionUuid: string
  density: Density
}

/**
 * Build the hosted preview error inner content.
 *
 * The caller (the React composite) owns the outer red-tinted Rails div —
 * `panel` in v1 has only `default | muted` tones, and we intentionally do not
 * extend the shared spec for renderer-specific styling.
 *
 * Returns `null` when there's nothing to render.
 */
export function hostedPreviewErrorInner(
  input: HostedPreviewErrorInput,
): UiNodeV1 | null {
  if (!input.error) return null

  const messageRow: UiNodeV1 = {
    type: 'inline',
    props: { gap: '2', align: 'start' },
    children: [
      {
        type: 'icon',
        props: {
          name: 'exclamation-triangle',
          size: input.density === 'sidebar' ? 'xs' : 'sm',
          tone: 'danger',
        },
      },
      {
        type: 'text',
        props: {
          text: input.error,
          tone: 'danger',
          size: input.density === 'sidebar' ? 'xs' : 'sm',
        },
      },
    ],
  }

  const rows: UiNodeV1[] = [messageRow]

  if (input.installUrl) {
    const action: UiActionV1 = {
      id: 'botster.session.preview.open',
      payload: { sessionUuid: input.sessionUuid, url: input.installUrl },
    }
    rows.push({
      type: 'button',
      props: {
        label: 'Install cloudflared',
        variant: 'ghost',
        tone: 'danger',
        action,
      },
    })
  }

  return {
    type: 'stack',
    props: {
      direction: 'vertical',
      gap: input.density === 'sidebar' ? '1' : '2',
      align: 'start',
    },
    children: rows,
  }
}

// ---------- SessionRow body ----------

export type SessionRowBodyInput = {
  sessionId: string
  sessionUuid: string
  primaryName: string
  titleLine: string | null
  subtext: string
  selected: boolean
  notification: boolean
  sessionType: 'agent' | 'accessory' | string
  activityState: 'hidden' | 'idle' | 'active' | 'accessory'
  action: UiActionV1
  hostedPreviewNode?: UiNodeV1 | null
  density: Density
}

/**
 * Build the session row body as a `tree_item`.
 *
 * The actions menu and the preview-error banner are rendered alongside by the
 * JSX wrapper (the row has parts that don't fit the tree_item slot model:
 * a dropdown menu, an error banner that sits below the row).
 */
export function sessionRowTreeItem(input: SessionRowBodyInput): UiNodeV1 {
  const isAccessory = input.sessionType === 'accessory'
  const titleSize = input.density === 'sidebar' ? 'xs' : 'sm'

  const titleChildren: UiNodeV1[] = [
    {
      type: 'text',
      props: {
        text: input.primaryName,
        size: titleSize,
        monospace: true,
        truncate: true,
        tone: isAccessory ? 'muted' : 'default',
        weight: input.selected ? 'medium' : 'regular',
      },
    },
  ]

  const subtitleNodes: UiNodeV1[] = []
  if (input.titleLine) {
    subtitleNodes.push({
      type: 'text',
      props: {
        text: input.titleLine,
        size: 'xs',
        tone: 'muted',
        italic: true,
        truncate: true,
      },
    })
  }
  if (input.subtext) {
    subtitleNodes.push({
      type: 'text',
      props: {
        text: input.subtext,
        size: 'xs',
        tone: 'muted',
        truncate: true,
      },
    })
  }

  const startSlot = activityDotForState(input.activityState)
  const endSlot: UiNodeV1[] = []
  if (input.hostedPreviewNode) endSlot.push(input.hostedPreviewNode)

  const slots: Record<string, UiNodeV1[]> = {
    title: titleChildren,
  }
  if (subtitleNodes.length > 0) slots['subtitle'] = subtitleNodes
  if (startSlot) slots['start'] = startSlot
  if (endSlot.length > 0) slots['end'] = endSlot

  return {
    type: 'tree_item',
    id: input.sessionId,
    props: {
      selected: input.selected,
      notification: input.notification,
      action: input.action,
    },
    slots,
  }
}

function activityDotForState(
  state: SessionRowBodyInput['activityState'],
): UiNodeV1[] | undefined {
  if (state === 'hidden' || state === 'accessory') return undefined
  const dotState = state === 'idle' ? 'idle' : 'active'
  const label = state === 'idle' ? 'Idle' : 'Active'
  return [
    {
      type: 'status_dot',
      props: { state: dotState, label },
    },
  ]
}

// ---------- SessionRow content (hybrid migration) ----------
//
// The SessionRow JSX wrapper owns two things the tree_item primitive does not
// (and should not) model: an `<a href>` for browser navigation (right-click /
// middle-click open in new tab) and the density-specific outer container
// styling for selected/hover/notification states. The content INSIDE that
// anchor is built via primitives below.

export type SessionRowContentInput = {
  primaryName: string
  titleLine: string | null
  subtext: string
  selected: boolean
  sessionType: 'agent' | 'accessory' | string
  activityState: 'hidden' | 'idle' | 'active' | 'accessory'
  density: Density
}

/**
 * Build the UiNodeV1 tree for a session row's INNER content (activity dot +
 * title + title line + subtext). The outer `<a href>` + density container
 * remain JSX in the composite for browser-navigation semantics.
 */
export function sessionRowContent(input: SessionRowContentInput): UiNodeV1 {
  const isAccessory = input.sessionType === 'accessory'
  const titleSize = input.density === 'sidebar' ? 'xs' : 'sm'

  const titleRowChildren: UiNodeV1[] = []
  const activityDot = activityDotForState(input.activityState)
  if (activityDot) titleRowChildren.push(activityDot[0]!)
  titleRowChildren.push({
    type: 'text',
    props: {
      text: input.primaryName,
      size: titleSize,
      monospace: true,
      truncate: true,
      tone: isAccessory ? 'muted' : 'default',
      weight: input.selected ? 'medium' : 'regular',
    },
  })

  const rows: UiNodeV1[] = [
    {
      type: 'inline',
      props: { gap: '2', align: 'center' },
      children: titleRowChildren,
    },
  ]

  if (input.titleLine) {
    rows.push({
      type: 'text',
      props: {
        text: input.titleLine,
        size: 'xs',
        tone: 'muted',
        italic: true,
        truncate: true,
      },
    })
  }
  if (input.subtext) {
    rows.push({
      type: 'text',
      props: {
        text: input.subtext,
        size: 'xs',
        tone: 'muted',
        truncate: true,
      },
    })
  }

  return {
    type: 'stack',
    props: {
      direction: 'vertical',
      gap: '0',
      align: 'stretch',
    },
    children: rows,
  }
}

// ---------- WorkspaceGroup header ----------

export type WorkspaceHeaderInput = {
  title: string
  count: number
  expanded: boolean
  density: Density
}

/**
 * Build the workspace group header content (chevron + title + count). The
 * click/toggle semantics and the rename button live on the outer JSX wrapper
 * because the rename button has hover-only affordance that v1 primitives
 * can't express cleanly.
 */
export function workspaceHeaderContent(
  input: WorkspaceHeaderInput,
): UiNodeV1 {
  const children: UiNodeV1[] = [
    {
      type: 'icon',
      props: {
        name: 'chevron-down',
        size: input.density === 'sidebar' ? 'xs' : 'sm',
        tone: 'muted',
      },
    },
    {
      type: 'text',
      props: {
        text: input.title,
        size: 'xs',
        tone: 'muted',
        weight: 'medium',
        truncate: true,
      },
    },
  ]

  if (input.density !== 'sidebar') {
    children.push({
      type: 'text',
      props: {
        text: String(input.count),
        size: 'xs',
        tone: 'muted',
      },
    })
  }

  return {
    type: 'inline',
    props: { gap: '2', align: 'center' },
    children,
  }
}
