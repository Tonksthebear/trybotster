// Wire protocol — frontend entity store tests.
//
// Mirrors `cli/tests/entity_broadcast_test.rs` and the TUI's
// `cli/src/tui/entity_stores/mod.rs` test cases. Both clients must apply
// the same wire envelopes the same way; these tests + the Rust ones are
// the redundant evidence that they do.

import { beforeEach, describe, expect, it } from 'vitest'

import { createEntityStore } from '../store/entities/createEntityStore'
import {
  ENTITY_STORES,
  applyEntityFrame,
  isEntityFrame,
  storeFor,
  useSessionStore,
  useWorkspaceEntityStore,
  _resetEntityStoresForTest,
} from '../store/entities'

describe('createEntityStore', () => {
  let store

  beforeEach(() => {
    store = createEntityStore('session', { idField: 'session_uuid' })
  })

  it('applySnapshot replaces contents and orders by items array', () => {
    const s = store.getState()
    s.applySnapshot(
      [
        { session_uuid: 'sess-b', title: 'beta' },
        { session_uuid: 'sess-a', title: 'alpha' },
      ],
      5
    )
    const next = store.getState()
    expect(next.order).toEqual(['sess-b', 'sess-a'])
    expect(next.snapshotSeq).toBe(5)
    expect(next.byId['sess-a']).toEqual({ session_uuid: 'sess-a', title: 'alpha' })
  })

  it('applyUpsert appends new ids but preserves position for updates', () => {
    const s = store.getState()
    s.applySnapshot(
      [
        { session_uuid: 'sess-a', title: 'alpha' },
        { session_uuid: 'sess-b', title: 'beta' },
      ],
      1
    )
    s.applyUpsert('sess-a', { session_uuid: 'sess-a', title: 'alpha2' }, 2)
    s.applyUpsert('sess-c', { session_uuid: 'sess-c', title: 'gamma' }, 3)

    const next = store.getState()
    expect(next.order).toEqual(['sess-a', 'sess-b', 'sess-c'])
    expect(next.byId['sess-a'].title).toBe('alpha2')
  })

  it('applyPatch merges top-level fields and replaces nested objects wholesale', () => {
    const s = store.getState()
    s.applySnapshot(
      [
        {
          session_uuid: 'sess-a',
          title: 'alpha',
          is_idle: true,
          hosted_preview: { status: 'starting', url: null },
        },
      ],
      1
    )

    s.applyPatch('sess-a', { title: 'alpha2', is_idle: false }, 2)
    expect(store.getState().byId['sess-a'].title).toBe('alpha2')
    expect(store.getState().byId['sess-a'].is_idle).toBe(false)

    // Nested object replaces wholesale (per §12.4).
    s.applyPatch('sess-a', { hosted_preview: { status: 'running' } }, 3)
    const hp = store.getState().byId['sess-a'].hosted_preview
    expect(hp.status).toBe('running')
    expect(hp.url).toBeUndefined()
  })

  it('applyPatch on unknown id is a no-op', () => {
    const s = store.getState()
    s.applySnapshot([{ session_uuid: 'sess-a', title: 'alpha' }], 1)
    s.applyPatch('sess-missing', { title: 'phantom' }, 2)
    expect(store.getState().byId['sess-missing']).toBeUndefined()
  })

  it('applyRemove drops id from order and byId', () => {
    const s = store.getState()
    s.applySnapshot(
      [
        { session_uuid: 'sess-a', title: 'a' },
        { session_uuid: 'sess-b', title: 'b' },
      ],
      1
    )
    s.applyRemove('sess-a', 2)
    const next = store.getState()
    expect(next.order).toEqual(['sess-b'])
    expect(next.byId['sess-a']).toBeUndefined()
  })

  it('drops out-of-order frames', () => {
    const s = store.getState()
    s.applySnapshot([{ session_uuid: 'sess-a', title: 'a' }], 5)
    s.applyPatch('sess-a', { title: 'stale' }, 3)
    expect(store.getState().byId['sess-a'].title).toBe('a')
    expect(store.getState().snapshotSeq).toBe(5)
  })

  it('applies snapshot resyncs even when seq is unchanged or lower', () => {
    const s = store.getState()
    s.applySnapshot([{ session_uuid: 'sess-a', title: 'stale' }], 5)
    s.applySnapshot([{ session_uuid: 'sess-b', title: 'fresh' }], 5)
    s.applySnapshot([{ session_uuid: 'sess-c', title: 'reset' }], 4)

    const next = store.getState()
    expect(next.order).toEqual(['sess-c'])
    expect(next.byId['sess-a']).toBeUndefined()
    expect(next.byId['sess-c'].title).toBe('reset')
    expect(next.snapshotSeq).toBe(4)
  })

  it('list() returns [id, entity] pairs in insertion order', () => {
    const s = store.getState()
    s.applySnapshot(
      [
        { session_uuid: 'a', title: 'A' },
        { session_uuid: 'b', title: 'B' },
      ],
      1
    )
    const out = store.getState().list()
    expect(out).toEqual([
      ['a', { session_uuid: 'a', title: 'A' }],
      ['b', { session_uuid: 'b', title: 'B' }],
    ])
  })
})

describe('frontend entity store dispatch', () => {
  beforeEach(() => {
    _resetEntityStoresForTest()
  })

  it('isEntityFrame recognises only the four entity envelope types', () => {
    expect(isEntityFrame('entity_snapshot')).toBe(true)
    expect(isEntityFrame('entity_upsert')).toBe(true)
    expect(isEntityFrame('entity_patch')).toBe(true)
    expect(isEntityFrame('entity_remove')).toBe(true)
    expect(isEntityFrame('agent_list')).toBe(false)
    expect(isEntityFrame('ui_tree_snapshot')).toBe(false)
    expect(isEntityFrame(undefined)).toBe(false)
  })

  it('routes session frames to the session store', () => {
    const handled = applyEntityFrame({
      v: 2,
      type: 'entity_snapshot',
      entity_type: 'session',
      items: [{ session_uuid: 'sess-1', title: 'one' }],
      snapshot_seq: 1,
    })
    expect(handled).toBe(true)
    expect(useSessionStore.getState().byId['sess-1'].title).toBe('one')
  })

  it('routes workspace frames to the workspace entity store', () => {
    applyEntityFrame({
      v: 2,
      type: 'entity_upsert',
      entity_type: 'workspace',
      id: 'ws-1',
      entity: { workspace_id: 'ws-1', name: 'Roadmap' },
      snapshot_seq: 1,
    })
    expect(useWorkspaceEntityStore.getState().byId['ws-1'].name).toBe('Roadmap')
  })

  it('returns false for non-entity frames', () => {
    expect(applyEntityFrame({ type: 'agent_list', agents: [] })).toBe(false)
  })

  it('lazily creates a plugin store for unknown entity_types', () => {
    applyEntityFrame({
      v: 2,
      type: 'entity_snapshot',
      entity_type: 'kanban.board',
      items: [
        { id: 'board-1', name: 'Roadmap' },
        { id: 'board-2', name: 'Triage' },
      ],
      snapshot_seq: 1,
    })
    const pluginStore = storeFor('kanban.board').getState()
    expect(pluginStore.order).toEqual(['board-1', 'board-2'])
  })

  it('built-in stores are exposed in ENTITY_STORES', () => {
    expect(ENTITY_STORES.session).toBe(useSessionStore)
    expect(ENTITY_STORES.workspace).toBe(useWorkspaceEntityStore)
  })
})
