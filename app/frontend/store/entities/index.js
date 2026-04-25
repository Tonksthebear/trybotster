// Wire protocol — entity store registry + wire-frame dispatcher.
//
// This module is the SINGLE entry point used by `hub_connection.js` to
// route incoming wire envelopes (`entity_snapshot` / `entity_upsert` /
// `entity_patch` / `entity_remove`) into the right Zustand store. Plugin
// entity types (`<plugin>.<type>`) are registered lazily on first frame
// via `pluginEntityStore`.
//
// Mirrors `cli/src/tui/entity_stores/mod.rs::TuiEntityStores`. Both clients
// must agree on store keying, snapshot_seq semantics, and patch-merge
// rules so a plugin layout that uses `$bind` resolves the same value on
// either renderer.

import { createEntityStore } from './createEntityStore'
import { useSessionStore } from './session-store'
import { useWorkspaceEntityStore } from './workspace-store'
import { useSpawnTargetStore } from './spawn-target-store'
import { useWorktreeStore } from './worktree-store'
import { useHubMetaStore, useConnectionCodeStore } from './hub-meta-store'

/** Built-in stores keyed by wire entity_type. */
export const ENTITY_STORES = {
  session: useSessionStore,
  workspace: useWorkspaceEntityStore,
  spawn_target: useSpawnTargetStore,
  worktree: useWorktreeStore,
  hub: useHubMetaStore,
  connection_code: useConnectionCodeStore,
}

/** Plugin entity types are created on demand. Wire id field defaults to "id". */
const pluginStores = new Map()

/** Default `idField` per built-in entity type, matching design brief §4.1. */
const ID_FIELDS = {
  session: 'session_uuid',
  workspace: 'workspace_id',
  spawn_target: 'target_id',
  worktree: 'worktree_path',
  hub: 'hub_id',
  connection_code: 'hub_id',
}

/**
 * Resolve the store for a given entity_type. Built-in types come from
 * ENTITY_STORES; unknown (plugin) types lazily create a store with idField="id".
 */
export function storeFor(entityType) {
  if (ENTITY_STORES[entityType]) return ENTITY_STORES[entityType]
  let store = pluginStores.get(entityType)
  if (!store) {
    store = createEntityStore(entityType, { idField: 'id' })
    pluginStores.set(entityType, store)
  }
  return store
}

/**
 * Returns true when `messageType` is one of the four entity envelope
 * names. Used by hub_connection.handleMessage to short-circuit before
 * branching into the legacy switch.
 */
export function isEntityFrame(messageType) {
  return (
    messageType === 'entity_snapshot' ||
    messageType === 'entity_upsert' ||
    messageType === 'entity_patch' ||
    messageType === 'entity_remove'
  )
}

/**
 * Apply a single entity envelope to the appropriate store. Returns
 * true when the frame was recognised; the caller can stop forwarding
 * to the legacy switch.
 */
export function applyEntityFrame(frame) {
  const messageType = frame?.type
  if (!isEntityFrame(messageType)) return false
  const entityType = frame.entity_type
  if (typeof entityType !== 'string' || entityType === '') {
    // eslint-disable-next-line no-console
    console.warn('entity store dispatch: missing entity_type', frame)
    return true
  }
  const store = storeFor(entityType).getState()
  const seq = Number.isFinite(frame.snapshot_seq) ? frame.snapshot_seq : 0
  switch (messageType) {
    case 'entity_snapshot': {
      const items = Array.isArray(frame.items) ? frame.items : []
      store.applySnapshot(items, seq)
      break
    }
    case 'entity_upsert': {
      const id = frame.id
      if (typeof id !== 'string' || id === '') {
        // eslint-disable-next-line no-console
        console.warn('entity store dispatch: entity_upsert missing id', frame)
        return true
      }
      store.applyUpsert(id, frame.entity ?? null, seq)
      break
    }
    case 'entity_patch': {
      const id = frame.id
      if (typeof id !== 'string' || id === '') {
        // eslint-disable-next-line no-console
        console.warn('entity store dispatch: entity_patch missing id', frame)
        return true
      }
      store.applyPatch(id, frame.patch ?? {}, seq)
      break
    }
    case 'entity_remove': {
      const id = frame.id
      if (typeof id !== 'string' || id === '') {
        // eslint-disable-next-line no-console
        console.warn('entity store dispatch: entity_remove missing id', frame)
        return true
      }
      store.applyRemove(id, seq)
      break
    }
    default:
      return false
  }
  return true
}

/** Default idField for a registered entity type. Internal helper for tests. */
export function idFieldFor(entityType) {
  return ID_FIELDS[entityType] ?? 'id'
}

/** Test-only — clear every built-in store back to empty. */
export function _resetEntityStoresForTest() {
  for (const useStore of Object.values(ENTITY_STORES)) {
    useStore.getState()._reset()
  }
  for (const useStore of pluginStores.values()) {
    useStore.getState()._reset()
  }
  pluginStores.clear()
}

export {
  useSessionStore,
  useWorkspaceEntityStore,
  useSpawnTargetStore,
  useWorktreeStore,
  useHubMetaStore,
  useConnectionCodeStore,
}
