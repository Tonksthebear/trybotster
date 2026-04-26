import React from 'react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router-dom'

import SettingsRoute from '../components/pages/SettingsRoute'
import { resetSettingsBootstrapCacheForTests } from '../store/settings-bootstrap-store'

vi.mock('../components/settings/SettingsApp', () => ({
  default: ({ hubName }) => <div>{`SettingsApp:${hubName}`}</div>,
}))

function renderSettingsRoute() {
  return render(
    <MemoryRouter initialEntries={['/hubs/1/settings']}>
      <Routes>
        <Route path="/hubs/:hubId/settings" element={<SettingsRoute />} />
      </Routes>
    </MemoryRouter>,
  )
}

describe('SettingsRoute', () => {
  beforeEach(() => {
    resetSettingsBootstrapCacheForTests()
    vi.clearAllMocks()
  })

  afterEach(() => {
    cleanup()
    resetSettingsBootstrapCacheForTests()
  })

  it('uses cached settings data on warm remounts instead of flashing loading', async () => {
    globalThis.fetch = vi.fn(() =>
      Promise.resolve({
        ok: true,
        json: () => Promise.resolve({ hubName: 'Hub One' }),
      }),
    )

    const first = renderSettingsRoute()
    expect(screen.getByText('Loading settings...')).toBeInTheDocument()
    expect(await screen.findByText('SettingsApp:Hub One')).toBeInTheDocument()
    first.unmount()

    globalThis.fetch = vi.fn(() => new Promise(() => {}))
    renderSettingsRoute()

    expect(screen.getByText('SettingsApp:Hub One')).toBeInTheDocument()
    await waitFor(() => {
      expect(screen.queryByText('Loading settings...')).not.toBeInTheDocument()
    })
  })
})
