import { useSyncExternalStore } from 'react'
import { useUiPresentationStore } from '../store/ui-presentation-store'

export type UiActionLifecycleResult = {
  action_request_id: string
  action_id: string
  target_surface?: string
  ok: boolean
  handled?: boolean
  via?: string
  message?: string
  error?: string
  navigate?: {
    label?: string
    path?: string
  }
  presentation?: unknown
}

export type UiActionLifecycleSnapshot = {
  pending: boolean
  result: UiActionLifecycleResult | null
}

type PendingEntry = {
  requestId: string
  actionId: string
  targetSurface: string
  sourceKey: string
  timer: ReturnType<typeof setTimeout>
}

const TIMEOUT_MS = 15_000
const pendingByRequest = new Map<string, PendingEntry>()
const pendingRequestBySource = new Map<string, string>()
const resultBySource = new Map<string, UiActionLifecycleResult>()
const sourceByRequest = new Map<string, string>()
const snapshotCache = new Map<string, UiActionLifecycleSnapshot>()
const listeners = new Set<() => void>()
const EMPTY_SNAPSHOT: UiActionLifecycleSnapshot = { pending: false, result: null }

function emitChange() {
  for (const listener of listeners) listener()
}

function sourceIdentity(actionId: string, targetSurface: string, sourceKey?: string | null) {
  return `${targetSurface}\u0000${sourceKey || actionId}`
}

function randomHex() {
  return Math.floor(Math.random() * 0xffffffff).toString(16).padStart(8, '0')
}

function generateRequestId() {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return `ua_${crypto.randomUUID()}`
  }
  return `ua_${Date.now().toString(36)}_${randomHex()}`
}

function settle(requestId: string, result: UiActionLifecycleResult) {
  const entry = pendingByRequest.get(requestId)
  const source = entry
    ? sourceIdentity(entry.actionId, entry.targetSurface, entry.sourceKey)
    : sourceByRequest.get(requestId)
  if (!source) return
  if (entry) {
    clearTimeout(entry.timer)
    pendingByRequest.delete(requestId)
    pendingRequestBySource.delete(source)
  }
  resultBySource.set(source, result)
  sourceByRequest.delete(requestId)
  emitChange()
}

export function beginUiActionLifecycle(args: {
  actionId: string
  targetSurface: string
  sourceKey?: string | null
}) {
  const requestId = generateRequestId()
  const source = sourceIdentity(args.actionId, args.targetSurface, args.sourceKey)
  const previousRequest = pendingRequestBySource.get(source)
  if (previousRequest) {
    const previous = pendingByRequest.get(previousRequest)
    if (previous) clearTimeout(previous.timer)
    pendingByRequest.delete(previousRequest)
  }
  resultBySource.delete(source)
  sourceByRequest.set(requestId, source)
  const timer = setTimeout(() => {
    settle(requestId, {
      action_request_id: requestId,
      action_id: args.actionId,
      target_surface: args.targetSurface,
      ok: false,
      handled: false,
      via: 'timeout',
      error: 'Action timed out.',
    })
  }, TIMEOUT_MS)
  pendingByRequest.set(requestId, {
    requestId,
    actionId: args.actionId,
    targetSurface: args.targetSurface,
    sourceKey: args.sourceKey || args.actionId,
    timer,
  })
  pendingRequestBySource.set(source, requestId)
  emitChange()
  return requestId
}

export function failUiActionLifecycle(requestId: string, message: string) {
  const entry = pendingByRequest.get(requestId)
  settle(requestId, {
    action_request_id: requestId,
    action_id: entry?.actionId ?? '',
    target_surface: entry?.targetSurface,
    ok: false,
    handled: false,
    via: 'transport',
    error: message,
  })
}

export function receiveUiActionResult(
  message: unknown,
  scope?: { hubId?: string },
) {
  if (!message || typeof message !== 'object') return
  const frame = message as Record<string, unknown>
  const requestId = frame.action_request_id
  const actionId = frame.action_id
  if (typeof requestId !== 'string' || typeof actionId !== 'string') return
  const result = {
    action_request_id: requestId,
    action_id: actionId,
    target_surface: typeof frame.target_surface === 'string' ? frame.target_surface : undefined,
    ok: frame.ok === true,
    handled: typeof frame.handled === 'boolean' ? frame.handled : undefined,
    via: typeof frame.via === 'string' ? frame.via : undefined,
    message: typeof frame.message === 'string' ? frame.message : undefined,
    error: typeof frame.error === 'string' ? frame.error : undefined,
    navigate: parseNavigate(frame.navigate),
    presentation: frame.presentation,
  }
  if (result.ok) {
    applyPresentationResult(result.presentation, {
      hubId: scope?.hubId,
      targetSurface: result.target_surface,
    })
  }
  settle(requestId, result)
}

function parseNavigate(value: unknown) {
  if (!value || typeof value !== 'object') return undefined
  const nav = value as Record<string, unknown>
  if (typeof nav.path !== 'string' || nav.path.length === 0) return undefined
  return {
    path: nav.path,
    label: typeof nav.label === 'string' ? nav.label : undefined,
  }
}

function applyPresentationResult(
  value: unknown,
  scope: { hubId?: string; targetSurface?: string },
) {
  if (!value || typeof value !== 'object') return
  const presentation = value as Record<string, unknown>
  const store = useUiPresentationStore.getState()
  const clear = presentation.clear
  const clearKeys = Array.isArray(clear) ? clear : clear ? [clear] : []
  for (const key of clearKeys) {
    if (typeof key === 'string') {
      store.clearLocalValue(scope.hubId, scope.targetSurface, key)
    }
  }

  const setValue = presentation.set
  const setEntries = Array.isArray(setValue) ? setValue : setValue ? [setValue] : []
  for (const entry of setEntries) {
    if (!entry || typeof entry !== 'object') continue
    const item = entry as Record<string, unknown>
    if (typeof item.key === 'string') {
      store.setLocalValue(scope.hubId, scope.targetSurface, item.key, item.value)
    }
  }
}

export function getUiActionLifecycleSnapshot(
  actionId: string,
  targetSurface: string,
  sourceKey?: string | null,
): UiActionLifecycleSnapshot {
  const source = sourceIdentity(actionId, targetSurface, sourceKey)
  const requestId = pendingRequestBySource.get(source)
  const pending = requestId ? pendingByRequest.has(requestId) : false
  const result = resultBySource.get(source) ?? null
  const previous = snapshotCache.get(source)
  if (previous && previous.pending === pending && previous.result === result) {
    return previous
  }
  const snapshot = { pending, result }
  snapshotCache.set(source, snapshot)
  return snapshot
}

export function subscribeUiActionLifecycle(listener: () => void) {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

export function useUiActionLifecycle(
  actionId: string,
  targetSurface: string,
  sourceKey?: string | null,
) {
  return useSyncExternalStore(
    subscribeUiActionLifecycle,
    () => getUiActionLifecycleSnapshot(actionId, targetSurface, sourceKey),
    () => EMPTY_SNAPSHOT,
  )
}

export function _resetUiActionLifecycleForTests() {
  for (const entry of pendingByRequest.values()) clearTimeout(entry.timer)
  pendingByRequest.clear()
  pendingRequestBySource.clear()
  resultBySource.clear()
  sourceByRequest.clear()
  snapshotCache.clear()
  emitChange()
}
