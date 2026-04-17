import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { act, cleanup, render, screen } from '@testing-library/react'
import NewAgentForm from '../components/forms/NewAgentForm'
import { useDialogStore } from '../store/dialog-store'

const listeners = {}

const mockHub = {
  spawnTargets: {
    current: vi.fn(() => [{ id: 'target-1', name: 'Repo' }]),
    load: vi.fn(() => Promise.resolve()),
    onChange: vi.fn(() => () => {}),
  },
  openWorkspaces: {
    current: vi.fn(() => []),
    load: vi.fn(() => Promise.resolve()),
    onChange: vi.fn(() => () => {}),
  },
  agents: {
    load: vi.fn(() => Promise.resolve([])),
  },
  getWorktrees: vi.fn(() => []),
  getAgentConfig: vi.fn(() => ({ agents: [], accessories: [], workspaces: [] })),
  hasWorktrees: vi.fn(() => false),
  ensureWorktrees: vi.fn(() => Promise.resolve([])),
  ensureAgentConfig: vi.fn(() => Promise.resolve()),
  on: vi.fn((eventName, callback) => {
    listeners[eventName] = callback
    return () => {
      delete listeners[eventName]
    }
  }),
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
    useDialogStore.setState({
      activeDialog: 'newAgent',
      context: { targetId: 'target-1' },
    })
  })

  afterEach(() => {
    cleanup()
  })

  it('keeps target-scoped worktrees when an unscoped broadcast arrives later', async () => {
    render(<NewAgentForm hubId="hub-1" />)

    await act(async () => {
      listeners.worktreeList({
        targetId: 'target-1',
        worktrees: [{ path: '/wt/feature-a', branch: 'feature-a', active_sessions: 1 }],
      })
    })

    expect(screen.getByText('feature-a')).toBeInTheDocument()
    expect(screen.getByText('1 active')).toBeInTheDocument()

    await act(async () => {
      listeners.worktreeList({
        worktrees: [{ path: '/wt/wrong', branch: 'wrong-branch' }],
      })
    })

    expect(screen.getByText('feature-a')).toBeInTheDocument()
    expect(screen.queryByText('wrong-branch')).not.toBeInTheDocument()
  })
})
