// Wire protocol — fidelity restoration tests for `<SessionList>`.
//
// Verifies the v1 row contract: activity dot, two-line content
// (primaryName + titleLine + subtext), inline hosted-preview indicator,
// inline error panel for status==='error', and an actions trigger that
// dispatches `botster.session.menu.open` for `<SessionActionsMenu>` to
// pick up.

import React from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react'

import { SessionList } from '../components/composites/SessionList'
import { useSessionStore, useWorkspaceEntityStore } from '../store/entities'
import { useUiPresentationStore } from '../store/ui-presentation-store'

function fakeCtx(overrides = {}) {
  return {
    hubId: 'hub-1',
    viewport: {
      widthClass: 'regular',
      heightClass: 'regular',
      pointer: 'fine',
    },
    capabilities: {
      hover: true,
      dialog: true,
      tooltip: true,
      externalLinks: true,
      binaryTerminalSnapshots: false,
    },
    dispatch: vi.fn(),
    ...overrides,
  }
}

function seedSession(session) {
  useSessionStore.setState({
    byId: { [session.id ?? session.session_uuid]: session },
    order: [session.id ?? session.session_uuid],
    snapshotSeq: 1,
  })
}

beforeEach(() => {
  useSessionStore.getState()._reset()
  useWorkspaceEntityStore.getState()._reset()
  useUiPresentationStore.getState()._reset()
})

afterEach(() => {
  cleanup()
})

