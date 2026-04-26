import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { render, screen, cleanup } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import SidebarConnectionStatus from '../components/hub/SidebarConnectionStatus'
import { useHubStore } from '../store/hub-store'
import { useHubMetaStore } from '../store/entities/hub-meta-store'

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
    useHubMetaStore.getState()._reset()
  })

  afterEach(() => {
    cleanup()
    useHubMetaStore.getState()._reset()
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

  it('shows hub-status="online" when the local hub entity is ready and no transport health event has fired', () => {
    // Fresh / unpaired hub: health events are gated on Rails ActionCable
    // pairing, so cliStatus stays UNKNOWN and the transport hub badge is null.
    // The local `hub` entity reaches `recovery_state.state === "ready"` once
    // the hub finishes startup; the sidebar treats that as "online".
    useHubMetaStore.setState({
      byId: { 'test-hub': { hub_id: 'test-hub', state: 'ready' } },
      order: ['test-hub'],
      snapshotSeq: 1,
    })

    renderStatus()

    const toggle = screen.getByTestId('sidebar-connection-status')
    expect(toggle.dataset.hubStatus).toBe('online')
  })

  it('keeps hub-status="connecting" when the local hub entity is still starting', () => {
    useHubMetaStore.setState({
      byId: { 'test-hub': { hub_id: 'test-hub', state: 'starting' } },
      order: ['test-hub'],
      snapshotSeq: 1,
    })

    renderStatus()

    const toggle = screen.getByTestId('sidebar-connection-status')
    // null hubStatus → component renders 'connecting' on the data attr.
    expect(toggle.dataset.hubStatus).toBe('connecting')
  })
})
