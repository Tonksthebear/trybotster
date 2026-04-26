import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { render, screen, cleanup } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router-dom'
import HubSwitcher from '../components/hub/HubSwitcher'
import { useHubStore } from '../store/hub-store'

// Mock hub-bridge so store actions don't hit real connections
vi.mock('../lib/hub-bridge', () => ({
  connect: vi.fn(() => Promise.resolve({ connectionId: 1 })),
  disconnect: vi.fn(),
}))

function renderSwitcher(storeOverrides = {}) {
  useHubStore.setState({
    hubList: [],
    hubListLoading: false,
    selectedHubId: null,
    connectionState: 'disconnected',
    connectionDetail: '',
    _connectionRef: null,
    _statusUnsub: null,
    ...storeOverrides,
  })

  return render(
    <MemoryRouter>
      <HubSwitcher />
    </MemoryRouter>
  )
}

describe('HubSwitcher', () => {
  afterEach(() => {
    cleanup()
  })

  it('shows "Select a hub" when no hub is selected', () => {
    renderSwitcher()
    expect(screen.getByText('Select a hub')).toBeInTheDocument()
  })

  it('renders with hub list and shows selected hub name', () => {
    renderSwitcher({
      hubList: [
        { id: 1, name: 'My Hub', identifier: 'hub-1', active: true },
        { id: 2, name: 'Other Hub', identifier: 'hub-2', active: false },
      ],
      selectedHubId: 1,
      connectionState: 'connected',
    })

    expect(screen.getByText('My Hub')).toBeInTheDocument()
  })

  it('shows a green status dot when connected', () => {
    renderSwitcher({
      hubList: [{ id: 1, name: 'My Hub', identifier: 'hub-1', active: true }],
      selectedHubId: 1,
      connectionState: 'connected',
    })

    // The status dot should have the emerald (green) class
    const dot = document.querySelector('.bg-emerald-500')
    expect(dot).toBeInTheDocument()
  })

  it('shows a yellow pulsing dot when connecting', () => {
    renderSwitcher({
      hubList: [{ id: 1, name: 'My Hub', identifier: 'hub-1', active: true }],
      selectedHubId: 1,
      connectionState: 'connecting',
    })

    const dot = document.querySelector('.animate-pulse')
    expect(dot).toBeInTheDocument()
  })

  it('shows a red dot on error', () => {
    renderSwitcher({
      hubList: [{ id: 1, name: 'My Hub', identifier: 'hub-1', active: true }],
      selectedHubId: 1,
      connectionState: 'error',
    })

    const dot = document.querySelector('.bg-red-500')
    expect(dot).toBeInTheDocument()
  })

  it('lists all hubs when dropdown is opened', async () => {
    const user = userEvent.setup()

    renderSwitcher({
      hubList: [
        { id: 1, name: 'Hub Alpha', identifier: 'alpha', active: true },
        { id: 2, name: 'Hub Beta', identifier: 'beta', active: false },
      ],
      selectedHubId: 1,
      connectionState: 'connected',
    })

    // Click the dropdown trigger
    const trigger = screen.getByRole('button', { name: /switch hub/i })
    await user.click(trigger)

    // Hub Alpha appears in both trigger and menu, so use getAllByText
    const alphaElements = screen.getAllByText('Hub Alpha')
    expect(alphaElements.length).toBeGreaterThanOrEqual(2) // trigger + menu item
    expect(screen.getByText('Hub Beta')).toBeInTheDocument()
  })

  it('shows "Connect new hub" link in dropdown', async () => {
    const user = userEvent.setup()

    renderSwitcher({
      hubList: [{ id: 1, name: 'Hub Alpha', identifier: 'alpha', active: true }],
    })

    const trigger = screen.getByRole('button', { name: /switch hub/i })
    await user.click(trigger)

    expect(screen.getByText('Connect new hub')).toBeInTheDocument()
  })
})
