import { useEffect, useState } from 'react'
import type {
  UiConditionV1,
  UiHeightClassV1,
  UiOrientationV1,
  UiPointerV1,
  UiResponsiveV1,
  UiValueV1,
  UiViewportV1,
  UiWidthClassV1,
} from './types'
import { isResponsive } from './types'

// Default web thresholds from `docs/specs/adaptive-ui-viewport-and-presentation.md`.
// Renderers own exact thresholds; these are the defaults for the React island.
export const WIDTH_COMPACT_MAX = 640
export const WIDTH_REGULAR_MAX = 1024
export const HEIGHT_SHORT_MAX = 700
export const HEIGHT_REGULAR_MAX = 1000

export function widthClassForPx(width: number): UiWidthClassV1 {
  if (width < WIDTH_COMPACT_MAX) return 'compact'
  if (width < WIDTH_REGULAR_MAX) return 'regular'
  return 'expanded'
}

export function heightClassForPx(height: number): UiHeightClassV1 {
  if (height < HEIGHT_SHORT_MAX) return 'short'
  if (height < HEIGHT_REGULAR_MAX) return 'regular'
  return 'tall'
}

const SSR_VIEWPORT: UiViewportV1 = {
  widthClass: 'expanded',
  heightClass: 'regular',
  pointer: 'fine',
}

function readViewport(): UiViewportV1 {
  if (typeof window === 'undefined') return SSR_VIEWPORT

  const visualViewport = window.visualViewport
  const layoutWidth = window.innerWidth
  const layoutHeight = window.innerHeight
  const width = visualViewport?.width ?? layoutWidth
  const height = visualViewport?.height ?? layoutHeight

  const pointer: UiPointerV1 = window.matchMedia('(pointer: fine)').matches
    ? 'fine'
    : window.matchMedia('(pointer: coarse)').matches
      ? 'coarse'
      : 'none'

  let orientation: UiOrientationV1 | undefined
  if (window.matchMedia('(orientation: portrait)').matches) {
    orientation = 'portrait'
  } else if (window.matchMedia('(orientation: landscape)').matches) {
    orientation = 'landscape'
  }

  // Heuristic: if the visual viewport is meaningfully smaller than the layout
  // viewport (e.g. keyboard open on mobile), mark keyboardOccluded=true.
  const keyboardOccluded =
    visualViewport !== undefined && visualViewport !== null
      ? layoutHeight - visualViewport.height > 150
      : undefined

  const viewport: UiViewportV1 = {
    widthClass: widthClassForPx(width),
    heightClass: heightClassForPx(height),
    pointer,
  }
  if (orientation !== undefined) viewport.orientation = orientation
  if (keyboardOccluded !== undefined) viewport.keyboardOccluded = keyboardOccluded

  return viewport
}

/** Reactive viewport hook. Updates on resize / orientation / visualViewport changes. */
export function useViewport(): UiViewportV1 {
  const [viewport, setViewport] = useState<UiViewportV1>(readViewport)

  useEffect(() => {
    if (typeof window === 'undefined') return

    let raf = 0
    function schedule() {
      if (raf) return
      raf = window.requestAnimationFrame(() => {
        raf = 0
        setViewport(readViewport())
      })
    }

    window.addEventListener('resize', schedule)
    window.addEventListener('orientationchange', schedule)
    window.visualViewport?.addEventListener('resize', schedule)
    window.visualViewport?.addEventListener('scroll', schedule)

    const pointerQueries = [
      window.matchMedia('(pointer: fine)'),
      window.matchMedia('(pointer: coarse)'),
    ]
    pointerQueries.forEach((mq) => mq.addEventListener('change', schedule))

    return () => {
      if (raf) window.cancelAnimationFrame(raf)
      window.removeEventListener('resize', schedule)
      window.removeEventListener('orientationchange', schedule)
      window.visualViewport?.removeEventListener('resize', schedule)
      window.visualViewport?.removeEventListener('scroll', schedule)
      pointerQueries.forEach((mq) => mq.removeEventListener('change', schedule))
    }
  }, [])

  return viewport
}

// ---------- Responsive resolution ----------

const WIDTH_FALLBACK_ORDER: Record<UiWidthClassV1, UiWidthClassV1[]> = {
  compact: ['compact', 'regular', 'expanded'],
  regular: ['regular', 'compact', 'expanded'],
  expanded: ['expanded', 'regular', 'compact'],
}

const HEIGHT_FALLBACK_ORDER: Record<UiHeightClassV1, UiHeightClassV1[]> = {
  short: ['short', 'regular', 'tall'],
  regular: ['regular', 'short', 'tall'],
  tall: ['tall', 'regular', 'short'],
}

/**
 * Resolve a `UiValueV1<T>` against the viewport.
 *
 * Fallback order per the adaptive spec: exact match, then next smaller class,
 * then next larger class. When both `width` and `height` are defined on a
 * responsive value, `width` wins (width is primary in the spec's examples).
 */
export function resolveValue<T>(
  value: UiValueV1<T> | undefined,
  viewport: UiViewportV1,
): T | undefined {
  if (value === undefined) return undefined
  if (!isResponsive(value)) return value

  const resolved = resolveResponsive(value, viewport)
  return resolved
}

function resolveResponsive<T>(
  value: UiResponsiveV1<T>,
  viewport: UiViewportV1,
): T | undefined {
  if (value.width !== undefined) {
    const order = WIDTH_FALLBACK_ORDER[viewport.widthClass]
    for (const cls of order) {
      const v = value.width[cls]
      if (v !== undefined) return v
    }
  }
  if (value.height !== undefined) {
    const order = HEIGHT_FALLBACK_ORDER[viewport.heightClass]
    for (const cls of order) {
      const v = value.height[cls]
      if (v !== undefined) return v
    }
  }
  return undefined
}

/** Evaluate a viewport predicate. Returns true iff every populated field matches. */
export function matchesCondition(
  cond: UiConditionV1,
  viewport: UiViewportV1,
): boolean {
  if (cond.width !== undefined && cond.width !== viewport.widthClass) return false
  if (cond.height !== undefined && cond.height !== viewport.heightClass) return false
  if (cond.pointer !== undefined && cond.pointer !== viewport.pointer) return false
  if (cond.orientation !== undefined && cond.orientation !== viewport.orientation)
    return false
  if (
    cond.keyboardOccluded !== undefined &&
    cond.keyboardOccluded !== viewport.keyboardOccluded
  )
    return false
  return true
}
