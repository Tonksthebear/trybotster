import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'

import SettingsRoute from '../components/pages/SettingsRoute'

vi.mock('../components/settings/SettingsApp', () => ({
  default: ({ hubName }) => <div>{`SettingsApp:${hubName}`}</div>,
}))

function createTestQueryClient() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
}

function renderSettingsRoute(queryClient = createTestQueryClient()) {
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={['/hubs/1/settings']}>
        <Routes>
          <Route path="/hubs/:hubId/settings" element={<SettingsRoute />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  )
}

describe('SettingsRoute', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  afterEach(() => {
    cleanup()
  })

  it('uses cached settings data on warm remounts instead of flashing loading', async () => {
    globalThis.fetch = vi.fn(() =>
      Promise.resolve({
        ok: true,
        json: () => Promise.resolve({ hubName: 'Hub One' }),
      }),
    )

    const queryClient = createTestQueryClient()
    const first = renderSettingsRoute(queryClient)
    expect(screen.getByText('Loading settings...')).toBeInTheDocument()
    expect(await screen.findByText('SettingsApp:Hub One')).toBeInTheDocument()
    first.unmount()

    globalThis.fetch = vi.fn(() => new Promise(() => {}))
    renderSettingsRoute(queryClient)

    expect(screen.getByText('SettingsApp:Hub One')).toBeInTheDocument()
    await waitFor(() => {
      expect(screen.queryByText('Loading settings...')).not.toBeInTheDocument()
    })
  })
})
