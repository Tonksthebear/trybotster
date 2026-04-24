// Wire protocol v2 — B2 regression test: a workspace transitioning to
// `status: "closed"` via entity_patch updates the client-side workspace
// store, and the MoveSessionDialog's status==="active" filter drops it.
//
// Mirrors the Rust-side emission in connections.lua's workspace_closed
// hook handler. The hub emits:
//
//   { v:2, type:"entity_patch", entity_type:"workspace",
//     id:"ws-1", patch:{ status:"closed" } }
//
// and the frontend `applyEntityFrame` routes it to
// useWorkspaceEntityStore. This test drives that path end-to-end.

import { beforeEach, describe, expect, it } from 'vitest'

import {
  applyEntityFrame,
  useWorkspaceEntityStore,
  _resetEntityStoresForTest,
} from '../store/entities'

describe('workspace_closed wire flow', () => {
  beforeEach(() => {
    _resetEntityStoresForTest()
    applyEntityFrame({
      v: 2,
      type: 'entity_snapshot',
      entity_type: 'workspace',
      items: [
        { workspace_id: 'ws-active', name: 'Roadmap', status: 'active' },
        { workspace_id: 'ws-active-2', name: 'Triage', status: 'active' },
      ],
      snapshot_seq: 1,
    })
  })

  it('entity_patch with status:closed updates the workspace record', () => {
    applyEntityFrame({
      v: 2,
      type: 'entity_patch',
      entity_type: 'workspace',
      id: 'ws-active',
      patch: { status: 'closed' },
      snapshot_seq: 2,
    })
    const ws = useWorkspaceEntityStore.getState().byId['ws-active']
    expect(ws.status).toBe('closed')
    // name stays intact through the patch (top-level field merge).
    expect(ws.name).toBe('Roadmap')
  })

  it('closed workspaces survive in the store but drop from status==="active" filter', () => {
    applyEntityFrame({
      v: 2,
      type: 'entity_patch',
      entity_type: 'workspace',
      id: 'ws-active',
      patch: { status: 'closed' },
      snapshot_seq: 2,
    })
    const { byId, order } = useWorkspaceEntityStore.getState()
    // Workspace is still in the store (can be re-opened later; session
    // recovery may reactivate it).
    expect(order).toContain('ws-active')
    // But a component-level filter for active-only drops it.
    const active = Object.values(byId).filter((ws) => ws?.status === 'active')
    expect(active).toHaveLength(1)
    expect(active[0].workspace_id).toBe('ws-active-2')
  })

  it('subsequent patch can re-activate the workspace', () => {
    applyEntityFrame({
      v: 2,
      type: 'entity_patch',
      entity_type: 'workspace',
      id: 'ws-active',
      patch: { status: 'closed' },
      snapshot_seq: 2,
    })
    applyEntityFrame({
      v: 2,
      type: 'entity_patch',
      entity_type: 'workspace',
      id: 'ws-active',
      patch: { status: 'active' },
      snapshot_seq: 3,
    })
    expect(useWorkspaceEntityStore.getState().byId['ws-active'].status).toBe('active')
  })
})
