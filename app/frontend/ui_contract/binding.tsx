// Wire protocol — `$bind` / `bind_list` resolver for the React renderer.
//
// Mirrors `cli/src/tui/ui_contract_adapter/binding.rs`. Both renderers must
// agree on the path grammar so a plugin layout that uses `ui.bind()` /
// `ui.bind_list{}` resolves the same value on either client.
//
// Two flavours are exposed:
//
//   * `resolveBindings(value, stores)` — pure, recursive walker that returns
//     a deep copy with sentinels replaced. Mirrors the TUI's pre-dispatch
//     pass and is the path used at wire-tree-arrival time so the rendered
//     tree never contains sentinels.
//
//   * `<BindResolver path>` + `useBindingValue(path)` — React-flavoured
//     escape hatch for surfaces that want fine-grained Zustand-selector
//     re-rendering on a single bound field, without invalidating the
//     enclosing tree. Currently used by plugin layouts whose authors want
//     finer reactivity than the per-snapshot tree rebuild.

import React, { useMemo, useSyncExternalStore, type ReactNode } from 'react'

import {
  isBindList,
  isBindSentinel,
  isLocalSentinel,
  type UiBindList,
  type UiBind,
  type UiLocal,
  type UiNode,
} from './types'
import { storeFor } from '../store/entities'
import { useUiPresentationStore } from '../store/ui-presentation-store'

const ITEM_RELATIVE_PREFIX = '@'

type EntityRecord = Record<string, unknown>
type EntityState = {
  order: string[]
  byId: Record<string, unknown>
  snapshotSeq?: number
  revision?: number
}
type EntityStoreHook = {
  <T>(selector: (state: EntityState) => T): T
  getState: () => EntityState
}
type ItemContext = EntityRecord | undefined
type LocalScope = {
  hubId?: string
  targetSurface?: string
}

/**
 * Walk `value` (a wire-shape JSON tree) and return a deep copy with every
 * `$bind` sentinel replaced by its resolved value, and every `bind_list`
 * envelope expanded into the per-item array.
 *
 * The walker never errors — missing entity / field / store all resolve to
 * `null`, which the React renderers handle as "field absent".
 */
export function resolveBindings(value: unknown, localScope?: LocalScope): unknown {
  return resolveBindingsInner(value, undefined, localScope)
}

function resolveBindingsInner(
  value: unknown,
  item: ItemContext,
  localScope?: LocalScope,
): unknown {
  if (Array.isArray(value)) {
    const out: unknown[] = []
    for (const v of value) {
      const resolved = resolveBindingsInner(v, item, localScope)
      if (Array.isArray(resolved)) {
        out.push(...resolved)
      } else if (resolved == null) {
        continue
      } else {
        out.push(resolved)
      }
    }
    return out
  }
  if (value === null || typeof value !== 'object') {
    return value
  }
  if (isBindSentinel(value)) {
    return resolvePath(value.$bind, item)
  }
  if (isLocalSentinel(value)) {
    return resolveLocal(value, localScope)
  }
  if (isBindIf(value)) {
    return expandBindIf(value, item, localScope)
  }
  if (isBindList(value)) {
    return expandBindList(value, item, localScope)
  }
  // Only walk plain objects (those produced by JSON.parse / object literals).
  // Class instances (e.g. the `Boom` test fixture in ui-tree.test.jsx that
  // exists to verify the React error boundary) are passed through unchanged
  // so their getter throws still fire during the React render phase rather
  // than being eaten by the walker.
  if (!isPlainObject(value)) return value
  const out: Record<string, unknown> = {}
  for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
    out[k] = resolveBindingsInner(v, item, localScope)
  }
  return out
}

type UiBindIf = {
  $kind: 'bind_if'
  path: string
  node: UiNode
}

function isBindIf(value: unknown): value is UiBindIf {
  if (value === null || typeof value !== 'object') return false
  const v = value as Record<string, unknown>
  return v.$kind === 'bind_if' && typeof v.path === 'string'
}

function expandBindIf(
  envelope: UiBindIf,
  item: ItemContext,
  localScope?: LocalScope,
): unknown {
  const value = resolvePath(envelope.path, item)
  if (value === null || value === false) return null
  return resolveBindingsInner(envelope.node, item, localScope)
}

