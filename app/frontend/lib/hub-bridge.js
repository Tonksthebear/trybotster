import { HubManager } from 'connections'
import { HubGateError, HUB_GATE_ERROR_CODES } from 'connections/hub_gate_error'
import { useUiPresentationStore } from '../store/ui-presentation-store'
import { useRouteRegistryStore } from '../store/route-registry-store'

// Per-hub shared state
const hubState = new Map()  // hubId → { hub, unsubscribers, callerIds: Set }
const chains = new Map()    // hubId → Promise (serializes connect/disconnect per hub)
const hubWaiters = new Map() // hubId → Set<callback>
const hubGateWaiters = new Map() // hubId → Set<{ resolve, reject, cleanup }>
const hubOperationWaiters = new Map() // hubId → Set<{ reject, cleanup }>

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
    rejectHubGateWaiters(
      hubId,
      new HubGateError(
        HUB_GATE_ERROR_CODES.UNAVAILABLE,
        `Hub ${hubId} is unavailable`,
        { cause: err },
      ),
    )
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
  notifyHubAvailable(hubId, hub)
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
  rejectHubGateWaiters(
    hubId,
    new HubGateError(
      HUB_GATE_ERROR_CODES.UNAVAILABLE,
      `Hub ${hubId} disconnected before the operation could run`,
    ),
  )
  rejectHubOperationWaiters(
    hubId,
    new HubGateError(
      HUB_GATE_ERROR_CODES.UNAVAILABLE,
      `Hub ${hubId} disconnected before the operation could finish`,
    ),
  )
  useUiPresentationStore.getState().setSelectedSessionId(null)
  useRouteRegistryStore.getState().clearRoutes(hubId)
}

function currentHub(hubId) {
  return hubState.get(String(hubId))?.hub || null
}

export function waitForHub(hubId, timeoutMs = 10000) {
  if (!hubId) return Promise.resolve(null)
  const existing = currentHub(hubId)
  if (existing) return Promise.resolve(existing)

  const key = String(hubId)
  return new Promise((resolve) => {
    let settled = false
    let timer = null

    const finish = (hub) => {
      if (settled) return
      settled = true
      if (timer) window.clearTimeout(timer)
      const set = hubWaiters.get(key)
      set?.delete(finish)
      if (set && set.size === 0) hubWaiters.delete(key)
      resolve(hub || null)
    }

    if (!hubWaiters.has(key)) hubWaiters.set(key, new Set())
    hubWaiters.get(key).add(finish)

    if (timeoutMs != null) {
      timer = window.setTimeout(() => finish(currentHub(key)), timeoutMs)
    }
  })
}

/**
 * Run an operation against the route-owned HubSession.
 *
 * The default readiness gate requires a connected transport. Pass
 * `requireTransport: false` only for rare object-only callers that can safely
 * inspect the HubSession without sending hub commands.
 */
export async function withHub(hubId, operation, options = {}) {
  if (!hubId) {
    throw new HubGateError(
      HUB_GATE_ERROR_CODES.UNAVAILABLE,
      "Hub id is required",
    )
  }
  if (typeof operation !== 'function') {
    throw new TypeError('withHub requires an operation function')
  }

  const existing = currentHub(hubId)
  if (existing) return performWithBridgeGate(hubId, existing, operation, options)

  const hub = await waitForHubGate(hubId, options)
  return performWithBridgeGate(hubId, hub, operation, options)
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

function notifyHubAvailable(hubId, hub) {
  const key = String(hubId)
  const waiters = hubWaiters.get(key)
  if (waiters) {
    hubWaiters.delete(key)
    for (const callback of waiters) callback(hub)
  }

  const gateWaiters = hubGateWaiters.get(key)
  if (!gateWaiters) return
  hubGateWaiters.delete(key)
  for (const waiter of gateWaiters) {
    waiter.cleanup()
    waiter.resolve(hub)
  }
}

function waitForHubGate(hubId, { timeoutMs = null, signal = null } = {}) {
  const key = String(hubId)
  const existing = currentHub(key)
  if (existing) return Promise.resolve(existing)

  if (signal?.aborted) {
    return Promise.reject(new HubGateError(
      HUB_GATE_ERROR_CODES.ABORTED,
      "Hub operation was aborted",
    ))
  }

  return new Promise((resolve, reject) => {
    let timer = null
    let waiter = null

    const cleanup = () => {
      if (timer) {
        window.clearTimeout(timer)
        timer = null
      }
      signal?.removeEventListener?.('abort', onAbort)
      const set = hubGateWaiters.get(key)
      set?.delete(waiter)
      if (set && set.size === 0) hubGateWaiters.delete(key)
    }

    const finish = (callback, value) => {
      cleanup()
      callback(value)
    }

    const onAbort = () => finish(reject, new HubGateError(
      HUB_GATE_ERROR_CODES.ABORTED,
      "Hub operation was aborted",
    ))

    waiter = { resolve, reject, cleanup }

    if (!hubGateWaiters.has(key)) hubGateWaiters.set(key, new Set())
    hubGateWaiters.get(key).add(waiter)
    signal?.addEventListener?.('abort', onAbort, { once: true })

    if (timeoutMs != null) {
      timer = window.setTimeout(() => {
        finish(reject, new HubGateError(
          HUB_GATE_ERROR_CODES.TIMEOUT,
          `Timed out waiting for hub ${key}`,
        ))
      }, timeoutMs)
    }
  })
}

function rejectHubGateWaiters(hubId, error) {
  const key = String(hubId)
  const waiters = hubGateWaiters.get(key)
  if (!waiters) return
  hubGateWaiters.delete(key)
  for (const waiter of waiters) {
    waiter.cleanup()
    waiter.reject(error)
  }
}

function performWithBridgeGate(hubId, hub, operation, options) {
  const requireTransport = options.requireTransport ?? true
  if (!requireTransport || hub.isConnected?.()) {
    return hub.perform(operation, options)
  }

  const key = String(hubId)
  const controller = new AbortController()
  const onUserAbort = () => controller.abort()
  if (options.signal?.aborted) {
    controller.abort()
  } else {
    options.signal?.addEventListener?.('abort', onUserAbort, { once: true })
  }
  const gatedOptions = { ...options, signal: controller.signal }
  let waiter = null
  let settled = false

  const cleanup = () => {
    options.signal?.removeEventListener?.('abort', onUserAbort)
    const set = hubOperationWaiters.get(key)
    set?.delete(waiter)
    if (set && set.size === 0) hubOperationWaiters.delete(key)
  }

  const bridgeDisconnect = new Promise((_, reject) => {
    waiter = {
      cleanup,
      reject: (error) => {
        if (settled) return
        settled = true
        cleanup()
        reject(error)
        controller.abort()
      },
    }
  })

  if (!hubOperationWaiters.has(key)) hubOperationWaiters.set(key, new Set())
  hubOperationWaiters.get(key).add(waiter)

  const operationPromise = Promise.resolve().then(() => (
    hub.perform(operation, gatedOptions)
  ))
  operationPromise.then(() => {
    if (settled) return
    settled = true
    cleanup()
  }, () => {
    if (settled) return
    settled = true
    cleanup()
  })

  return Promise.race([operationPromise, bridgeDisconnect])
}

function rejectHubOperationWaiters(hubId, error) {
  const key = String(hubId)
  const waiters = hubOperationWaiters.get(key)
  if (!waiters) return
  hubOperationWaiters.delete(key)
  for (const waiter of waiters) {
    waiter.cleanup()
    waiter.reject(error)
  }
}
