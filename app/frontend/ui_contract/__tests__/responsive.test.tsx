import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from '@testing-library/react'
import { UiTreeBody, createRawDispatch, resolveValue } from '..'
import type { UiNodeV1, UiResponsiveV1, UiViewportV1 } from '../types'

afterEach(() => {
  cleanup()
})

const COMPACT: UiViewportV1 = {
  widthClass: 'compact',
  heightClass: 'regular',
  pointer: 'coarse',
}
const REGULAR: UiViewportV1 = {
  widthClass: 'regular',
  heightClass: 'regular',
  pointer: 'fine',
}
const EXPANDED: UiViewportV1 = {
  widthClass: 'expanded',
  heightClass: 'regular',
  pointer: 'fine',
}
const SHORT: UiViewportV1 = {
  widthClass: 'regular',
  heightClass: 'short',
  pointer: 'fine',
}
const TALL: UiViewportV1 = {
  widthClass: 'regular',
  heightClass: 'tall',
  pointer: 'fine',
}

function renderTree(node: UiNodeV1, viewport: UiViewportV1) {
  return render(
    <UiTreeBody
      node={node}
      dispatch={createRawDispatch(() => {})}
      viewport={viewport}
    />,
  )
}

describe('resolveValue — width dimension', () => {
  it('returns scalar value unchanged', () => {
    expect(resolveValue('md', REGULAR)).toBe('md')
  })

  it('exact width match wins', () => {
    const value: UiResponsiveV1<string> = {
      $kind: 'responsive',
      width: { compact: 'a', regular: 'b', expanded: 'c' },
    }
    expect(resolveValue(value, COMPACT)).toBe('a')
    expect(resolveValue(value, REGULAR)).toBe('b')
    expect(resolveValue(value, EXPANDED)).toBe('c')
  })

  it('falls back to next smaller then next larger', () => {
    // Only regular defined, viewport is compact — next smaller missing, goes
    // to next larger (regular).
    const onlyRegular: UiResponsiveV1<string> = {
      $kind: 'responsive',
      width: { regular: 'R' },
    }
    expect(resolveValue(onlyRegular, COMPACT)).toBe('R')
    expect(resolveValue(onlyRegular, EXPANDED)).toBe('R')

    // Expanded viewport, only compact defined — falls through the order
    // [expanded, regular, compact] and picks compact.
    const onlyCompact: UiResponsiveV1<string> = {
      $kind: 'responsive',
      width: { compact: 'C' },
    }
    expect(resolveValue(onlyCompact, EXPANDED)).toBe('C')
  })
})

describe('resolveValue — height dimension', () => {
  it('exact height match wins', () => {
    const v: UiResponsiveV1<string> = {
      $kind: 'responsive',
      height: { short: 'S', regular: 'R', tall: 'T' },
    }
    expect(resolveValue(v, SHORT)).toBe('S')
    expect(resolveValue(v, REGULAR)).toBe('R')
    expect(resolveValue(v, TALL)).toBe('T')
  })
})

describe('resolveValue — width wins over height when both present', () => {
  it('uses width dimension first if any width breakpoint resolves', () => {
    const v: UiResponsiveV1<string> = {
      $kind: 'responsive',
      width: { regular: 'FROM_W' },
      height: { regular: 'FROM_H' },
    }
    expect(resolveValue(v, REGULAR)).toBe('FROM_W')
  })
})

describe('Stack direction responsive resolution', () => {
  it('resolves compact→vertical and expanded→horizontal at render time', () => {
    const node: UiNodeV1 = {
      type: 'stack',
      props: {
        direction: {
          $kind: 'responsive',
          width: { compact: 'vertical', expanded: 'horizontal' },
        },
      },
      children: [{ type: 'text', props: { text: 'x' } }],
    }

    const compact = renderTree(node, COMPACT)
    expect((compact.container.firstChild as HTMLElement).className).toContain(
      'flex-col',
    )
    cleanup()

    const expanded = renderTree(node, EXPANDED)
    expect((expanded.container.firstChild as HTMLElement).className).toContain(
      'flex-row',
    )
  })
})

describe('Conditional wrappers', () => {
  it('ui.when renders its node only when condition matches', () => {
    const node: UiNodeV1 = {
      type: 'stack',
      props: { direction: 'vertical' },
      children: [
        {
          $kind: 'when',
          condition: { width: 'expanded' },
          node: { type: 'text', props: { text: 'WIDE' } },
        },
        {
          $kind: 'when',
          condition: { width: 'compact' },
          node: { type: 'text', props: { text: 'NARROW' } },
        },
      ],
    }

    const compact = renderTree(node, COMPACT)
    expect(compact.container.textContent).not.toContain('WIDE')
    expect(compact.container.textContent).toContain('NARROW')
    cleanup()

    const expanded = renderTree(node, EXPANDED)
    expect(expanded.container.textContent).toContain('WIDE')
    expect(expanded.container.textContent).not.toContain('NARROW')
  })

  it('ui.hidden renders its node only when condition does NOT match', () => {
    const node: UiNodeV1 = {
      type: 'stack',
      props: { direction: 'vertical' },
      children: [
        {
          $kind: 'hidden',
          condition: { width: 'compact' },
          node: { type: 'text', props: { text: 'NON-COMPACT' } },
        },
      ],
    }
    const compact = renderTree(node, COMPACT)
    expect(compact.container.textContent).not.toContain('NON-COMPACT')
    cleanup()
    const expanded = renderTree(node, EXPANDED)
    expect(expanded.container.textContent).toContain('NON-COMPACT')
  })

  it('condition with multiple fields requires all to match', () => {
    const node: UiNodeV1 = {
      type: 'stack',
      props: { direction: 'vertical' },
      children: [
        {
          $kind: 'when',
          condition: { width: 'compact', pointer: 'coarse' },
          node: { type: 'text', props: { text: 'MOBILE' } },
        },
      ],
    }

    const match = renderTree(node, {
      widthClass: 'compact',
      heightClass: 'regular',
      pointer: 'coarse',
    })
    expect(match.container.textContent).toContain('MOBILE')
    cleanup()

    const partial = renderTree(node, {
      widthClass: 'compact',
      heightClass: 'regular',
      pointer: 'fine',
    })
    expect(partial.container.textContent).not.toContain('MOBILE')
  })
})
