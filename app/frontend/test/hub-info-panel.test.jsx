import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import HubInfoPanel from '../components/settings/HubInfoPanel'

const navigate = vi.hoisted(() => vi.fn())

vi.mock('react-router-dom', async () => ({
  useNavigate: () => navigate,
}))

function createTestQueryClient() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
}

function renderHubInfoPanel(queryClient = createTestQueryClient()) {
  return render(
    <QueryClientProvider client={queryClient}>
      <HubInfoPanel
        hubId="hub-1"
        hubName="Old Hub"
        hubIdentifier="old-hub"
        hubSettingsPath="/hubs/hub-1/settings"
        hubPath="/hubs/hub-1"
      />
    </QueryClientProvider>,
  )
}

describe('HubInfoPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    document.head.innerHTML = '<meta name="csrf-token" content="csrf-token">'
  })

  afterEach(() => {
    cleanup()
  })

  it('saves hub identity through a React Query mutation', async () => {
    const user = userEvent.setup()
    globalThis.fetch = vi.fn(() =>
      Promise.resolve({
        ok: true,
        redirected: false,
        json: () => Promise.resolve({}),
      }),
    )

    renderHubInfoPanel()

    const input = screen.getByDisplayValue('Old Hub')
    await user.clear(input)
    await user.type(input, 'New Hub')
    await user.click(screen.getByRole('button', { name: 'Save' }))

    await waitFor(() => {
      expect(globalThis.fetch).toHaveBeenCalledWith('/hubs/hub-1/settings', {
        method: 'PATCH',
        headers: {
          'Content-Type': 'application/json',
          'X-CSRF-Token': 'csrf-token',
          Accept: 'application/json',
        },
        body: JSON.stringify({ hub: { name: 'New Hub' } }),
        redirect: 'follow',
      })
    })
    expect(navigate).toHaveBeenCalledWith('/hubs/hub-1')
  })
})
