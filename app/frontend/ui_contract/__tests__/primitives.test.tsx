import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render, screen, fireEvent } from '@testing-library/react'
import { UiTree, createRawDispatch } from '..'
import type { UiActionV1, UiNodeV1, UiViewportV1 } from '../types'

afterEach(() => {
  cleanup()
})

const REGULAR_FINE: UiViewportV1 = {
  widthClass: 'expanded',
  heightClass: 'regular',
  pointer: 'fine',
}

function renderTree(
  node: UiNodeV1,
  opts: { viewport?: UiViewportV1; onAction?: (action: UiActionV1) => void } = {},
): ReturnType<typeof render> {
  const handler = vi.fn(opts.onAction ?? (() => {}))
  return render(
    <UiTree
      node={node}
      dispatch={createRawDispatch(handler)}
      viewport={opts.viewport ?? REGULAR_FINE}
    />,
  )
}

describe('ui_contract registry — layout primitives', () => {
  it('stack renders flex column for vertical direction', () => {
    const { container } = renderTree({
      type: 'stack',
      props: { direction: 'vertical', gap: '2' },
      children: [
        { type: 'text', props: { text: 'one' } },
        { type: 'text', props: { text: 'two' } },
      ],
    })
    const stack = container.firstChild as HTMLElement
    expect(stack.className).toContain('flex')
    expect(stack.className).toContain('flex-col')
    expect(stack.className).toContain('gap-2')
    expect(stack.textContent).toContain('one')
    expect(stack.textContent).toContain('two')
  })

  it('stack renders flex row for horizontal direction', () => {
    const { container } = renderTree({
      type: 'stack',
      props: { direction: 'horizontal' },
      children: [{ type: 'text', props: { text: 'x' } }],
    })
    expect((container.firstChild as HTMLElement).className).toContain('flex-row')
  })

  it('inline renders flex-row with wrap', () => {
    const { container } = renderTree({
      type: 'inline',
      props: { gap: '1', wrap: true, justify: 'between' },
      children: [{ type: 'text', props: { text: 'a' } }],
    })
    const el = container.firstChild as HTMLElement
    expect(el.className).toContain('flex-row')
    expect(el.className).toContain('flex-wrap')
    expect(el.className).toContain('justify-between')
  })

  it('panel renders title and border', () => {
    const { container, getByText } = renderTree({
      type: 'panel',
      props: { title: 'Preview error', tone: 'muted', border: true },
      children: [{ type: 'text', props: { text: 'body' } }],
    })
    expect(getByText('Preview error')).toBeInTheDocument()
    expect((container.firstChild as HTMLElement).className).toContain('border')
  })

  it('scroll_area renders overflow-y-auto by default', () => {
    const { container } = renderTree({
      type: 'scroll_area',
      children: [{ type: 'text', props: { text: 'content' } }],
    })
    expect((container.firstChild as HTMLElement).className).toContain(
      'overflow-y-auto',
    )
  })

  it('scroll_area honors explicit axis', () => {
    const { container } = renderTree({
      type: 'scroll_area',
      props: { axis: 'x' },
    })
    expect((container.firstChild as HTMLElement).className).toContain(
      'overflow-x-auto',
    )
  })
})

