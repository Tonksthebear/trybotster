import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { act, cleanup, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import NewAgentForm from '../components/forms/NewAgentForm'
import { useDialogStore } from '../store/dialog-store'
import {
  _resetEntityStoresForTest,
  useSpawnTargetStore,
  useWorktreeStore,
} from '../store/entities'

const listeners = {}

const mockHub = {
  agents: {
    load: vi.fn(() => Promise.resolve([])),
  },
  getAgentConfig: vi.fn(() => ({ agents: [], accessories: [], workspaces: [] })),
  hasAgentConfig: vi.fn(() => false),
  ensureAgentConfig: vi.fn(() => Promise.resolve()),
  requestSpawnTargets: vi.fn(),
  requestOpenWorkspaces: vi.fn(),
  requestWorktrees: vi.fn(),
  on: vi.fn((eventName, callback) => {
    listeners[eventName] = callback
    return () => {
      delete listeners[eventName]
    }
  }),
}

vi.mock('../lib/hub-bridge', () => ({
  waitForHub: vi.fn(() => Promise.resolve(mockHub)),
}))

vi.mock('../components/catalyst/dialog', () => ({
  Dialog: ({ open, children }) => (open ? <div>{children}</div> : null),
  DialogTitle: ({ children }) => <h2>{children}</h2>,
  DialogDescription: ({ children }) => <div>{children}</div>,
  DialogBody: ({ children }) => <div>{children}</div>,
  DialogActions: ({ children }) => <div>{children}</div>,
}))

vi.mock('../components/catalyst/fieldset', () => ({
  Field: ({ children }) => <div>{children}</div>,
  Label: ({ children }) => <label>{children}</label>,
  Description: ({ children }) => <div>{children}</div>,
}))

vi.mock('../components/catalyst/input', () => ({
  Input: (props) => <input {...props} />,
}))

vi.mock('../components/catalyst/select', () => ({
  Select: ({ children, ...props }) => <select {...props}>{children}</select>,
}))

vi.mock('../components/catalyst/button', () => ({
  Button: ({ children, ...props }) => <button type="button" {...props}>{children}</button>,
}))

describe('NewAgentForm', () => {
  beforeEach(() => {
    Object.keys(listeners).forEach((key) => delete listeners[key])
    vi.clearAllMocks()
    mockHub.getAgentConfig.mockReturnValue({ agents: [], accessories: [], workspaces: [] })
    mockHub.hasAgentConfig.mockReturnValue(false)
    mockHub.ensureAgentConfig.mockResolvedValue({ agents: [], accessories: [], workspaces: [] })
    _resetEntityStoresForTest()
    useSpawnTargetStore.getState().applySnapshot(
      [{ id: 'target-1', name: 'Repo' }],
      1,
    )
    useDialogStore.setState({
      activeDialog: 'newAgent',
      context: { targetId: 'target-1' },
    })
  })

  afterEach(() => {
    cleanup()
    _resetEntityStoresForTest()
  })

  it('renders worktrees from the target-scoped worktree entity store', async () => {
    render(<NewAgentForm hubId="hub-1" />)

    await act(async () => {
      useWorktreeStore.getState().applySnapshot(
        [
          {
            worktree_path: '/wt/feature-a',
            path: '/wt/feature-a',
            target_id: 'target-1',
            branch: 'feature-a',
            active_sessions: 1,
          },
          {
            worktree_path: '/wt/wrong',
            path: '/wt/wrong',
            target_id: 'target-2',
            branch: 'wrong-branch',
          },
        ],
        1,
      )
    })

    expect(screen.getByText('feature-a')).toBeInTheDocument()
    expect(screen.getByText('1 active')).toBeInTheDocument()
    expect(screen.queryByText('wrong-branch')).not.toBeInTheDocument()
  })

  it('shows loading instead of empty-config warning while agent config is pending', async () => {
    const user = userEvent.setup()
    let resolveConfig
    mockHub.ensureAgentConfig.mockImplementation(
      () => new Promise((resolve) => {
        resolveConfig = resolve
      }),
    )

    render(<NewAgentForm hubId="hub-1" />)

    await user.click(screen.getByText('Main branch'))

    expect(screen.getByText('Loading agent configurations for this spawn target...')).toBeInTheDocument()
    expect(screen.queryByText(/No agent configurations found/)).not.toBeInTheDocument()

    await act(async () => {
      resolveConfig({ agents: ['claude'], accessories: [], workspaces: [] })
    })

    expect(await screen.findByText('Claude')).toBeInTheDocument()
    expect(screen.queryByText(/No agent configurations found/)).not.toBeInTheDocument()
  })
})
