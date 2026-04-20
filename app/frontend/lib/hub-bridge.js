import { HubManager } from 'connections'
import { useWorkspaceStore } from '../store/workspace-store'
import { useRouteRegistryStore } from '../store/route-registry-store'

// Per-hub shared state
const hubState = new Map()  // hubId → { hub, unsubscribers, callerIds: Set }
const chains = new Map()    // hubId → Promise (serializes connect/disconnect per hub)

// Caller identity
let nextCallerId = 0
const callerHub = new Map() // callerId → hubId

function getStoreActions() {
  return useWorkspaceStore.getState()
}

/**
 * Connect to a hub. Returns { hub, connectionId }.
 * Call disconnect(connectionId) when done.
 */
export function connect(hubId, { surface = 'panel' } = {}) {
  getStoreActions().setSurface(surface)

  const callerId = nextCallerId++
  callerHub.set(callerId, hubId)

  const prev = chains.get(hubId) || Promise.resolve()
  const next = prev.then(() => doConnect(hubId, callerId))
  chains.set(hubId, next.catch(() => {}))

  return next
}

async function doConnect(hubId, callerId) {
  // Caller may have been disconnected while queued
  if (!callerHub.has(callerId)) return { hub: null, connectionId: callerId }

  const { normalize } = getStoreActions()
  let state = hubState.get(hubId)

  if (state) {
    // Hub already acquired — just add this caller
    state.callerIds.add(callerId)
    normalize(state.hub.agents.current(), state.hub.openWorkspaces.current())
    syncSelectionFromUrl(state.hub)
    return { hub: state.hub, connectionId: callerId }
  }

  // First caller — acquire hub and subscribe
  const HubManager = resolveHubManager()
  let hub
  try {
    hub = await HubManager.acquire(hubId)
  } catch (err) {
    callerHub.delete(callerId)
    throw err
  }

  // Re-check after await — caller may have disconnected during acquire
  if (!callerHub.has(callerId)) {
    hub.release()
    return { hub, connectionId: callerId }
  }

  const unsubscribers = []

  normalize(hub.agents.current(), hub.openWorkspaces.current())

  hub.agents.load().catch(() => {})
  hub.openWorkspaces.load().catch(() => {})

  unsubscribers.push(
    hub.agents.onChange((agents) => {
      const workspaces = hub.openWorkspaces.current()
      getStoreActions().normalize(agents, workspaces)
      syncSelectionFromUrl(hub)
    })
  )

  unsubscribers.push(
    hub.openWorkspaces.onChange((workspaces) => {
      const agents = hub.agents.current()
      getStoreActions().normalize(agents, workspaces)
    })
  )

  // Phase 4a: seed + follow the hub-authored route registry. The hub sends
  // `ui_route_registry_v1` on hub-channel subscribe (so the first frame
  // arrives shortly after acquire) and on every `surfaces_changed` hook
  // firing. `hub.transport` is the HubTransport; `uiRouteRegistry` is both
  // an event name and a snapshot accessor.
  const seedRoutes = () => {
    const transport = hub.transport
    if (transport && typeof transport.uiRouteRegistry === 'function') {
      const initial = transport.uiRouteRegistry()
      if (Array.isArray(initial) && initial.length > 0) {
        useRouteRegistryStore.getState().setRoutes(hubId, initial)
      }
    }
  }
  seedRoutes()
  if (hub.transport && typeof hub.transport.on === 'function') {
    const off = hub.transport.on('uiRouteRegistry', (routes) => {
      useRouteRegistryStore.getState().setRoutes(hubId, routes)
    })
    if (typeof off === 'function') {
      unsubscribers.push(off)
    }
  }

  syncSelectionFromUrl(hub)

  state = { hub, unsubscribers, callerIds: new Set([callerId]) }
  hubState.set(hubId, state)
  return { hub, connectionId: callerId }
}

/**
 * Disconnect a specific caller. Pass the connectionId from connect().
 */
export function disconnect(connectionId) {
  const hubId = callerHub.get(connectionId)
  if (hubId == null) return

  callerHub.delete(connectionId)

  const prev = chains.get(hubId) || Promise.resolve()
  const next = prev.then(() => doDisconnect(hubId, connectionId))
  chains.set(hubId, next.catch(() => {}))

  return next
}

function doDisconnect(hubId, callerId) {
  const state = hubState.get(hubId)
  if (!state) return

  state.callerIds.delete(callerId)
  if (state.callerIds.size > 0) return

  // Last caller — tear down
  state.unsubscribers.forEach((unsub) => unsub())
  state.hub.release()
  hubState.delete(hubId)
  chains.delete(hubId)
  getStoreActions().setConnected(false)
  useRouteRegistryStore.getState().clearRoutes(hubId)
}

export function getHub(hubId) {
  return hubState.get(hubId)?.hub || null
}

export function syncSelectionFromUrl(hub) {
  const match = window.location.pathname.match(
    /\/hubs\/[^/]+\/sessions\/([^/]+)/
  )
  if (!match) {
    useWorkspaceStore.getState().setSelectedSessionId(null)
    return
  }

  const sessionUuid = match[1]
  const agents = hub?.agents.current() || []
  const agent = agents.find((a) => a.session_uuid === sessionUuid)
  if (agent) {
    useWorkspaceStore.getState().setSelectedSessionId(agent.id)
  } else if (agents.length > 0) {
    useWorkspaceStore.getState().setSelectedSessionId(null)
  }
}

function resolveHubManager() {
  return HubManager
}
