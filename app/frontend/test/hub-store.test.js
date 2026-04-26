import { waitFor } from '@testing-library/react'
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { resetHubListSubscriptionForTest, subscribeHubListUpdates, useHubStore } from '../store/hub-store'

// Mock hub-bridge
const mockConnect = vi.fn(() => Promise.resolve({ connectionId: 42 }))
const mockDisconnect = vi.fn()
const mockGetHub = vi.fn(() => null)

vi.mock('../lib/hub-bridge', () => ({
  connect: (...args) => mockConnect(...args),
  disconnect: (...args) => mockDisconnect(...args),
}))

const mockSubscriptionCreate = vi.fn()

vi.mock('../lib/transport/hub_signaling_client', () => ({
  getActionCableConsumer: vi.fn(async () => ({
    subscriptions: {
      create: (...args) => mockSubscriptionCreate(...args),
    },
  })),
}))

// Mock fetch for hub list
const mockHubs = [
  { id: 1, name: 'Hub Alpha', identifier: 'alpha', active: true },
  { id: 2, name: 'Hub Beta', identifier: 'beta', active: false },
]

describe('hub-store', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    localStorage.clear()
    resetHubListSubscriptionForTest()
    useHubStore.setState({
      hubList: [],
      hubListLoading: true,
      selectedHubId: null,
      connectionState: 'disconnected',
      connectionDetail: '',
      _connectionRef: null,
      _statusUnsub: null,
    })
  })

  afterEach(() => {
    resetHubListSubscriptionForTest()
  })

  describe('fetchHubList', () => {
    it('fetches and stores hub list', async () => {
      globalThis.fetch = vi.fn(() =>
        Promise.resolve({
          ok: true,
          status: 200,
          redirected: false,
          json: () => Promise.resolve(mockHubs),
        })
      )

      const hubs = await useHubStore.getState().fetchHubList()

      expect(hubs).toEqual(mockHubs)
      expect(useHubStore.getState().hubList).toEqual(mockHubs)
      expect(useHubStore.getState().hubListLoading).toBe(false)
    })

    it('handles fetch errors gracefully', async () => {
      globalThis.fetch = vi.fn(() => Promise.reject(new Error('Network error')))

      const hubs = await useHubStore.getState().fetchHubList()

      expect(hubs).toEqual([])
      expect(useHubStore.getState().hubListLoading).toBe(false)
    })
  })

  describe('selectHub', () => {
    it('transitions to connecting state', async () => {
      await useHubStore.getState().selectHub(1)

      expect(useHubStore.getState().selectedHubId).toBe('1')
      expect(mockConnect).toHaveBeenCalledWith('1', { surface: 'panel' })
    })

    it('stores last hub ID in localStorage', async () => {
      await useHubStore.getState().selectHub(1)

      expect(localStorage.getItem('botster:lastHubId')).toBe('1')
    })

    it('disconnects previous hub when switching', async () => {
      await useHubStore.getState().selectHub(1)
      const firstRef = useHubStore.getState()._connectionRef

      mockConnect.mockResolvedValueOnce({ connectionId: 99 })
      await useHubStore.getState().selectHub(2)

      expect(mockDisconnect).toHaveBeenCalledWith(firstRef)
      expect(useHubStore.getState().selectedHubId).toBe('2')
    })

    it('clears state when selecting null', async () => {
      await useHubStore.getState().selectHub(1)
      await useHubStore.getState().selectHub(null)

      expect(useHubStore.getState().selectedHubId).toBe(null)
      expect(useHubStore.getState().connectionState).toBe('disconnected')
      expect(localStorage.getItem('botster:lastHubId')).toBe(null)
    })

    it('does not re-connect if same hub is selected', async () => {
      await useHubStore.getState().selectHub(1)
      mockConnect.mockClear()

      await useHubStore.getState().selectHub(1)

      expect(mockConnect).not.toHaveBeenCalled()
    })

    it('sets error state when connect throws', async () => {
      mockConnect.mockRejectedValueOnce(new Error('Connection refused'))

      await useHubStore.getState().selectHub(1)

      expect(useHubStore.getState().connectionState).toBe('error')
      expect(useHubStore.getState().connectionDetail).toBe('Connection refused')
    })
  })

  describe('subscribeHubListUpdates', () => {
    it('refreshes the shared hub list when the cable channel broadcasts', async () => {
      let received
      const unsubscribe = vi.fn()
      mockSubscriptionCreate.mockImplementation((_identifier, callbacks) => {
        received = callbacks.received
        return { unsubscribe }
      })

      globalThis.fetch = vi.fn(() =>
        Promise.resolve({
          ok: true,
          status: 200,
          redirected: false,
          json: () => Promise.resolve(mockHubs),
        })
      )

      const cleanup = await subscribeHubListUpdates()
      await received({ type: 'refresh' })

      await waitFor(() => {
        expect(useHubStore.getState().hubList).toEqual(mockHubs)
      })

      cleanup()
      expect(unsubscribe).toHaveBeenCalledTimes(1)
    })
  })

  describe('disconnectHub', () => {
    it('tears down connection without clearing localStorage', async () => {
      await useHubStore.getState().selectHub(1)
      expect(localStorage.getItem('botster:lastHubId')).toBe('1')

      useHubStore.getState().disconnectHub()

      expect(useHubStore.getState().selectedHubId).toBe(null)
      expect(useHubStore.getState().connectionState).toBe('disconnected')
      // localStorage should still have the last hub ID
      expect(localStorage.getItem('botster:lastHubId')).toBe('1')
    })
  })

  describe('getLastHubId', () => {
    it('returns stored hub ID from localStorage', () => {
      localStorage.setItem('botster:lastHubId', '42')
      expect(useHubStore.getState().getLastHubId()).toBe('42')
    })

    it('returns null when no hub stored', () => {
      expect(useHubStore.getState().getLastHubId()).toBe(null)
    })
  })
})
