// Wire protocol — fidelity restoration tests for `<SessionList>`.
//
// Verifies the current row contract: activity dot, two-line content
// (primaryName + titleLine + subtext), inline generic session-action
// indicators, inline error panel for action errors, and an actions trigger that
// dispatches `botster.session.menu.open` for `<SessionActionsMenu>` to
// pick up.

import React from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react'

import { SessionList } from '../components/composites/SessionList'
import { WorkspaceList } from '../components/composites/WorkspaceList'
import {
  useSessionActionStore,
  useSessionStore,
  useWorkspaceEntityStore,
} from '../store/entities'
import { useRouteRegistryStore } from '../store/route-registry-store'
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

function seedSessionActions(actions) {
  useSessionActionStore.setState({
    byId: Object.fromEntries(actions.map((action) => [action.id, action])),
    order: actions.map((action) => action.id),
    snapshotSeq: 1,
  })
}

beforeEach(() => {
  useSessionStore.getState()._reset()
  useSessionActionStore.getState()._reset()
  useWorkspaceEntityStore.getState()._reset()
  useRouteRegistryStore.setState({
    routesByHubId: {},
    snapshotReceivedAtByHubId: {},
  })
  useUiPresentationStore.getState()._reset()
})

afterEach(() => {
  cleanup()
})