describe('ui_contract registry — content primitives', () => {
  it('text applies tone, size, weight, italic, monospace, truncate', () => {
    renderTree({
      type: 'text',
      props: {
        text: 'hello',
        tone: 'accent',
        size: 'md',
        weight: 'semibold',
        italic: true,
        monospace: true,
        truncate: true,
      },
    })
    const span = screen.getByText('hello')
    expect(span.className).toContain('text-sky-400')
    expect(span.className).toContain('text-base')
    expect(span.className).toContain('font-semibold')
    expect(span.className).toContain('italic')
    expect(span.className).toContain('font-mono')
    expect(span.className).toContain('truncate')
  })

  it('icon exposes name via data-icon', () => {
    const { container } = renderTree({
      type: 'icon',
      props: { name: 'workspace', label: 'Workspaces', size: 'sm' },
    })
    const el = container.querySelector('[data-icon="workspace"]')
    expect(el).not.toBeNull()
    expect(el?.getAttribute('aria-label')).toBe('Workspaces')
  })

  it('badge renders tone + text', () => {
    renderTree({
      type: 'badge',
      props: { text: 'Running', tone: 'success', size: 'sm' },
    })
    const badge = screen.getByText('Running')
    expect(badge.className).toContain('text-emerald-700')
  })

  it('status_dot exposes state via aria-label', () => {
    const { container } = renderTree({
      type: 'status_dot',
      props: { state: 'active', label: 'Running' },
    })
    const dot = container.querySelector('[role="status"]')
    expect(dot?.getAttribute('aria-label')).toBe('Running')
    expect(dot?.className).toContain('bg-emerald-400')
  })

  it('empty_state renders title, description, icon, and primary action', () => {
    const onAction = vi.fn()
    renderTree(
      {
        type: 'empty_state',
        props: {
          title: 'No sessions yet',
          description: 'Spawn one.',
          icon: 'sparkle',
          primaryAction: {
            id: 'botster.session.create.request',
            payload: {},
          },
        },
      },
      { onAction },
    )
    expect(screen.getByText('No sessions yet')).toBeInTheDocument()
    expect(screen.getByText('Spawn one.')).toBeInTheDocument()
    const button = screen.getByRole('button')
    fireEvent.click(button)
    expect(onAction).toHaveBeenCalledOnce()
    expect(onAction.mock.calls[0]![0].id).toBe('botster.session.create.request')
  })
})

describe('ui_contract registry — action primitives', () => {
  it('button dispatches its action on click', () => {
    const onAction = vi.fn()
    renderTree(
      {
        type: 'button',
        props: {
          label: 'Save',
          action: {
            id: 'botster.workspace.save',
            payload: { workspaceId: 'w1' },
          },
          variant: 'solid',
          tone: 'accent',
        },
      },
      { onAction },
    )
    const btn = screen.getByRole('button', { name: 'Save' })
    fireEvent.click(btn)
    expect(onAction).toHaveBeenCalledOnce()
    expect(onAction.mock.calls[0]![0]).toEqual({
      id: 'botster.workspace.save',
      payload: { workspaceId: 'w1' },
    })
  })

  it('button respects action.disabled', () => {
    const onAction = vi.fn()
    renderTree(
      {
        type: 'button',
        props: {
          label: 'Save',
          action: { id: 'x', disabled: true },
        },
      },
      { onAction },
    )
    const btn = screen.getByRole('button', { name: 'Save' })
    expect(btn).toBeDisabled()
    fireEvent.click(btn)
    expect(onAction).not.toHaveBeenCalled()
  })

  it('icon_button carries accessible label and fires action', () => {
    const onAction = vi.fn()
    renderTree(
      {
        type: 'icon_button',
        props: {
          icon: 'close',
          label: 'Close session',
          action: {
            id: 'botster.session.close.request',
            payload: { sessionUuid: 'sess-1' },
          },
        },
      },
      { onAction },
    )
    const btn = screen.getByRole('button', { name: 'Close session' })
    fireEvent.click(btn)
    expect(onAction).toHaveBeenCalledOnce()
  })
})

