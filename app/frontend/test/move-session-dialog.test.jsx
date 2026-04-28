import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { cleanup, render, screen } from '@testing-library/react'
import MoveSessionDialog from '../components/dialogs/MoveSessionDialog'
import { useDialogStore } from '../store/dialog-store'
import {
  _resetEntityStoresForTest,
  useSessionStore,
  useWorkspaceEntityStore,
} from '../store/entities'

vi.mock('../components/catalyst/dialog', () => ({
  Dialog: ({ open, children }) => (open ? <div>{children}</div> : null),
  DialogTitle: ({ children }) => <h2>{children}</h2>,
  DialogDescription: ({ children }) => <div>{children}</div>,
  DialogBody: ({ children }) => <div>{children}</div>,
  DialogActions: ({ children }) => <div>{children}</div>,
}))

vi.mock('../components/catalyst/input', () => ({
  Input: (props) => <input {...props} />,
}))

vi.mock('../components/catalyst/button', () => ({
  Button: ({ children, ...props }) => <button type={props.type || 'button'} {...props}>{children}</button>,
}))

function seedMoveDialogState() {
  useWorkspaceEntityStore.getState().applySnapshot(
    [
      { workspace_id: 'ws-current', name: 'Current', status: 'active' },
      { workspace_id: 'ws-target', name: 'Target', status: 'active' },
    ],
    1,
  )
  useSessionStore.getState().applySnapshot(
    [
      {
        session_uuid: 'sess-current',
        label: 'Codex',
        workspace_id: 'ws-current',
        status: 'active',
      },
      {
        session_uuid: 'sess-target',
        label: 'Claude',
        workspace_id: 'ws-target',
        status: 'active',
      },
    ],
    1,
  )
  useDialogStore.setState({
    activeDialog: 'move',
    context: { sessionId: 'sess-current', sessionUuid: 'sess-current' },
  })
}

describe('MoveSessionDialog', () => {
  beforeEach(() => {
    _resetEntityStoresForTest()
    useDialogStore.setState({ activeDialog: null, context: {} })
  })

  afterEach(() => {
    cleanup()
    _resetEntityStoresForTest()
  })

  it('renders existing workspace choices without a Headless Label context error', () => {
    seedMoveDialogState()

    expect(() => render(<MoveSessionDialog hubId="hub-1" />)).not.toThrow()
    expect(screen.getByText('Existing workspaces')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Target' })).toBeInTheDocument()
  })
})
