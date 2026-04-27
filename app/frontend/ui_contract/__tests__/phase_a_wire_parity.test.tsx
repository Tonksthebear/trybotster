/**
 * Phase A wire-format parity tests.
 *
 * Each fixture below is copied verbatim from the Rust JSON shapes asserted in
 * `cli/tests/ui_contract_lua_test.rs`. If these stop rendering, the TS
 * renderer has drifted from the Phase A wire contract.
 */
import React from 'react'
import { afterEach, describe, expect, it } from 'vitest'
import { cleanup, render } from '@testing-library/react'
import { UiTreeBody, createRawDispatch } from '..'
import type { UiNode, UiViewport } from '../types'

afterEach(() => {
  cleanup()
})

const EXPANDED_FINE: UiViewport = {
  widthClass: 'expanded',
  heightClass: 'regular',
  pointer: 'fine',
}

const COMPACT_COARSE: UiViewport = {
  widthClass: 'compact',
  heightClass: 'regular',
  pointer: 'coarse',
}

function renderTree(node: UiNode, viewport = EXPANDED_FINE) {
  return render(
    <UiTreeBody
      node={node}
      dispatch={createRawDispatch(() => {})}
      viewport={viewport}
    />,
  )
}

describe('Phase A wire-format parity', () => {
  // Sourced from stack_wire_shape_and_typed_props_round_trip
  it('stack with scalar direction + gap renders as flex-col', () => {
    const fixture: UiNode = {
      type: 'stack',
      props: { direction: 'vertical', gap: '2' },
    }
    const { container } = renderTree(fixture)
    expect((container.firstChild as HTMLElement).className).toContain(
      'flex-col',
    )
    expect((container.firstChild as HTMLElement).className).toContain('gap-2')
  })

  // Sourced from panel_typed_round_trip_with_interaction_density
  it('panel with interactionDensity=comfortable renders with its label', () => {
    const fixture: UiNode = {
      type: 'panel',
      props: {
        title: 'Preview',
        tone: 'muted',
        border: true,
        interactionDensity: 'comfortable',
      },
    }
    const { getByText } = renderTree(fixture)
    expect(getByText('Preview')).toBeInTheDocument()
  })

  // Sourced from responsive_value_embeds_inside_primitive_prop
  it('stack.direction = responsive({compact: vertical, expanded: horizontal}) resolves at render time', () => {
    const fixture: UiNode = {
      type: 'stack',
      props: {
        direction: {
          $kind: 'responsive',
          width: { compact: 'vertical', expanded: 'horizontal' },
        },
        gap: '2',
      },
    }
    const expanded = renderTree(fixture, EXPANDED_FINE)
    expect(
      (expanded.container.firstChild as HTMLElement).className,
    ).toContain('flex-row')
    cleanup()

    const compact = renderTree(fixture, COMPACT_COARSE)
    expect(
      (compact.container.firstChild as HTMLElement).className,
    ).toContain('flex-col')
  })

  // Sourced from when_hidden_wrappers_accepted_in_children_position
  it('stack with $kind=when + $kind=hidden children resolves both', () => {
    const fixture: UiNode = {
      type: 'stack',
      props: { direction: 'vertical' },
      children: [
        {
          $kind: 'when',
          condition: { width: 'expanded' },
          node: { type: 'text', props: { text: 'Desktop only' } },
        },
        {
          $kind: 'hidden',
          condition: { width: 'compact' },
          node: { type: 'badge', props: { text: 'hidden-on-compact' } },
        },
      ],
    }

    const expanded = renderTree(fixture, EXPANDED_FINE)
    expect(expanded.container.textContent).toContain('Desktop only')
    expect(expanded.container.textContent).toContain('hidden-on-compact')
    cleanup()

    const compact = renderTree(fixture, COMPACT_COARSE)
    expect(compact.container.textContent).not.toContain('Desktop only')
    expect(compact.container.textContent).not.toContain('hidden-on-compact')
  })

  // Sourced from tree_item_with_all_optional_slots
  it('tree_item with all slots including children renders full structure', () => {
    const fixture: UiNode = {
      type: 'tree_item',
      id: 'ws-1',
      props: {
        selected: true,
        expanded: true,
        notification: true,
        action: {
          id: 'botster.workspace.toggle',
          payload: { workspaceId: 'ws-1' },
        },
      },
      slots: {
        title: [{ type: 'text', props: { text: 'Workspace' } }],
        subtitle: [{ type: 'text', props: { text: '3 sessions' } }],
        start: [{ type: 'status_dot', props: { state: 'active' } }],
        end: [{ type: 'badge', props: { text: '3' } }],
        children: [
          {
            type: 'tree_item',
            id: 'sess-1',
            slots: {
              title: [{ type: 'text', props: { text: 'sess' } }],
            },
          },
        ],
      },
    }
    const { container } = renderTree(fixture)
    const item = container.querySelector('[data-session-id="ws-1"]')
    expect(item).not.toBeNull()
    expect(item?.getAttribute('aria-selected')).toBe('true')
    expect(item?.getAttribute('aria-expanded')).toBe('true')
    expect(item?.getAttribute('data-notification')).toBe('true')
    expect(item?.textContent).toContain('Workspace')
    expect(item?.textContent).toContain('3 sessions')
    expect(item?.textContent).toContain('sess')
  })

  // Sourced from dialog_wire_shape_with_hoisted_slots
  it('dialog with hoisted body+footer slots renders both', () => {
    const fixture: UiNode = {
      type: 'dialog',
      props: {
        open: true,
        title: 'Rename Workspace',
        presentation: 'sheet',
      },
      slots: {
        body: [{ type: 'text', props: { text: 'Enter a new name' } }],
        footer: [
          {
            type: 'button',
            props: {
              label: 'Save',
              action: { id: 'botster.workspace.rename.commit' },
            },
          },
        ],
      },
    }
    const { container } = renderTree(fixture)
    const dialog = container.querySelector('[role="dialog"]')
    expect(dialog).not.toBeNull()
    expect(dialog?.getAttribute('data-presentation')).toBe('sheet')
    expect(dialog?.textContent).toContain('Enter a new name')
    expect(dialog?.textContent).toContain('Save')
  })

  // Icon button with payload
  it('icon_button with action.payload round-trips via data-action-id', () => {
    const fixture: UiNode = {
      type: 'icon_button',
      props: {
        icon: 'close',
        label: 'Close session',
        action: {
          id: 'botster.session.close.request',
          payload: { sessionUuid: 'sess-42' },
        },
        tone: 'danger',
      },
    }
    const { container } = renderTree(fixture)
    const btn = container.querySelector(
      '[data-action-id="botster.session.close.request"]',
    )
    expect(btn).not.toBeNull()
    expect(btn?.getAttribute('aria-label')).toBe('Close session')
  })
})
