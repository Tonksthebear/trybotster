import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { cleanup, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import DeleteSessionDialog from '../components/dialogs/DeleteSessionDialog'
import { useDialogStore } from '../store/dialog-store'
import { useWorkspaceStore } from '../store/workspace-store'

const mockHub = {
  deleteAgent: vi.fn(),
}

vi.mock('../lib/hub-bridge', () => ({
  getHub: vi.fn(() => mockHub),
}))

vi.mock('../components/catalyst/dialog', () => ({
  Dialog: ({ open, children }) => (open ? <div>{children}</div> : null),
  DialogTitle: ({ children }) => <h2>{children}</h2>,
  DialogDescription: ({ children }) => <div>{children}</div>,
  DialogBody: ({ children }) => <div>{children}</div>,
  DialogActions: ({ children }) => <div>{children}</div>,
}))

vi.mock('../components/catalyst/button', () => ({
  Button: ({ children, onClick }) => (
    <button type="button" onClick={onClick}>
      {children}
    </button>
  ),
}))

function setDialogState(sessionId = 'sess-1') {
  useDialogStore.setState({
    activeDialog: 'delete',
    context: { sessionId, sessionUuid: sessionId },
  })
}

function setSession(session) {
  useWorkspaceStore.setState({
    sessionsById: { [session.id]: session },
    sessionOrder: [session.id],
    workspacesById: {},
    workspaceOrder: [],
    ungroupedSessionIds: [session.id],
    selectedSessionId: session.id,
    collapsedWorkspaceIds: new Set(),
    connected: true,
    surface: 'agent_list',
  })
}

describe('DeleteSessionDialog', () => {
  beforeEach(() => {
    mockHub.deleteAgent.mockReset()
    useDialogStore.setState({ activeDialog: null, context: {} })
    useWorkspaceStore.setState({
      sessionsById: {},
      sessionOrder: [],
      workspacesById: {},
      workspaceOrder: [],
      ungroupedSessionIds: [],
      selectedSessionId: null,
      collapsedWorkspaceIds: new Set(),
      connected: false,
      surface: 'agent_list',
    })
  })

  afterEach(() => {
    cleanup()
  })

  it('shows delete-worktree actions when the hub capability allows it', () => {
    setSession({
      id: 'sess-1',
      session_uuid: 'sess-1',
      label: 'Claude Code',
      display_name: 'Claude Code',
      in_worktree: true,
      close_actions: {
        can_close: true,
        can_delete_worktree: true,
        delete_worktree_reason: null,
        other_active_sessions: 0,
      },
    })
    setDialogState()

    render(<DeleteSessionDialog hubId="hub-1" />)

    expect(screen.getByText('Close Session')).toBeInTheDocument()
    expect(screen.getByText(/This session has a worktree on disk\./)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Close & delete worktree' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Close, keep worktree' })).toBeInTheDocument()
  })

  it('hides destructive cleanup and explains why when other sessions are active', () => {
    setSession({
      id: 'sess-1',
      session_uuid: 'sess-1',
      label: 'Claude Code',
      display_name: 'Claude Code',
      in_worktree: true,
      close_actions: {
        can_close: true,
        can_delete_worktree: false,
        delete_worktree_reason: 'other_sessions_active',
        other_active_sessions: 1,
      },
    })
    setDialogState()

    render(<DeleteSessionDialog hubId="hub-1" />)

    expect(screen.queryByRole('button', { name: 'Close & delete worktree' })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Close session' })).toBeInTheDocument()
    expect(
      screen.getByText(/The workspace stays open because other sessions are still active\./)
    ).toBeInTheDocument()
    expect(
      screen.getByText(
        'Another session is still active in this workspace, so closing this session will keep the workspace on disk.'
      )
    ).toBeInTheDocument()
  })

  it('explains when the session is not running in a detachable worktree', () => {
    setSession({
      id: 'sess-1',
      session_uuid: 'sess-1',
      label: 'Claude Code',
      display_name: 'Claude Code',
      in_worktree: false,
      close_actions: {
        can_close: true,
        can_delete_worktree: false,
        delete_worktree_reason: 'not_in_worktree',
        other_active_sessions: 0,
      },
    })
    setDialogState()

    render(<DeleteSessionDialog hubId="hub-1" />)

    expect(screen.queryByRole('button', { name: 'Close & delete worktree' })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Close session' })).toBeInTheDocument()
    expect(
      screen.getByText(/This session is using the repo checkout, not a detachable worktree\./)
    ).toBeInTheDocument()
    expect(
      screen.getByText(
        'Closing this session will leave the repository untouched because it is not running in a separate worktree.'
      )
    ).toBeInTheDocument()
  })

  it('sends a delete-worktree close request when the destructive action is chosen', async () => {
    const user = userEvent.setup()

    setSession({
      id: 'sess-1',
      session_uuid: 'sess-1',
      label: 'Claude Code',
      display_name: 'Claude Code',
      in_worktree: true,
      close_actions: {
        can_close: true,
        can_delete_worktree: true,
        delete_worktree_reason: null,
        other_active_sessions: 0,
      },
    })
    setDialogState()

    render(<DeleteSessionDialog hubId="hub-1" />)
    await user.click(screen.getByRole('button', { name: 'Close & delete worktree' }))

    expect(mockHub.deleteAgent).toHaveBeenCalledWith('sess-1', true)
  })
})
