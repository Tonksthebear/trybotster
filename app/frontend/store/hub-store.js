import { create } from 'zustand'
import { connect, disconnect, getHub } from '../lib/hub-bridge'
import { getActionCableConsumer } from '../lib/transport/hub_signaling_client'

const LAST_HUB_KEY = 'botster:lastHubId'
let hubListSubscription = null
let hubListSubscriptionPromise = null
let hubListSubscriptionCount = 0

export const useHubStore = create((set, get) => ({
  hubList: [],
  hubListLoading: true,
  selectedHubId: null,
  connectionState: 'disconnected',
  connectionDetail: '',

  // Internal — not part of the public API
  _connectionRef: null,
  _statusUnsub: null,

  fetchHubList: async () => {
    set({ hubListLoading: true })
    try {
      const res = await fetch('/hubs.json', {
        headers: { Accept: 'application/json' },
        credentials: 'same-origin',
      })
      if (res.status === 401 || res.redirected) {
        window.location.href = '/github/authorization/new'
        return []
      }
      if (!res.ok) throw new Error(`${res.status}`)
      const data = await res.json()
      const hubs = Array.isArray(data) ? data : data.hubs || []
      set({ hubList: hubs, hubListLoading: false })
      return hubs
    } catch {
      set({ hubListLoading: false })
      return []
    }
  },

  selectHub: async (hubId) => {
    if (hubId != null) hubId = String(hubId)
    const { selectedHubId, _connectionRef, _statusUnsub } = get()
    if (hubId === selectedHubId) return

    // Tear down previous connection
    if (_statusUnsub) _statusUnsub()
    if (_connectionRef != null) disconnect(_connectionRef)
    set({ _statusUnsub: null, _connectionRef: null })

    if (!hubId) {
      set({
        selectedHubId: null,
        connectionState: 'disconnected',
        connectionDetail: '',
      })
      localStorage.removeItem(LAST_HUB_KEY)
      return
    }

    set({
      selectedHubId: hubId,
      connectionState: 'connecting',
      connectionDetail: 'Connecting to hub...',
    })
    localStorage.setItem(LAST_HUB_KEY, hubId)

    try {
      const { connectionId } = await connect(hubId, { surface: 'panel' })

      // Hub may have changed while awaiting
      if (get().selectedHubId !== hubId) {
        disconnect(connectionId)
        return
      }

      set({ _connectionRef: connectionId })

      const hub = getHub(hubId)
      if (hub) {
        // Read initial status
        const initial = hub.connectionStatus?.current()
        if (initial) applyConnectionStatus(set, get, hubId, initial)

        // Subscribe to ongoing changes
        const unsub = hub.onConnectionStatusChange?.((status) => {
          applyConnectionStatus(set, get, hubId, status)
        })
        if (unsub) set({ _statusUnsub: unsub })
      }
    } catch (err) {
      if (get().selectedHubId === hubId) {
        set({
          connectionState: 'error',
          connectionDetail: err.message || 'Failed to connect',
        })
      }
    }
  },

  // Tear down the active connection without clearing localStorage.
  // Used by HubShell on unmount so the last-used hub is preserved.
  disconnectHub: () => {
    const { _connectionRef, _statusUnsub } = get()
    if (_statusUnsub) _statusUnsub()
    if (_connectionRef != null) disconnect(_connectionRef)
    set({
      selectedHubId: null,
      connectionState: 'disconnected',
      connectionDetail: '',
      _connectionRef: null,
      _statusUnsub: null,
    })
  },

  retryConnection: () => {
    const { selectedHubId } = get()
    if (!selectedHubId) return

    // Force full reconnect by clearing and re-selecting
    const hubId = selectedHubId
    const { _connectionRef, _statusUnsub } = get()
    if (_statusUnsub) _statusUnsub()
    if (_connectionRef != null) disconnect(_connectionRef)

    set({
      selectedHubId: null,
      connectionState: 'disconnected',
      connectionDetail: '',
      _connectionRef: null,
      _statusUnsub: null,
    })

    // Re-select on next tick so zustand sees the null first
    queueMicrotask(() => get().selectHub(hubId))
  },

  getLastHubId: () => localStorage.getItem(LAST_HUB_KEY),
}))

export async function subscribeHubListUpdates() {
  hubListSubscriptionCount += 1

  if (!hubListSubscriptionPromise) {
    hubListSubscriptionPromise = getActionCableConsumer().then((consumer) => {
      hubListSubscription = consumer.subscriptions.create(
        { channel: 'HubListChannel' },
        {
          received: (data) => {
            if (data?.type === 'refresh') void useHubStore.getState().fetchHubList()
          },
        }
      )

      return hubListSubscription
    })
  }

  await hubListSubscriptionPromise

  let released = false
  return () => {
    if (released) return
    released = true

    hubListSubscriptionCount = Math.max(0, hubListSubscriptionCount - 1)
    if (hubListSubscriptionCount === 0 && hubListSubscription) {
      hubListSubscription.unsubscribe()
      hubListSubscription = null
      hubListSubscriptionPromise = null
    }
  }
}

export function resetHubListSubscriptionForTest() {
  hubListSubscriptionCount = 0
  hubListSubscription?.unsubscribe()
  hubListSubscription = null
  hubListSubscriptionPromise = null
}

function applyConnectionStatus(set, get, hubId, status) {
  if (get().selectedHubId !== hubId) return

  const { connection, hub: hubStatus } = status
  let connectionState = 'connecting'
  let connectionDetail = 'Connecting...'

  if (connection === 'direct' || connection === 'relay') {
    connectionState = 'connected'
    connectionDetail = connection === 'direct'
      ? 'Connected directly'
      : 'Connected via relay'
  } else if (connection === 'unpaired') {
    connectionState = 'pairing_needed'
    connectionDetail = 'Scan the QR code to pair this device'
  } else if (connection === 'expired') {
    connectionState = 'error'
    connectionDetail = 'Session expired — re-pair to reconnect'
  } else if (connection === 'disconnected') {
    if (hubStatus === 'offline') {
      connectionState = 'disconnected'
      connectionDetail = 'Hub is offline'
    } else if (hubStatus === 'online') {
      connectionState = 'connecting'
      connectionDetail = 'Establishing secure connection...'
    } else {
      connectionState = 'connecting'
      connectionDetail = 'Waiting for hub...'
    }
  } else if (connection === 'connecting') {
    connectionState = 'connecting'
    connectionDetail = hubStatus === 'online'
      ? 'Establishing secure connection...'
      : 'Connecting to hub...'
  }

  set({ connectionState, connectionDetail })
}
