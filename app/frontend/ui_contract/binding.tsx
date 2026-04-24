// Wire protocol v2 — `$bind` / `bind_list` resolver for the React renderer.
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

import React, { type ReactNode } from 'react'

import {
  isBindList,
  isBindSentinel,
  type UiBindListV1,
  type UiBindV1,
  type UiNodeV1,
} from './types'
import { storeFor } from '../store/entities'

const ITEM_RELATIVE_PREFIX = '@'

type EntityRecord = Record<string, unknown>
type ItemContext = EntityRecord | undefined

/**
 * Walk `value` (a wire-shape JSON tree) and return a deep copy with every
 * `$bind` sentinel replaced by its resolved value, and every `bind_list`
 * envelope expanded into the per-item array.
 *
 * The walker never errors — missing entity / field / store all resolve to
 * `null`, which the React renderers handle as "field absent".
 */
export function resolveBindings(value: unknown): unknown {
  return resolveBindingsInner(value, undefined)
}

function resolveBindingsInner(value: unknown, item: ItemContext): unknown {
  if (Array.isArray(value)) {
    return value.map((v) => resolveBindingsInner(v, item))
  }
  if (value === null || typeof value !== 'object') {
    return value
  }
  if (isBindSentinel(value)) {
    return resolvePath(value.$bind, item)
  }
  if (isBindList(value)) {
    return expandBindList(value, item)
  }
  // Only walk plain objects (those produced by JSON.parse / object literals).
  // Class instances (e.g. the `Boom` test fixture in ui-tree.test.jsx that
  // exists to verify the React error boundary) are passed through unchanged
  // so their getter throws still fire during the React render phase rather
  // than being eaten by the walker.
  if (!isPlainObject(value)) return value
  const out: Record<string, unknown> = {}
  for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
    out[k] = resolveBindingsInner(v, item)
  }
  return out
}

function isPlainObject(value: unknown): boolean {
  if (value === null || typeof value !== 'object') return false
  const proto = Object.getPrototypeOf(value)
  return proto === Object.prototype || proto === null
}

function expandBindList(envelope: UiBindListV1, parentItem: ItemContext): unknown[] {
  const entityType = envelope.source.replace(/^\//, '')
  const store = storeFor(entityType).getState()
  const out: unknown[] = []
  for (const id of store.order) {
    const record = store.byId[id] as EntityRecord | undefined
    if (record == null) continue
    // Per-item resolution shadows the outer item context.
    const expanded = resolveBindingsInner(envelope.item_template, record)
    if (expanded != null) out.push(expanded)
  }
  // `parentItem` deliberately not threaded down — bind_list always shadows.
  void parentItem
  return out
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
      return resolveList(parts[0])
    case 2:
      return resolveRecord(parts[0], parts[1])
    case 3:
      return resolveScalar(parts[0], parts[1], parts[2])
    default:
      // Too many segments — return null and log for debugging. Matches the
      // TUI's defensive default.
      // eslint-disable-next-line no-console
      console.debug(`binding: path "${path}" has too many segments`)
      return null
  }
}

function resolveList(entityType: string): EntityRecord[] {
  const store = storeFor(entityType).getState()
  return store.order
    .map((id) => store.byId[id] as EntityRecord | undefined)
    .filter((entity): entity is EntityRecord => entity != null)
}

function resolveRecord(entityType: string, id: string): EntityRecord | null {
  const store = storeFor(entityType).getState()
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
 * Currently unused by the built-in v2 composites (they read from stores
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
  const useStore = storeFor(entityType)
  // The selector returns just the slice we care about, so re-renders only
  // happen when that slice changes.
  return useStore((state) => {
    if (parts.length <= 1) {
      return state.order
        .map((id: string) => state.byId[id] as EntityRecord | undefined)
        .filter((entity: EntityRecord | undefined): entity is EntityRecord => entity != null)
    }
    if (parts.length === 2) {
      return (state.byId[parts[1]] as EntityRecord | undefined) ?? null
    }
    if (parts.length === 3) {
      const record = state.byId[parts[1]] as EntityRecord | undefined
      if (record == null) return null
      return parts[2] in record ? record[parts[2]] : null
    }
    return null
  })
}

// Convenience: detect bind sentinels in a typed prop bag (used by tests
// and the interpreter when it decides whether to wrap a prop in BindResolver).
export function findBindSentinels(props: Record<string, unknown>): UiBindV1[] {
  const out: UiBindV1[] = []
  for (const value of Object.values(props)) {
    if (isBindSentinel(value)) {
      out.push(value)
    }
  }
  return out
}

// Tree walker used by tests + diagnostics.
export function countBindings(value: unknown): number {
  let count = 0
  walk(value, (v) => {
    if (isBindSentinel(v) || isBindList(v)) count += 1
  })
  return count
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
export type { UiNodeV1 }
