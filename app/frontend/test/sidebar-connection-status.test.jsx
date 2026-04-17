import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { render, screen, cleanup } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import SidebarConnectionStatus from '../components/hub/SidebarConnectionStatus'
import { useHubStore } from '../store/hub-store'

// Mock hub-bridge
vi.mock('../lib/hub-bridge', () => ({
  connect: vi.fn(() => Promise.resolve({ connectionId: 1 })),
  disconnect: vi.fn(),
  getHub: vi.fn(() => null),
}))

// Mock the transport module to avoid import errors
vi.mock('transport/hub_signaling_client', () => ({
  observeBrowserSocketState: vi.fn(() => Promise.resolve(() => {})),
}))

function renderStatus(storeOverrides = {}) {
  useHubStore.setState({
    hubList: [{ id: 1, name: 'Test Hub', identifier: 'test-hub', active: true }],
    hubListLoading: false,
    selectedHubId: 1,
    connectionState: 'connecting',
    connectionDetail: 'Connecting...',
    _connectionRef: null,
    _statusUnsub: null,
    ...storeOverrides,
  })

  return render(<SidebarConnectionStatus />)
}

describe('SidebarConnectionStatus', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  afterEach(() => {
    cleanup()
  })

  it('does not render when no hub is selected', () => {
    renderStatus({ selectedHubId: null })
    expect(screen.queryByText('Connecting...')).not.toBeInTheDocument()
  })

  it('shows three status dots when hub is selected', () => {
    const { container } = renderStatus()
    const dots = container.querySelectorAll('.rounded-full')
    // 3 dots in the compact view
    expect(dots.length).toBeGreaterThanOrEqual(3)
  })

  it('shows summary label', () => {
    renderStatus()
    expect(screen.getByText('Connecting...')).toBeInTheDocument()
  })

  it('expands to show detail rows when clicked', async () => {
    const user = userEvent.setup()
    renderStatus()

    // Click to expand
    const toggle = screen.getByRole('button')
    await user.click(toggle)

    // Should show individual status rows
    expect(screen.getByText('Browser')).toBeInTheDocument()
    expect(screen.getByText('Connection')).toBeInTheDocument()
    expect(screen.getByText('Hub')).toBeInTheDocument()
  })

  it('collapses detail when clicked again', async () => {
    const user = userEvent.setup()
    renderStatus()

    const toggle = screen.getByRole('button')
    await user.click(toggle)
    expect(screen.getByText('Browser')).toBeInTheDocument()

    await user.click(toggle)
    expect(screen.queryByText('Browser')).not.toBeInTheDocument()
  })
})