describe('<SessionList> fidelity row', () => {
  it('left-border color reflects row state with notification > active > idle priority', () => {
    // Active session → emerald border, no notification.
    seedSession({
      id: 'sess-active',
      session_uuid: 'uuid-active',
      session_type: 'agent',
      label: 'api-work',
      output_activity: 'active',
    })
    const ctx = fakeCtx()
    render(<SessionList density="panel" grouping="flat" ctx={ctx} />)
    expect(screen.getByTestId('session-row-primary').closest('[data-row-state]'))
      .toHaveAttribute('data-row-state', 'active')

    cleanup()

    // Idle session → zinc/gray border.
    seedSession({
      id: 'sess-idle',
      session_uuid: 'uuid-idle',
      session_type: 'agent',
      output_activity: 'idle',
    })
    render(<SessionList density="panel" grouping="flat" ctx={ctx} />)
    expect(screen.getByTestId('session-row-primary').closest('[data-row-state]'))
      .toHaveAttribute('data-row-state', 'idle')

    cleanup()

    // Notification + active → notification wins.
    seedSession({
      id: 'sess-notif',
      session_uuid: 'uuid-notif',
      session_type: 'agent',
      output_activity: 'active',
      notification: true,
    })
    render(<SessionList density="panel" grouping="flat" ctx={ctx} />)
    expect(screen.getByTestId('session-row-primary').closest('[data-row-state]'))
      .toHaveAttribute('data-row-state', 'notification')
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
      agent_name: 'codex',
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
    expect(sub).toHaveTextContent('codex')
  })

  it('renders a linked action status when a session action has status and url', () => {
    seedSession({
      id: 'sess-1',
      session_uuid: 'uuid-1',
      session_type: 'agent',
    })
    seedSessionActions([
      {
        id: 'uuid-1:cloudflare.preview.open',
        session_uuid: 'uuid-1',
        action_id: 'cloudflare.preview.open',
        label: 'Cloudflare preview',
        status: 'running',
        url: 'https://preview.test',
        visibility: 'visible',
        enabled: true,
      },
    ])
    const ctx = fakeCtx()
    render(<SessionList density="panel" grouping="flat" ctx={ctx} />)
    const running = screen.getByTestId('session-action-link')
    fireEvent.click(running)
    expect(ctx.dispatch).toHaveBeenCalledWith(
      expect.objectContaining({
        id: 'botster.url.open',
        payload: expect.objectContaining({ url: 'https://preview.test' }),
      }),
      expect.any(Object),
    )
  })

  it('renders the inline error panel when a session action reports an error', () => {
    seedSession({
      id: 'sess-1',
      session_uuid: 'uuid-1',
      session_type: 'agent',
    })
    seedSessionActions([
      {
        id: 'uuid-1:cloudflare.preview.toggle',
        session_uuid: 'uuid-1',
        action_id: 'cloudflare.preview.toggle',
        label: 'Cloudflare preview',
        status: 'error',
        error: 'cloudflared not installed',
        install_url: 'https://install.cloudflared.test',
        visibility: 'visible',
        enabled: true,
      },
    ])
    const ctx = fakeCtx()
    render(<SessionList density="panel" grouping="flat" ctx={ctx} />)

    const errorPanel = screen.getByTestId('session-action-error')
    expect(errorPanel).toHaveTextContent('cloudflared not installed')

    const installButton = within(errorPanel).getByRole('button', {
      name: /Open Cloudflare preview/i,
    })
    fireEvent.click(installButton)
    expect(ctx.dispatch).toHaveBeenCalledWith(
      expect.objectContaining({
        id: 'botster.url.open',
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
    const chevronHeader = screen
      .getAllByRole('button', { name: /live/i })
      .find((button) => button.getAttribute('aria-expanded') === 'true')
    expect(chevronHeader.querySelector('svg[data-slot="icon"]')).not.toBeNull()
  })

  it('workspace rename button dispatches botster.workspace.rename.request', () => {
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
    const ctx = fakeCtx()
    render(<SessionList density="panel" grouping="workspace" ctx={ctx} />)

    fireEvent.click(screen.getByRole('button', { name: /rename workspace live/i }))

    expect(ctx.dispatch).toHaveBeenCalledWith(
      expect.objectContaining({
        id: 'botster.workspace.rename.request',
        payload: { workspaceId: 'ws-1', title: 'live' },
      }),
      expect.any(Object),
    )
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

  it('does not render workspace headers after the last active agent leaves', () => {
    useWorkspaceEntityStore.setState({
      byId: {
        'ws-live': { workspace_id: 'ws-live', name: 'live-ws' },
        'ws-empty': { workspace_id: 'ws-empty', name: 'empty-ws' },
        'ws-accessory': { workspace_id: 'ws-accessory', name: 'accessory-ws' },
      },
      order: ['ws-live', 'ws-empty', 'ws-accessory'],
      snapshotSeq: 1,
    })
    useSessionStore.setState({
      byId: {
        'sess-live': {
          id: 'sess-live',
          session_uuid: 'uuid-live',
          session_type: 'agent',
          label: 'agent',
          workspace_id: 'ws-live',
        },
        'sess-accessory': {
          id: 'sess-accessory',
          session_uuid: 'uuid-accessory',
          session_type: 'accessory',
          label: 'editor',
          workspace_id: 'ws-accessory',
        },
      },
      order: ['sess-live', 'sess-accessory'],
      snapshotSeq: 1,
    })

    render(<SessionList density="panel" grouping="workspace" ctx={fakeCtx()} />)

    expect(screen.getByText('live-ws')).toBeInTheDocument()
    expect(screen.queryByText('empty-ws')).toBeNull()
    expect(screen.queryByText('accessory-ws')).toBeNull()
  })

  it('filters plugin-scoped sessions out of the default workspace list', () => {
    useSessionStore.setState({
      byId: {
        regular: {
          id: 'regular',
          session_uuid: 'regular',
          session_type: 'agent',
          label: 'regular',
          visibility: 'workspace',
        },
        vault: {
          id: 'vault',
          session_uuid: 'vault',
          session_type: 'agent',
          label: 'vault worker',
          owner_plugin: 'vault',
          visibility: 'plugin',
          surface: 'vault',
        },
      },
      order: ['regular', 'vault'],
      snapshotSeq: 1,
    })

    render(<SessionList density="panel" grouping="flat" ctx={fakeCtx()} />)
    expect(screen.getByText('regular')).toBeInTheDocument()
    expect(screen.queryByText('vault worker')).toBeNull()

    cleanup()

    render(
      <SessionList
        density="panel"
        grouping="flat"
        visibility="plugin"
        ownerPlugin="vault"
        ctx={fakeCtx()}
      />,
    )
    expect(screen.queryByText('regular')).toBeNull()
    expect(screen.getByText('vault worker')).toBeInTheDocument()
  })

  it('routes plugin-owned sessions through the owning surface', () => {
    useRouteRegistryStore.getState().setRoutes('hub-1', [
      {
        path: '/vault',
        base_path: '/vault',
        surface: 'vault',
        label: 'Vault',
        icon: 'book-open',
        nav: { section: 'workspace', order: 25 },
      },
    ])
    seedSession({
      id: 'sess-vault',
      session_uuid: 'uuid-vault',
      session_type: 'agent',
      label: 'vault worker',
      owner_plugin: 'vault',
      visibility: 'plugin',
      surface: 'vault',
    })

    render(
      <SessionList
        density="sidebar"
        grouping="flat"
        visibility="plugin"
        ctx={fakeCtx()}
      />,
    )

    expect(screen.getByText('vault worker').closest('a'))
      .toHaveAttribute('href', '/hubs/hub-1/vault/sessions/uuid-vault')
  })
})

describe('<WorkspaceList>', () => {
  it('only renders workspaces that have an active agent session', () => {
    useWorkspaceEntityStore.setState({
      byId: {
        'ws-live': { workspace_id: 'ws-live', name: 'live-ws' },
        'ws-empty': { workspace_id: 'ws-empty', name: 'empty-ws' },
        'ws-accessory': { workspace_id: 'ws-accessory', name: 'accessory-ws' },
      },
      order: ['ws-live', 'ws-empty', 'ws-accessory'],
      snapshotSeq: 1,
    })
    useSessionStore.setState({
      byId: {
        'sess-live': {
          id: 'sess-live',
          session_uuid: 'uuid-live',
          session_type: 'agent',
          workspace_id: 'ws-live',
        },
        'sess-accessory': {
          id: 'sess-accessory',
          session_uuid: 'uuid-accessory',
          session_type: 'accessory',
          workspace_id: 'ws-accessory',
        },
      },
      order: ['sess-live', 'sess-accessory'],
      snapshotSeq: 1,
    })

    render(<WorkspaceList ctx={fakeCtx()} />)

    expect(screen.getByText('live-ws')).toBeInTheDocument()
    expect(screen.queryByText('empty-ws')).toBeNull()
    expect(screen.queryByText('accessory-ws')).toBeNull()
  })
})