describe('<SessionList> v1-fidelity row', () => {
  it('renders the green activity dot only when is_idle === false', () => {
    seedSession({
      id: 'sess-1',
      session_uuid: 'uuid-1',
      session_type: 'agent',
      label: 'api-work',
      is_idle: false,
    })
    const ctx = fakeCtx()
    render(<SessionList density="panel" grouping="flat" ctx={ctx} />)
    expect(screen.getByLabelText('Active')).toBeInTheDocument()

    cleanup()
    seedSession({
      id: 'sess-2',
      session_uuid: 'uuid-2',
      session_type: 'agent',
      is_idle: true,
    })
    render(<SessionList density="panel" grouping="flat" ctx={ctx} />)
    expect(screen.queryByLabelText('Active')).toBeNull()
  })

  it('renders primaryName + titleLine + subtext on separate lines', () => {
    seedSession({
      id: 'sess-1',
      session_uuid: 'uuid-1',
      session_type: 'agent',
      label: 'api-work',
      title: 'Refactor request path',
      task: 'Trim dead routes',
      target_name: 'backend',
      branch_name: 'feature/api',
      agent_name: 'claude',
    })
    render(<SessionList density="panel" grouping="flat" ctx={fakeCtx()} />)

    const primary = screen.getByTestId('session-row-primary')
    expect(primary).toHaveTextContent('api-work')

    const title = screen.getByTestId('session-row-title-line')
    expect(title).toHaveTextContent('Refactor request path')
    expect(title).toHaveTextContent('Trim dead routes')

    const sub = screen.getByTestId('session-row-subtext')
    expect(sub).toHaveTextContent('backend')
    expect(sub).toHaveTextContent('feature/api')
    expect(sub).toHaveTextContent('claude')
  })

  it('renders the hosted-preview "Running" button when status === "running" with url', () => {
    seedSession({
      id: 'sess-1',
      session_uuid: 'uuid-1',
      session_type: 'agent',
      port: 8080,
      hosted_preview: { status: 'running', url: 'https://preview.test' },
    })
    const ctx = fakeCtx()
    render(<SessionList density="panel" grouping="flat" ctx={ctx} />)
    const running = screen.getByTestId('hosted-preview-running')
    fireEvent.click(running)
    expect(ctx.dispatch).toHaveBeenCalledWith(
      expect.objectContaining({
        id: 'botster.session.preview.open',
        payload: expect.objectContaining({ url: 'https://preview.test' }),
      }),
      expect.any(Object),
    )
  })

  it('renders the inline error panel when hosted_preview.status === "error"', () => {
    seedSession({
      id: 'sess-1',
      session_uuid: 'uuid-1',
      session_type: 'agent',
      port: 8080,
      hosted_preview: {
        status: 'error',
        error: 'cloudflared not installed',
        install_url: 'https://install.cloudflared.test',
      },
    })
    const ctx = fakeCtx()
    render(<SessionList density="panel" grouping="flat" ctx={ctx} />)

    const errorPanel = screen.getByTestId('hosted-preview-error')
    expect(errorPanel).toHaveTextContent('cloudflared not installed')

    const installButton = within(errorPanel).getByRole('button', {
      name: /Install cloudflared/i,
    })
    fireEvent.click(installButton)
    expect(ctx.dispatch).toHaveBeenCalledWith(
      expect.objectContaining({
        id: 'botster.session.preview.open',
        payload: expect.objectContaining({
          url: 'https://install.cloudflared.test',
        }),
      }),
      expect.any(Object),
    )
  })

  it('actions trigger dispatches botster.session.menu.open with sessionId/uuid', () => {
    seedSession({
      id: 'sess-1',
      session_uuid: 'uuid-1',
      session_type: 'agent',
    })
    const ctx = fakeCtx()
    render(<SessionList density="panel" grouping="flat" ctx={ctx} />)
    const trigger = screen.getByTestId('session-actions-trigger')
    fireEvent.click(trigger)
    expect(ctx.dispatch).toHaveBeenCalledWith(
      expect.objectContaining({
        id: 'botster.session.menu.open',
        payload: { sessionId: 'sess-1', sessionUuid: 'uuid-1' },
      }),
      expect.any(Object),
    )
  })

  it('selecting a row dispatches botster.session.select and updates the presentation store', () => {
    seedSession({
      id: 'sess-1',
      session_uuid: 'uuid-1',
      session_type: 'agent',
      label: 'api',
    })
    const ctx = fakeCtx()
    render(<SessionList density="panel" grouping="flat" ctx={ctx} />)
    const link = screen.getByRole('link', { name: /api/ })
    fireEvent.click(link)
    expect(ctx.dispatch).toHaveBeenCalledWith(
      expect.objectContaining({ id: 'botster.session.select' }),
      expect.any(Object),
    )
    expect(useUiPresentationStore.getState().selectedSessionId).toBe('uuid-1')
  })

  it('accessory sessions show the "accessory" subtext discriminator', () => {
    seedSession({
      id: 'sess-1',
      session_uuid: 'uuid-1',
      session_type: 'accessory',
      label: 'editor',
    })
    render(<SessionList density="panel" grouping="flat" ctx={fakeCtx()} />)
    const sub = screen.getByTestId('session-row-subtext')
    expect(sub).toHaveTextContent('accessory')
  })

  it('renders the empty state when no sessions are in the store', () => {
    render(<SessionList density="panel" grouping="flat" ctx={fakeCtx()} />)
    expect(screen.getByText(/No sessions running/i)).toBeInTheDocument()
  })

  it('renders SVG icons (IconGlyph) for the workspace chevron and the actions trigger', () => {
    useWorkspaceEntityStore.setState({
      byId: { 'ws-1': { workspace_id: 'ws-1', name: 'live' } },
      order: ['ws-1'],
      snapshotSeq: 1,
    })
    seedSession({
      id: 'sess-1',
      session_uuid: 'uuid-1',
      session_type: 'agent',
      label: 'work',
      workspace_id: 'ws-1',
    })
    render(<SessionList density="panel" grouping="workspace" ctx={fakeCtx()} />)
    const trigger = screen.getByTestId('session-actions-trigger')
    expect(trigger.querySelector('svg[data-slot="icon"]')).not.toBeNull()
    const chevronHeader = screen.getByRole('button', { name: /live/i })
    expect(chevronHeader.querySelector('svg[data-slot="icon"]')).not.toBeNull()
  })

  it('does not render a header for a workspace whose status === "closed"', () => {
    useWorkspaceEntityStore.setState({
      byId: {
        'ws-open': { workspace_id: 'ws-open', name: 'open-ws' },
        'ws-closed': {
          workspace_id: 'ws-closed',
          name: 'closed-ws',
          status: 'closed',
        },
      },
      order: ['ws-open', 'ws-closed'],
      snapshotSeq: 1,
    })
    useSessionStore.setState({
      byId: {
        'sess-open': {
          id: 'sess-open',
          session_uuid: 'uuid-open',
          session_type: 'agent',
          label: 'live',
          workspace_id: 'ws-open',
        },
        'sess-closed': {
          id: 'sess-closed',
          session_uuid: 'uuid-closed',
          session_type: 'agent',
          label: 'orphan',
          workspace_id: 'ws-closed',
        },
      },
      order: ['sess-open', 'sess-closed'],
      snapshotSeq: 1,
    })
    render(<SessionList density="panel" grouping="workspace" ctx={fakeCtx()} />)
    expect(screen.getByText('open-ws')).toBeInTheDocument()
    expect(screen.queryByText('closed-ws')).toBeNull()
  })
})
