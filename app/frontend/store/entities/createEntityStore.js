// Wire protocol v2 — per-entity-type store factory.
//
// Each entity type (`session`, `workspace`, `spawn_target`, `worktree`,
// `hub`, `connection_code`, plus plugin types `<plugin>.<type>`) gets a
// Zustand store built from this factory. The shape mirrors the TUI's
// `EntityStore` (cli/src/tui/entity_stores/mod.rs) so the wire envelopes
// the hub ships flow into both clients with identical semantics.
//
// Patch merging matches design brief §12.4: top-level fields merge, nested
// objects (e.g. `hosted_preview`) replace wholesale rather than deep
// merging. Out-of-order delta frames (snapshot_seq <= last applied) are
// dropped so a re-ordered network delivery doesn't corrupt the local view.
// Snapshots always replace local contents and reset the baseline.

import { create } from 'zustand'

const SNAPSHOT_RESET_SEQ = 0

/**
 * Create a Zustand store for one entity type.
 *
 * @param {string} entityType - Wire identifier ("session", "workspace", ...).
 * @param {object} options - { idField?: string }
 * @returns {import('zustand').UseBoundStore} a Zustand bound store with
 *   { byId, order, snapshotSeq, applySnapshot, applyUpsert, applyPatch,
 *     applyRemove, _reset }.
 */
export function createEntityStore(entityType, { idField = 'id' } = {}) {
  return create((set, get) => ({
    /** Insertion-ordered list of entity ids. */
    order: [],
    /** id → entity record (untyped — renderers read fields dynamically). */
    byId: {},
    /** Most recent snapshot_seq applied. */
    snapshotSeq: 0,

    /**
     * Replace the store with a fresh snapshot. Order comes from items so the
     * renderer's iteration matches the hub's intent.
     */
    applySnapshot(items, snapshotSeq) {
      const order = []
      const byId = {}
      for (const item of items || []) {
        const id = extractId(item, idField)
        if (id == null) {
          // eslint-disable-next-line no-console
          console.warn(`entity store ${entityType}: snapshot item missing id_field=${idField}`, item)
          continue
        }
        order.push(id)
        byId[id] = item
      }
      set({ order, byId, snapshotSeq: normaliseSeq(snapshotSeq) })
    },

    /**
     * Insert a new entity or replace an existing one wholesale. Position in
     * `order` is preserved on update; new ids append.
     */
    applyUpsert(id, entity, snapshotSeq) {
      if (!acceptSeq(get(), snapshotSeq, 'upsert')) return
      const { byId, order } = get()
      const nextById = { ...byId, [id]: entity }
      const nextOrder = order.includes(id) ? order : [...order, id]
      set({ byId: nextById, order: nextOrder, snapshotSeq: normaliseSeq(snapshotSeq) })
    },

    /**
     * Merge a sparse patch into an existing entity. Top-level fields merge;
     * nested objects in the patch REPLACE existing nested objects wholesale
     * (per design brief §12.4 — `hosted_preview` is the canonical example).
     * No-op when the entity is unknown — the next snapshot reconciles.
     */
    applyPatch(id, patch, snapshotSeq) {
      if (!acceptSeq(get(), snapshotSeq, 'patch')) return
      const { byId } = get()
      const existing = byId[id]
      if (!existing) {
        // eslint-disable-next-line no-console
        console.debug(`entity store ${entityType}: patch for unknown id ${id} — will reconcile on next snapshot`)
        // Still bump the seq so subsequent strictly-ordered patches don't
        // re-trigger this branch unnecessarily. The next snapshot rebuilds.
        set({ snapshotSeq: normaliseSeq(snapshotSeq) })
        return
      }
      if (!patch || typeof patch !== 'object') return
      const merged = { ...existing }
      for (const key of Object.keys(patch)) {
        merged[key] = patch[key]
      }
      set({
        byId: { ...byId, [id]: merged },
        snapshotSeq: normaliseSeq(snapshotSeq),
      })
    },

    /** Drop an entity. Idempotent. */
    applyRemove(id, snapshotSeq) {
      if (!acceptSeq(get(), snapshotSeq, 'remove')) return
      const { byId, order } = get()
      if (!(id in byId)) {
        set({ snapshotSeq: normaliseSeq(snapshotSeq) })
        return
      }
      const { [id]: _removed, ...rest } = byId
      set({
        byId: rest,
        order: order.filter((existing) => existing !== id),
        snapshotSeq: normaliseSeq(snapshotSeq),
      })
    },

    /**
     * Iterator helper used by selectors that need entities in insertion
     * order. Returns `[id, entity]` pairs.
     */
    list() {
      const { byId, order } = get()
      return order.map((id) => [id, byId[id]]).filter(([, entity]) => entity != null)
    },

    /** Test-only — reset to empty. */
    _reset() {
      set({ order: [], byId: {}, snapshotSeq: 0 })
    },
  }))
}

function extractId(entity, idField) {
  if (entity == null || typeof entity !== 'object') return null
  const fromField = entity[idField]
  if (typeof fromField === 'string' && fromField !== '') return fromField
  const fromId = entity.id
  if (typeof fromId === 'string' && fromId !== '') return fromId
  return null
}

function acceptSeq(current, snapshotSeq, op) {
  // snapshot_seq == 0 marks the very first delta for an empty registered
  // type; subsequent deltas must strictly increase. Snapshots intentionally
  // bypass this check because subscribe/reconnect uses them as authoritative
  // resync frames, often with the same seq as the latest delta.
  const seq = normaliseSeq(snapshotSeq)
  if (seq === SNAPSHOT_RESET_SEQ) return true
  if (seq <= current.snapshotSeq) {
    // eslint-disable-next-line no-console
    console.debug(`entity store: dropping out-of-order ${op} (seq=${seq}, last=${current.snapshotSeq})`)
    return false
  }
  return true
}

function normaliseSeq(seq) {
  return Number.isFinite(seq) && seq >= 0 ? Math.floor(seq) : 0
}
