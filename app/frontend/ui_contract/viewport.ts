import { useEffect, useState } from 'react'
import type {
  UiCondition,
  UiHeightClass,
  UiOrientation,
  UiPointer,
  UiResponsive,
  UiValue,
  UiViewport,
  UiWidthClass,
} from './types'
import { isResponsive } from './types'

// Default web thresholds from `docs/specs/adaptive-ui-viewport-and-presentation.md`.
// Renderers own exact thresholds; these are the defaults for the React island.
export const WIDTH_COMPACT_MAX = 640
export const WIDTH_REGULAR_MAX = 1024
export const HEIGHT_SHORT_MAX = 700
export const HEIGHT_REGULAR_MAX = 1000

export function widthClassForPx(width: number): UiWidthClass {
  if (width < WIDTH_COMPACT_MAX) return 'compact'
  if (width < WIDTH_REGULAR_MAX) return 'regular'
  return 'expanded'
}

export function heightClassForPx(height: number): UiHeightClass {
  if (height < HEIGHT_SHORT_MAX) return 'short'
  if (height < HEIGHT_REGULAR_MAX) return 'regular'
  return 'tall'
}

const SSR_VIEWPORT: UiViewport = {
  widthClass: 'expanded',
  heightClass: 'regular',
  pointer: 'fine',
}

function readViewport(): UiViewport {
  if (typeof window === 'undefined') return SSR_VIEWPORT

  const visualViewport = window.visualViewport
  const layoutWidth = window.innerWidth
  const layoutHeight = window.innerHeight
  const width = visualViewport?.width ?? layoutWidth
  const height = visualViewport?.height ?? layoutHeight

  const pointer: UiPointer = window.matchMedia('(pointer: fine)').matches
    ? 'fine'
    : window.matchMedia('(pointer: coarse)').matches
      ? 'coarse'
      : 'none'

  let orientation: UiOrientation | undefined
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

  const viewport: UiViewport = {
    widthClass: widthClassForPx(width),
    heightClass: heightClassForPx(height),
    pointer,
  }
  if (orientation !== undefined) viewport.orientation = orientation
  if (keyboardOccluded !== undefined) viewport.keyboardOccluded = keyboardOccluded

  return viewport
}

/** Reactive viewport hook. Updates on resize / orientation / visualViewport changes. */
export function useViewport(): UiViewport {
  const [viewport, setViewport] = useState<UiViewport>(readViewport)

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

const WIDTH_FALLBACK_ORDER: Record<UiWidthClass, UiWidthClass[]> = {
  compact: ['compact', 'regular', 'expanded'],
  regular: ['regular', 'compact', 'expanded'],
  expanded: ['expanded', 'regular', 'compact'],
}

const HEIGHT_FALLBACK_ORDER: Record<UiHeightClass, UiHeightClass[]> = {
  short: ['short', 'regular', 'tall'],
  regular: ['regular', 'short', 'tall'],
  tall: ['tall', 'regular', 'short'],
}

/**
 * Resolve a `UiValue<T>` against the viewport.
 *
 * Fallback order per the adaptive spec: exact match, then next smaller class,
 * then next larger class. When both `width` and `height` are defined on a
 * responsive value, `width` wins (width is primary in the spec's examples).
 */
export function resolveValue<T>(
  value: UiValue<T> | undefined,
  viewport: UiViewport,
): T | undefined {
  if (value === undefined) return undefined
  if (!isResponsive(value)) return value

  const resolved = resolveResponsive(value, viewport)
  return resolved
}

function resolveResponsive<T>(
  value: UiResponsive<T>,
  viewport: UiViewport,
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
  cond: UiCondition,
  viewport: UiViewport,
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
