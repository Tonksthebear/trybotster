import { HubManager } from 'connections'
import { useUiPresentationStore } from '../store/ui-presentation-store'
import { useRouteRegistryStore } from '../store/route-registry-store'

// Per-hub shared state
const hubState = new Map()  // hubId → { hub, unsubscribers, callerIds: Set }
const chains = new Map()    // hubId → Promise (serializes connect/disconnect per hub)

// Caller identity
let nextCallerId = 0
const callerHub = new Map() // callerId → hubId

/**
 * Connect to a hub. Returns { hub, connectionId }.
 * Call disconnect(connectionId) when done.
 *
 * Wire protocol: entity stores (`store/entities/`) update themselves
 * straight from `hub_connection.handleMessage` via `applyEntityFrame`.
 * This bridge no longer normalises agent/workspace lists into a unified
 * Zustand store; it only owns the per-hub connection lifecycle and the
 * route-registry seed/follow loop.
 */
export function connect(hubId, _options = {}) {
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

  let state = hubState.get(hubId)

  if (state) {
    state.callerIds.add(callerId)
    syncSelectionFromUrl()
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

  // Wire protocol — seed + follow the hub-authored route registry. The
  // hub sends `ui_route_registry` on hub-channel subscribe and on every
  // `surfaces_changed` hook firing.
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

  syncSelectionFromUrl()

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
  useUiPresentationStore.getState().setSelectedSessionId(null)
  useRouteRegistryStore.getState().clearRoutes(hubId)
}

export function getHub(hubId) {
  return hubState.get(hubId)?.hub || null
}

/**
 * Sync the per-browser selectedSessionId from the URL. Wire protocol
 * keeps selection client-side: a `/hubs/<id>/sessions/<uuid>` URL hydrates
 * the presentation store; the hub never sees per-client selection.
 */
export function syncSelectionFromUrl(_hub) {
  const match = window.location.pathname.match(
    /\/hubs\/[^/]+\/sessions\/([^/]+)/
  )
  // When the URL doesn't name a session, clear selection; otherwise set it
  // from the URL. The selection is applied eagerly even if the session isn't
  // in the entity store yet — the SessionList picks it up once the next
  // entity_snapshot arrives and the byId[uuid] lookup succeeds.
  const sessionUuid = match ? match[1] : null
  useUiPresentationStore.getState().setSelectedSessionId(sessionUuid)
}

function resolveHubManager() {
  return HubManager
}
