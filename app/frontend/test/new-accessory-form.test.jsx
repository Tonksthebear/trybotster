import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { act, cleanup, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import NewAccessoryForm from '../components/forms/NewAccessoryForm'
import { useDialogStore } from '../store/dialog-store'
import {
  _resetEntityStoresForTest,
  useSpawnTargetStore,
  useWorktreeStore,
} from '../store/entities'

const mockHub = {
  createAccessory: vi.fn(() => Promise.resolve(true)),
  ensureAgentConfig: vi.fn(() => Promise.resolve({
    agents: [],
    accessories: ['rails-server'],
    workspaces: [],
  })),
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

vi.mock('../components/catalyst/select', () => ({
  Select: ({ children, ...props }) => <select {...props}>{children}</select>,
}))

vi.mock('../components/catalyst/button', () => ({
  Button: ({ children, ...props }) => <button type="button" {...props}>{children}</button>,
}))

function renderNewAccessoryForm() {
  return render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <NewAccessoryForm hubId="hub-1" />
    </QueryClientProvider>,
  )
}

describe('NewAccessoryForm', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    _resetEntityStoresForTest()
    useSpawnTargetStore.getState().applySnapshot(
      [{ id: 'target-1', name: 'Repo' }],
      1,
    )
    useDialogStore.setState({
      activeDialog: 'newAccessory',
      context: { targetId: 'target-1' },
    })
  })

  afterEach(() => {
    cleanup()
    _resetEntityStoresForTest()
  })

  it('creates an accessory in a selected existing worktree', async () => {
    const user = userEvent.setup()

    renderNewAccessoryForm()

    await act(async () => {
      useWorktreeStore.getState().applySnapshot(
        [
          {
            worktree_path: '/wt/feature-a',
            path: '/wt/feature-a',
            target_id: 'target-1',
            branch: 'feature-a',
          },
        ],
        1,
      )
    })

    await user.click(await screen.findByText('feature-a'))
    await user.click(await screen.findByText('rails-server'))
    await user.click(screen.getByText('Create Accessory'))

    expect(mockHub.createAccessory).toHaveBeenCalledWith(
      'rails-server',
      null,
      null,
      'target-1',
      { fromWorktree: '/wt/feature-a', branch: 'feature-a' },
    )
  })
})