describe('ui_contract registry — collection primitives', () => {
  it('tree renders a role=tree container with items', () => {
    const { container } = renderTree({
      type: 'tree',
      children: [
        {
          type: 'tree_item',
          id: 'sess-1',
          props: { selected: true },
          slots: {
            title: [{ type: 'text', props: { text: 'Primary' } }],
            subtitle: [{ type: 'text', props: { text: 'Secondary' } }],
          },
        },
      ],
    })
    expect(container.querySelector('[role="tree"]')).not.toBeNull()
    const item = container.querySelector('[role="treeitem"]')
    expect(item?.getAttribute('aria-selected')).toBe('true')
    expect(item?.getAttribute('data-session-id')).toBe('sess-1')
    expect(item?.textContent).toContain('Primary')
    expect(item?.textContent).toContain('Secondary')
  })

  it('tree_item fires action on click', () => {
    const onAction = vi.fn()
    const { container } = renderTree(
      {
        type: 'tree',
        children: [
          {
            type: 'tree_item',
            id: 'sess-1',
            props: {
              action: {
                id: 'botster.session.select',
                payload: { sessionUuid: 'sess-1' },
              },
            },
            slots: {
              title: [{ type: 'text', props: { text: 'Primary' } }],
            },
          },
        ],
      },
      { onAction },
    )
    const row = container.querySelector('[data-session-id="sess-1"] > div')
    expect(row).not.toBeNull()
    fireEvent.click(row as Element)
    expect(onAction).toHaveBeenCalledOnce()
    expect(onAction.mock.calls[0]![0].id).toBe('botster.session.select')
  })

  it('tree_item honors children slot only when expanded !== false', () => {
    const { container, rerender } = renderTree({
      type: 'tree_item',
      id: 'ws-1',
      props: { expanded: false },
      slots: {
        title: [{ type: 'text', props: { text: 'Workspace A' } }],
        children: [
          { type: 'text', props: { text: 'Child one' } },
        ],
      },
    })
    expect(container.querySelector('[role="group"]')).toBeNull()

    rerender(
      <UiTree
        node={{
          type: 'tree_item',
          id: 'ws-1',
          props: { expanded: true },
          slots: {
            title: [{ type: 'text', props: { text: 'Workspace A' } }],
            children: [{ type: 'text', props: { text: 'Child one' } }],
          },
        }}
        dispatch={() => {}}
        viewport={REGULAR_FINE}
      />,
    )
    expect(container.querySelector('[role="group"]')).not.toBeNull()
    expect(container.textContent).toContain('Child one')
  })
})

describe('ui_contract registry — dialog', () => {
  it('dialog renders nothing when open=false', () => {
    const { container } = renderTree({
      type: 'dialog',
      props: { open: false, title: 'x' },
    })
    expect(container.querySelector('[role="dialog"]')).toBeNull()
  })

  it('dialog renders sheet on compact auto', () => {
    const { container } = renderTree(
      {
        type: 'dialog',
        props: { open: true, title: 'Rename', presentation: 'auto' },
      },
      { viewport: { widthClass: 'compact', heightClass: 'regular', pointer: 'coarse' } },
    )
    const el = container.querySelector('[role="dialog"]')
    expect(el).not.toBeNull()
    expect(el?.getAttribute('data-presentation')).toBe('sheet')
  })

  it('dialog renders overlay on expanded auto', () => {
    const { container } = renderTree(
      {
        type: 'dialog',
        props: { open: true, title: 'Rename', presentation: 'auto' },
      },
      { viewport: { widthClass: 'expanded', heightClass: 'regular', pointer: 'fine' } },
    )
    expect(
      container.querySelector('[role="dialog"]')?.getAttribute('data-presentation'),
    ).toBe('overlay')
  })

  it('dialog renders fullscreen on compact+short auto', () => {
    const { container } = renderTree(
      {
        type: 'dialog',
        props: { open: true, title: 'Rename', presentation: 'auto' },
      },
      { viewport: { widthClass: 'compact', heightClass: 'short', pointer: 'coarse' } },
    )
    expect(
      container.querySelector('[role="dialog"]')?.getAttribute('data-presentation'),
    ).toBe('fullscreen')
  })
})

describe('ui_contract registry — unknown primitive', () => {
  it('warns and renders nothing for an unknown type', () => {
    const spy = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const { container } = renderTree({ type: 'never-seen-before' })
    expect(container.firstChild).toBeNull()
    expect(spy).toHaveBeenCalled()
    spy.mockRestore()
  })
})
