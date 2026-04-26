import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { render, screen, act, cleanup } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import ConnectionOverlay from '../components/hub/ConnectionOverlay'
import { useHubStore } from '../store/hub-store'

// Mock hub-bridge
vi.mock('../lib/hub-bridge', () => ({
  connect: vi.fn(() => Promise.resolve({ connectionId: 1 })),
  disconnect: vi.fn(),
}))

function renderOverlay(storeOverrides = {}) {
  useHubStore.setState({
    hubList: [{ id: 1, name: 'Test Hub', identifier: 'test-hub', active: true }],
    hubListLoading: false,
    selectedHubId: 1,
    connectionState: 'connecting',
    connectionDetail: 'Connecting to hub...',
    _connectionRef: null,
    _statusUnsub: null,
    ...storeOverrides,
  })

  return render(
    <MemoryRouter>
      <ConnectionOverlay />
    </MemoryRouter>
  )
}

describe('ConnectionOverlay', () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true })
  })

  afterEach(() => {
    cleanup()
    vi.useRealTimers()
  })

  it('does not render when no hub is selected', () => {
    renderOverlay({ selectedHubId: null, connectionState: 'disconnected' })
    expect(screen.queryByText('Test Hub')).not.toBeInTheDocument()
  })

  it('shows spinner and hub name when connecting', () => {
    renderOverlay({ connectionState: 'connecting', connectionDetail: 'Connecting to hub...' })
    expect(screen.getByText('Test Hub')).toBeInTheDocument()
    expect(screen.getByText('Connecting to hub...')).toBeInTheDocument()
  })

  it('shows error state with retry button', () => {
    renderOverlay({
      connectionState: 'error',
      connectionDetail: 'Session expired — re-pair to reconnect',
    })
    expect(screen.getByText('Test Hub')).toBeInTheDocument()
    expect(screen.getByText('Session expired — re-pair to reconnect')).toBeInTheDocument()
    expect(screen.getByText('Retry connection')).toBeInTheDocument()
  })

  it('shows disconnected state with retry button', () => {
    renderOverlay({
      connectionState: 'disconnected',
      connectionDetail: 'Hub is offline',
    })
    expect(screen.getByText('Test Hub')).toBeInTheDocument()
    expect(screen.getByText('Hub is offline')).toBeInTheDocument()
    expect(screen.getByText('Retry connection')).toBeInTheDocument()
  })

  it('shows pairing needed state with start pairing button', () => {
    renderOverlay({
      connectionState: 'pairing_needed',
      connectionDetail: 'Scan the QR code to pair this device',
    })
    expect(screen.getByText('Test Hub')).toBeInTheDocument()
    expect(screen.getByText('Start pairing')).toBeInTheDocument()
  })

  it('fades out when connection state changes to connected', async () => {
    const { container } = renderOverlay({ connectionState: 'connecting' })

    expect(screen.getByText('Test Hub')).toBeInTheDocument()

    act(() => {
      useHubStore.setState({ connectionState: 'connected' })
    })

    // Should have the fade-out class
    const overlay = container.querySelector('.opacity-0')
    expect(overlay).toBeInTheDocument()

    // After transition, should be removed from DOM
    act(() => {
      vi.advanceTimersByTime(350)
    })

    expect(screen.queryByText('Test Hub')).not.toBeInTheDocument()
  })

  it('does not render when already connected', () => {
    renderOverlay({ connectionState: 'connected' })
    expect(screen.queryByText('Test Hub')).not.toBeInTheDocument()
  })
})
