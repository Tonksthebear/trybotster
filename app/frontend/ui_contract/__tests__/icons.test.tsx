import React from 'react'
import { afterEach, describe, expect, it } from 'vitest'
import { cleanup, render } from '@testing-library/react'
import { UiTreeBody, createRawDispatch } from '..'
import { IconGlyph, hasIcon } from '../icons'
import type { UiNode, UiViewport } from '../types'

afterEach(() => {
  cleanup()
})

const REGULAR_FINE: UiViewport = {
  widthClass: 'expanded',
  heightClass: 'regular',
  pointer: 'fine',
}

describe('icons registry', () => {
  const required = [
    'external-link',
    'close',
    'chevron-down',
    'chevron-right',
    'ellipsis-vertical',
    'plus',
    'trash',
    'pencil',
    'globe',
    'arrows-right-left',
    'command-line',
    'sparkle',
    'cog',
    'exclamation-triangle',
    'workspace',
  ]

  it('all required icons have registered glyphs', () => {
    for (const name of required) {
      expect(hasIcon(name), `icon "${name}" missing from registry`).toBe(true)
    }
  })

  it('IconGlyph renders an SVG with the glyph path', () => {
    const { container } = render(<IconGlyph name="external-link" />)
    const svg = container.querySelector('svg')
    expect(svg).not.toBeNull()
    expect(svg?.getAttribute('viewBox')).toBe('0 0 20 20')
    expect(svg?.querySelector('path')).not.toBeNull()
  })

  it('IconGlyph returns null for unknown icon names', () => {
    const { container } = render(<IconGlyph name="non-existent-xyz" />)
    expect(container.querySelector('svg')).toBeNull()
  })
})

describe('icon primitive renders a visible SVG', () => {
  it('renders SVG child for a registered name', () => {
    const node: UiNode = {
      type: 'icon',
      props: { name: 'external-link', size: 'sm' },
    }
    const { container } = render(
      <UiTreeBody node={node} dispatch={createRawDispatch(() => {})} viewport={REGULAR_FINE} />,
    )
    const span = container.querySelector('[data-icon="external-link"]')
    expect(span).not.toBeNull()
    expect(span?.querySelector('svg')).not.toBeNull()
  })

  it('icon_button renders an SVG inside the button', () => {
    const node: UiNode = {
      type: 'icon_button',
      props: {
        icon: 'close',
        label: 'Close',
        action: { id: 'botster.session.close.request' },
      },
    }
    const { container } = render(
      <UiTreeBody node={node} dispatch={createRawDispatch(() => {})} viewport={REGULAR_FINE} />,
    )
    expect(container.querySelector('button svg')).not.toBeNull()
  })

  it('button with leading icon renders an SVG', () => {
    const node: UiNode = {
      type: 'button',
      props: {
        label: 'Save',
        icon: 'plus',
        action: { id: 'botster.workspace.save' },
      },
    }
    const { container } = render(
      <UiTreeBody node={node} dispatch={createRawDispatch(() => {})} viewport={REGULAR_FINE} />,
    )
    expect(container.querySelector('button svg')).not.toBeNull()
    expect(container.textContent).toContain('Save')
  })

  it('empty_state icon renders an SVG', () => {
    const node: UiNode = {
      type: 'empty_state',
      props: { title: 'Empty', icon: 'sparkle' },
    }
    const { container } = render(
      <UiTreeBody node={node} dispatch={createRawDispatch(() => {})} viewport={REGULAR_FINE} />,
    )
    expect(container.querySelector('svg')).not.toBeNull()
  })
})