function isPlainObject(value: unknown): boolean {
  if (value === null || typeof value !== 'object') return false
  const proto = Object.getPrototypeOf(value)
  return proto === Object.prototype || proto === null
}

function expandBindList(
  envelope: UiBindList,
  parentItem: ItemContext,
  localScope?: LocalScope,
): unknown[] {
  const entityType = envelope.source.replace(/^\//, '')
  const store = (storeFor(entityType) as EntityStoreHook).getState()
  const out: unknown[] = []
  for (const id of store.order) {
    const record = store.byId[id] as EntityRecord | undefined
    if (record == null) continue
    if (!matchesWhere(record, envelope.where)) continue
    // Per-item resolution shadows the outer item context.
    const expanded = resolveBindingsInner(envelope.item_template, record, localScope)
    if (expanded != null) out.push(expanded)
  }
  if (out.length === 0 && envelope.empty_template != null) {
    // Empty templates are not item-scoped. Global/local bindings still
    // resolve; @-relative bindings resolve to null.
    const empty = resolveBindingsInner(envelope.empty_template, undefined, localScope)
    if (empty != null) out.push(empty)
  }
  // `parentItem` deliberately not threaded down — bind_list always shadows.
  void parentItem
  return out
}

function resolveLocal(envelope: UiLocal, localScope?: LocalScope): unknown {
  return useUiPresentationStore
    .getState()
    .localValue(
      localScope?.hubId,
      localScope?.targetSurface,
      envelope.$local,
      envelope.default ?? null,
    )
}

function matchesWhere(record: EntityRecord, where: Record<string, unknown> | undefined): boolean {
  if (where == null) return true
  for (const [field, expected] of Object.entries(where)) {
    if (record[field] !== expected) return false
  }
  return true
}

/**
 * Resolve a single binding path against the entity stores.
 *
 * Path grammar (must match the TUI resolver in `cli/src/tui/ui_contract_adapter/binding.rs`):
 *
 *   - `/<type>/<id>/<field>`   → scalar lookup
 *   - `/<type>/<id>`           → whole record
 *   - `/<type>`                → array of records
 *   - `@/<field>`              → item-relative (only inside `bind_list`)
 */
export function resolvePath(path: string, item: ItemContext): unknown {
  if (path.startsWith(ITEM_RELATIVE_PREFIX)) {
    return resolveItemRelative(path, item)
  }
  const parts = path
    .replace(/^\//, '')
    .split('/')
    .filter((s) => s.length > 0)
  switch (parts.length) {
    case 0:
      return null
    case 1:
      return resolveList(parts[0] as string)
    case 2:
      return resolveRecord(parts[0] as string, parts[1] as string)
    case 3:
      return resolveScalar(parts[0] as string, parts[1] as string, parts[2] as string)
    default:
      // Too many segments — return null and log for debugging. Matches the
      // TUI's defensive default.
      // eslint-disable-next-line no-console
      console.debug(`binding: path "${path}" has too many segments`)
      return null
  }
}

function resolveList(entityType: string): EntityRecord[] {
  const store = (storeFor(entityType) as EntityStoreHook).getState()
  return store.order
    .map((id) => store.byId[id] as EntityRecord | undefined)
    .filter((entity): entity is EntityRecord => entity != null)
}

function resolveRecord(entityType: string, id: string): EntityRecord | null {
  const store = (storeFor(entityType) as EntityStoreHook).getState()
  return (store.byId[id] as EntityRecord | undefined) ?? null
}

function resolveScalar(entityType: string, id: string, field: string): unknown {
  const record = resolveRecord(entityType, id)
  if (record == null) return null
  return field in record ? record[field] : null
}

function resolveItemRelative(path: string, item: ItemContext): unknown {
  if (item == null) {
    // eslint-disable-next-line no-console
    console.debug(`binding: @-relative path "${path}" outside bind_list`)
    return null
  }
  const rest = path.replace(ITEM_RELATIVE_PREFIX, '').replace(/^\//, '')
  if (rest === '') return item
  let current: unknown = item
  for (const segment of rest.split('/')) {
    if (segment === '') continue
    if (current === null || typeof current !== 'object') return null
    current = (current as Record<string, unknown>)[segment]
    if (current === undefined) return null
  }
  return current
}

// ---------------------------------------------------------------------------
// React-flavoured wrappers
// ---------------------------------------------------------------------------

/**
 * Subscribe to a `$bind` path via Zustand selector and re-render the
 * supplied `render` fn on every change. Unlike `resolveBindings`, only this
 * wrapper re-renders on patches to the bound entity — the enclosing tree
 * stays stable.
 *
 * Currently unused by the built-in composites (they read from stores
 * directly). Plugin authors hooking `$bind` into existing primitives use
 * this via the auto-wrap inside `interpreter.tsx`.
 */
export type BindResolverProps = {
  path: string
  /** Optional outer item context for `@`-relative paths. */
  item?: EntityRecord
  render: (value: unknown) => ReactNode
}

export function BindResolver({ path, item, render }: BindResolverProps): ReactNode {
  const value = useBindingValue(path, item)
  return <>{render(value)}</>
}

/**
 * Hook flavour of [`BindResolver`]. Returns the resolved value; subscribes to
 * the relevant store via Zustand's selector mechanism so the component only
 * re-renders when the bound field changes.
 */
export function useBindingValue(path: string, item?: EntityRecord): unknown {
  // Item-relative paths don't subscribe to a store — the value comes from the
  // explicit item param.
  if (path.startsWith(ITEM_RELATIVE_PREFIX)) {
    return resolveItemRelative(path, item)
  }
  const parts = path
    .replace(/^\//, '')
    .split('/')
    .filter((s) => s.length > 0)
  const entityType = parts[0] ?? ''
  // We need a stable store reference — we always subscribe to the same store
  // for the lifetime of the component. `storeFor` may register a new plugin
  // store on first call; subsequent calls return the same instance.
  const useStore = storeFor(entityType) as EntityStoreHook
  // The selector returns just the slice we care about, so re-renders only
  // happen when that slice changes.
  return useStore((state) => {
    if (parts.length <= 1) {
      return state.order
        .map((id: string) => state.byId[id] as EntityRecord | undefined)
        .filter((entity: EntityRecord | undefined): entity is EntityRecord => entity != null)
    }
    if (parts.length === 2) {
      const id = parts[1] as string
      return (state.byId[id] as EntityRecord | undefined) ?? null
    }
    if (parts.length === 3) {
      const id = parts[1] as string
      const field = parts[2] as string
      const record = state.byId[id] as EntityRecord | undefined
      if (record == null) return null
      return field in record ? record[field] : null
    }
    return null
  })
}

// Convenience: detect bind sentinels in a typed prop bag (used by tests
// and the interpreter when it decides whether to wrap a prop in BindResolver).
export function findBindSentinels(props: Record<string, unknown>): UiBind[] {
  const out: UiBind[] = []
  for (const value of Object.values(props)) {
    if (isBindSentinel(value)) {
      out.push(value)
    }
  }
  return out
}

export function findLocalSentinels(value: unknown): UiLocal[] {
  const out: UiLocal[] = []
  walk(value, (v) => {
    if (isLocalSentinel(v)) out.push(v)
  })
  return out
}

// Tree walker used by tests + diagnostics.
export function countBindings(value: unknown): number {
  let count = 0
  walk(value, (v) => {
    if (isBindSentinel(v) || isBindList(v) || isBindIf(v) || isLocalSentinel(v)) count += 1
  })
  return count
}

export function bindingEntityTypes(value: unknown): string[] {
  const types = new Set<string>()
  walk(value, (v) => {
    if (isBindSentinel(v)) {
      const entityType = entityTypeFromPath(v.$bind)
      if (entityType) types.add(entityType)
    } else if (isBindList(v)) {
      const entityType = entityTypeFromPath(v.source)
      if (entityType) types.add(entityType)
    } else if (isBindIf(v)) {
      const entityType = entityTypeFromPath(v.path)
      if (entityType) types.add(entityType)
    }
  })
  return [...types].sort()
}

export function bindingDefaultEntityTypes(value: unknown): string[] {
  const types = new Set<string>()
  walk(value, (v) => {
    if (!isBindList(v)) return
    const entityType = entityTypeFromPath(v.source)
    if (!entityType) return
    if (v.where && typeof v.where === 'object' && !Array.isArray(v.where)) return
    types.add(entityType)
  })
  return [...types].sort()
}

export type EntityHydrationRequest = {
  entity_type: string
  id?: string
  where?: Record<string, unknown>
}

export function stableJson(value: unknown): string {
  if (value === null || typeof value !== 'object') return JSON.stringify(value) ?? 'null'
  if (Array.isArray(value)) return `[${value.map(stableJson).join(',')}]`
  const entries = Object.entries(value as Record<string, unknown>)
    .sort(([a], [b]) => a.localeCompare(b))
  return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${stableJson(item)}`).join(',')}}`
}

export function entityHydrationRequestKey(request: EntityHydrationRequest): string {
  return `${request.entity_type}:${request.id ?? ''}:${stableJson(request.where ?? null)}`
}

function requestFromPath(path: string): EntityHydrationRequest | null {
  const entityType = entityTypeFromPath(path)
  if (!entityType) return null
  const parts = path.split('/').filter(Boolean)
  if (parts.length < 2) return null
  const id = parts[1]
  if (!id || id.includes('*')) return null
  return { entity_type: entityType, id }
}

function requestFromBindList(value: UiBindList): EntityHydrationRequest | null {
  const entityType = entityTypeFromPath(value.source)
  if (!entityType) return null
  const where = value.where
  if (!where || typeof where !== 'object' || Array.isArray(where)) return null
  if (Object.keys(where).length === 0) return null
  return { entity_type: entityType, where: where as Record<string, unknown> }
}

export function bindingEntityRequests(value: unknown): EntityHydrationRequest[] {
  const requests = new Map<string, EntityHydrationRequest>()
  walk(value, (v) => {
    let request: EntityHydrationRequest | null = null
    if (isBindSentinel(v)) {
      request = requestFromPath(v.$bind)
    } else if (isBindIf(v)) {
      request = requestFromPath(v.path)
    } else if (isBindList(v)) {
      request = requestFromBindList(v)
    }
    if (request) requests.set(entityHydrationRequestKey(request), request)
  })
  return [...requests.values()].sort((a, b) =>
    entityHydrationRequestKey(a).localeCompare(entityHydrationRequestKey(b)),
  )
}

export function useBindingInvalidation(value: unknown): void {
  const entityTypes = useMemo(() => bindingEntityTypes(value), [value])
  const getSnapshot = () =>
    entityTypes
      .map((entityType) => {
        const state = storeFor(entityType).getState()
        return `${entityType}:${state.revision ?? state.snapshotSeq}:${state.order.length}`
      })
      .join('\u0000')
  useSyncExternalStore(
    (onStoreChange) => {
      if (entityTypes.length === 0) return () => {}
      const unsubscribers = entityTypes.map((entityType) =>
        storeFor(entityType).subscribe(onStoreChange),
      )
      return () => {
        for (const unsubscribe of unsubscribers) unsubscribe()
      }
    },
    getSnapshot,
    getSnapshot,
  )
}

export function useLocalInvalidation(
  value: unknown,
  hubId?: string,
  targetSurface?: string,
): void {
  const keys = useMemo(
    () => findLocalSentinels(value).map((sentinel) => sentinel.$local).sort(),
    [value],
  )
  const getSnapshot = () =>
    keys
      .map((key) => {
        const state = useUiPresentationStore.getState()
        const scopedKey = state.localKey(hubId, targetSurface, key)
        return `${scopedKey}:${JSON.stringify(state.localValues[scopedKey])}`
      })
      .join('\u0000')
  useSyncExternalStore(
    (onStoreChange) => {
      if (keys.length === 0) return () => {}
      return useUiPresentationStore.subscribe(onStoreChange)
    },
    getSnapshot,
    getSnapshot,
  )
}

function entityTypeFromPath(path: string): string | null {
  if (path.startsWith(ITEM_RELATIVE_PREFIX)) return null
  const [entityType] = path
    .replace(/^\//, '')
    .split('/')
    .filter((s) => s.length > 0)
  return entityType ?? null
}

function walk(value: unknown, visit: (v: unknown) => void): void {
  visit(value)
  if (Array.isArray(value)) {
    for (const item of value) walk(item, visit)
    return
  }
  if (value !== null && typeof value === 'object') {
    for (const v of Object.values(value as Record<string, unknown>)) {
      walk(v, visit)
    }
  }
}

// Re-export a non-component named template so plugin tests can import
// the resolved-tree shape without touching internals.
export type { UiNode }
