import React from 'react'
import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest'
import { cleanup, render, screen, fireEvent } from '@testing-library/react'

vi.mock('../lib/hub-bridge', () => ({
  getHub: () => null,
}))

import { useWorkspaceStore } from '../store/workspace-store'
import { useDialogStore } from '../store/dialog-store'
import WorkspaceList from '../components/workspace/WorkspaceList'

afterEach(() => {
  cleanup()
  useWorkspaceStore.setState({
    sessionsById: {},
    sessionOrder: [],
    workspacesById: {},
    workspaceOrder: [],
    ungroupedSessionIds: [],
    selectedSessionId: null,
    collapsedWorkspaceIds: new Set(),
    connected: true,
  })
})

describe('WorkspaceList', () => {
  it('renders the empty state when there are no sessions', () => {
    useWorkspaceStore.setState({ sessionOrder: [], connected: true })
    const { container } = render(<WorkspaceList hubId="h1" surface="panel" />)
    expect(screen.getByText('No sessions running')).toBeInTheDocument()
    // EmptyState builder composes Stack + Text via primitives, so there's an
    // SVG child from the sparkle icon.
    expect(container.querySelector('svg')).not.toBeNull()
    // NewSession button is present with its test id + commandfor binding
    // preserved.
    const btn = screen.getByTestId('new-session-button')
    expect(btn.getAttribute('commandfor')).toBe('new-session-chooser-modal')
  })

  it('renders a disabled new-session button while disconnected', () => {
    useWorkspaceStore.setState({ sessionOrder: [], connected: false })
    render(<WorkspaceList hubId="h1" surface="panel" />)
    const btn = screen.getByTestId('new-session-button')
    expect(btn).toBeDisabled()
    expect(btn.textContent).toMatch(/Connecting/)
  })

  it('opens new-session dialog on click', () => {
    const openNewSession = vi.fn()
    useDialogStore.setState({ openNewSession })
    useWorkspaceStore.setState({ sessionOrder: [], connected: true })
    render(<WorkspaceList hubId="h1" surface="panel" />)
    fireEvent.click(screen.getByTestId('new-session-button'))
    expect(openNewSession).toHaveBeenCalledOnce()
  })

  it('renders session rows when sessions are present', () => {
    const session = {
      id: 's-1',
      session_uuid: 'u-1',
      label: 'agent-one',
      target_name: 'tgt',
      branch_name: 'main',
      is_idle: false,
    }
    useWorkspaceStore.setState({
      sessionsById: { 's-1': session },
      sessionOrder: ['s-1'],
      ungroupedSessionIds: ['s-1'],
      connected: true,
    })
    render(<WorkspaceList hubId="h1" surface="panel" />)
    expect(screen.getByText('agent-one')).toBeInTheDocument()
  })
})
