/**
 * Integration tests for the phase-1 composite builders.
 *
 * These builders emit `UiNodeV1` trees that the React registry consumes. They
 * prove the registry works end-to-end for a real phase-1 surface (hosted
 * preview indicator, preview error banner, session row).
 */
import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { UiTree, createRawDispatch } from '..'
import {
  hostedPreviewErrorInner,
  hostedPreviewIndicator,
  sessionRowTreeItem,
} from '../composites'
import type { UiChildV1, UiViewportV1 } from '../types'

const REGULAR_FINE: UiViewportV1 = {
  widthClass: 'expanded',
  heightClass: 'regular',
  pointer: 'fine',
}

afterEach(() => {
  cleanup()
})

function firstChildNodeType(children?: UiChildV1[]): string | null {
  const first = children?.[0]
  return first && 'type' in first ? first.type : null
}

describe('hostedPreviewIndicator', () => {
  it('returns null node for inactive / unavailable', () => {
    for (const status of ['inactive', 'unavailable'] as const) {
      const r = hostedPreviewIndicator({
        sessionId: 's',
        sessionUuid: 'u',
        hubId: 'h',
        status,
        density: 'panel',
      })
      expect(r.node).toBeNull()
    }
  })

  it('starting renders a warning badge with tooltip title', () => {
    const r = hostedPreviewIndicator({
      sessionId: 's',
      sessionUuid: 'u',
      hubId: 'h',
      status: 'starting',
      density: 'panel',
    })
    expect(r.node?.type).toBe('badge')
    expect(r.tooltipTitle).toMatch(/starting/i)
    render(
      <UiTree node={r.node!} dispatch={createRawDispatch(() => {})} viewport={REGULAR_FINE} />,
    )
    expect(screen.getByText(/Starting/)).toBeInTheDocument()
  })

  it('running + url renders a button with VISIBLE "Running" label', () => {
    const onAction = vi.fn()
    const r = hostedPreviewIndicator({
      sessionId: 's-1',
      sessionUuid: 'u-1',
      hubId: 'h-1',
      status: 'running',
      url: 'https://example.com',
      density: 'panel',
    })
    expect(r.node?.type).toBe('button')
    expect(r.tooltipTitle).toBe('Open Cloudflare preview')
    render(
      <UiTree
        node={r.node!}
        dispatch={createRawDispatch(onAction)}
        viewport={REGULAR_FINE}
      />,
    )
    expect(screen.getByText('Running')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button'))
    expect(onAction).toHaveBeenCalledOnce()
    expect(onAction.mock.calls[0]![0]).toEqual({
      id: 'botster.session.preview.open',
      payload: { sessionId: 's-1', sessionUuid: 'u-1', url: 'https://example.com' },
    })
  })

  it('error renders a danger badge and carries the error string as tooltip', () => {
    const r = hostedPreviewIndicator({
      sessionId: 's',
      sessionUuid: 'u',
      hubId: 'h',
      status: 'error',
      error: 'boom',
      density: 'panel',
    })
    expect(r.node?.type).toBe('badge')
    expect(r.tooltipTitle).toBe('boom')
    render(
      <UiTree node={r.node!} dispatch={createRawDispatch(() => {})} viewport={REGULAR_FINE} />,
    )
    expect(screen.getByText('Error')).toBeInTheDocument()
  })

  it('density affects badge size (sidebar=sm, panel=md)', () => {
    const sidebar = hostedPreviewIndicator({
      sessionId: 's',
      sessionUuid: 'u',
      hubId: 'h',
      status: 'starting',
      density: 'sidebar',
    })
    const panel = hostedPreviewIndicator({
      sessionId: 's',
      sessionUuid: 'u',
      hubId: 'h',
      status: 'starting',
      density: 'panel',
    })
    expect(sidebar.node?.props?.size).toBe('sm')
    expect(panel.node?.props?.size).toBe('md')
  })
})

describe('hostedPreviewErrorInner', () => {
  it('returns null when error is empty', () => {
    expect(
      hostedPreviewErrorInner({
        sessionUuid: 'u',
        error: null,
        density: 'panel',
      }),
    ).toBeNull()
  })

  it('renders inline icon + text row', () => {
    const node = hostedPreviewErrorInner({
      sessionUuid: 'u',
      error: 'cloudflared not found',
      density: 'panel',
    })!
    expect(node.type).toBe('stack')
    render(
      <UiTree node={node} dispatch={createRawDispatch(() => {})} viewport={REGULAR_FINE} />,
    )
    expect(screen.getByText('cloudflared not found')).toBeInTheDocument()
  })

  it('renders install button when installUrl is present and dispatches preview.open', () => {
    const onAction = vi.fn()
    const node = hostedPreviewErrorInner({
      sessionUuid: 'u-99',
      error: 'cloudflared not found',
      installUrl: 'https://cf.example/install',
      density: 'panel',
    })!
    render(
      <UiTree node={node} dispatch={createRawDispatch(onAction)} viewport={REGULAR_FINE} />,
    )
    const btn = screen.getByRole('button', { name: 'Install cloudflared' })
    fireEvent.click(btn)
    expect(onAction).toHaveBeenCalledOnce()
    expect(onAction.mock.calls[0]![0].id).toBe('botster.session.preview.open')
    expect(onAction.mock.calls[0]![0].payload).toEqual({
      sessionUuid: 'u-99',
      url: 'https://cf.example/install',
    })
  })

  it('density affects gap spacing', () => {
    const sidebar = hostedPreviewErrorInner({
      sessionUuid: 'u',
      error: 'x',
      density: 'sidebar',
    })!
    const panel = hostedPreviewErrorInner({
      sessionUuid: 'u',
      error: 'x',
      density: 'panel',
    })!
    expect(sidebar.props?.gap).toBe('1')
    expect(panel.props?.gap).toBe('2')
  })
})

describe('sessionRowTreeItem', () => {
  it('builds a tree_item with title/subtitle slots and select action', () => {
    const onAction = vi.fn()
    const node = sessionRowTreeItem({
      sessionId: 'sess-1',
      sessionUuid: 'uuid-1',
      primaryName: 'agent-one',
      titleLine: 'working on CI',
      subtext: 'target-x \u00b7 main',
      selected: false,
      notification: true,
      sessionType: 'agent',
      activityState: 'active',
      action: {
        id: 'botster.session.select',
        payload: { sessionId: 'sess-1', sessionUuid: 'uuid-1' },
      },
      density: 'panel',
    })
    expect(node.type).toBe('tree_item')
    expect(node.id).toBe('sess-1')

    const { container } = render(
      <UiTree
        node={{ type: 'tree', children: [node] }}
        dispatch={createRawDispatch(onAction)}
        viewport={REGULAR_FINE}
      />,
    )
    const item = container.querySelector('[data-session-id="sess-1"]')
    expect(item).not.toBeNull()
    expect(item?.getAttribute('data-notification')).toBe('true')
    expect(item?.textContent).toContain('agent-one')
    expect(item?.textContent).toContain('working on CI')
    expect(item?.textContent).toContain('target-x')

    const dot = container.querySelector('[role="status"]')
    expect(dot?.getAttribute('aria-label')).toBe('Active')

    const row = container.querySelector('[data-session-id="sess-1"] > div')
    fireEvent.click(row as Element)
    expect(onAction).toHaveBeenCalledOnce()
    expect(onAction.mock.calls[0]![0].id).toBe('botster.session.select')
  })

  it('includes hosted preview node in end slot when provided', () => {
    const previewNode = { type: 'badge', props: { text: 'Running', tone: 'success' } }
    const node = sessionRowTreeItem({
      sessionId: 'sess-2',
      sessionUuid: 'uuid-2',
      primaryName: 'agent-two',
      titleLine: null,
      subtext: '',
      selected: true,
      notification: false,
      sessionType: 'agent',
      activityState: 'idle',
      action: { id: 'botster.session.select', payload: {} },
      hostedPreviewNode: previewNode,
      density: 'panel',
    })
    expect(firstChildNodeType(node.slots?.['end'])).toBe('badge')
  })

  it('omits start slot for accessory activity state', () => {
    const node = sessionRowTreeItem({
      sessionId: 's',
      sessionUuid: 'u',
      primaryName: 'port-forward',
      titleLine: null,
      subtext: 'accessory',
      selected: false,
      notification: false,
      sessionType: 'accessory',
      activityState: 'accessory',
      action: { id: 'botster.session.select', payload: {} },
      density: 'panel',
    })
    expect(node.slots?.['start']).toBeUndefined()
  })

  it('density affects title text size (sidebar=xs, panel=sm)', () => {
    const sidebar = sessionRowTreeItem({
      sessionId: 's',
      sessionUuid: 'u',
      primaryName: 'n',
      titleLine: null,
      subtext: '',
      selected: false,
      notification: false,
      sessionType: 'agent',
      activityState: 'active',
      action: { id: 'x', payload: {} },
      density: 'sidebar',
    })
    const panel = sessionRowTreeItem({
      sessionId: 's',
      sessionUuid: 'u',
      primaryName: 'n',
      titleLine: null,
      subtext: '',
      selected: false,
      notification: false,
      sessionType: 'agent',
      activityState: 'active',
      action: { id: 'x', payload: {} },
      density: 'panel',
    })
    function firstTitleSize(node: ReturnType<typeof sessionRowTreeItem>) {
      const first = node.slots?.['title']?.[0]
      return first && 'props' in first ? first.props?.size : null
    }
    expect(firstTitleSize(sidebar)).toBe('xs')
    expect(firstTitleSize(panel)).toBe('sm')
  })
})
