import { beforeEach, describe, expect, it } from 'vitest'

import {
  activeAgentWorkspaces,
  selectEntity,
  selectEntityField,
  selectEntityList,
} from '../lib/entity-selectors'
import { applyEntityFrame, _resetEntityStoresForTest } from '../store/entities'

describe('generic entity selectors', () => {
  beforeEach(() => {
    _resetEntityStoresForTest()
  })

  it('selects plugin entity lists, records, and fields by entity type and id', () => {
    applyEntityFrame({
      v: 2,
      type: 'entity_snapshot',
      entity_type: 'project-pipelines.ticket',
      items: [
        { id: 'ticket-1', hub_id: 'hub-1', title: 'Plan', status: 'open' },
        { id: 'ticket-2', hub_id: 'hub-1', title: 'Implement', status: 'active' },
      ],
      snapshot_seq: 1,
    })

    expect(selectEntityList({ entityType: 'project-pipelines.ticket' }).map((ticket) => ticket.id)).toEqual([
      'ticket-1',
      'ticket-2',
    ])
    expect(selectEntity({ entityType: 'project-pipelines.ticket', id: 'ticket-2' })).toMatchObject({
      title: 'Implement',
      status: 'active',
    })
    expect(selectEntityField({
      entityType: 'project-pipelines.ticket',
      id: 'ticket-1',
      field: 'title',
    })).toBe('Plan')
  })

  it('filters selector results by hub id without changing store partitioning', () => {
    applyEntityFrame({
      v: 2,
      type: 'entity_snapshot',
      entity_type: 'project-pipelines.ticket',
      items: [
        { id: 'ticket-1', hub_id: 'hub-1', title: 'Plan' },
        { id: 'ticket-2', hub_id: 'hub-2', title: 'Implement' },
      ],
      snapshot_seq: 1,
    })

    expect(selectEntityList({
      entityType: 'project-pipelines.ticket',
      hubId: 'hub-1',
    }).map((ticket) => ticket.id)).toEqual(['ticket-1'])
    expect(selectEntity({
      entityType: 'project-pipelines.ticket',
      id: 'ticket-2',
      hubId: 'hub-1',
    })).toBeUndefined()
    expect(selectEntityField({
      entityType: 'project-pipelines.ticket',
      id: 'ticket-2',
      field: 'title',
      hubId: 'hub-2',
    })).toBe('Implement')
  })

  it('returns empty values for invalid selector arguments', () => {
    expect(selectEntityList()).toEqual([])
    expect(selectEntity({ entityType: 'project-pipelines.ticket', id: '' })).toBeUndefined()
    expect(selectEntityField({
      entityType: 'project-pipelines.ticket',
      id: 'ticket-1',
      field: '',
    })).toBeUndefined()
  })
})

describe('activeAgentWorkspaces', () => {
  it('returns only workspaces with an active agent session', () => {
    const workspacesById = {
      live: { workspace_id: 'live', name: 'Live' },
      empty: { workspace_id: 'empty', name: 'Empty' },
      accessory: { workspace_id: 'accessory', name: 'Accessory only' },
      closed: { workspace_id: 'closed', name: 'Closed', status: 'closed' },
    }
    const sessionsById = {
      agent: {
        session_uuid: 'agent',
        session_type: 'agent',
        workspace_id: 'live',
      },
      accessory: {
        session_uuid: 'accessory',
        session_type: 'accessory',
        workspace_id: 'accessory',
      },
      closedAgent: {
        session_uuid: 'closedAgent',
        session_type: 'agent',
        status: 'closed',
        workspace_id: 'closed',
      },
    }

    expect(activeAgentWorkspaces({
      workspaceOrder: ['live', 'empty', 'accessory', 'closed'],
      workspacesById,
      sessionOrder: ['agent', 'accessory', 'closedAgent'],
      sessionsById,
    }).map((workspace) => workspace.id)).toEqual(['live'])
  })

  it('can exclude the current workspace for move-session choices', () => {
    const workspacesById = {
      current: { workspace_id: 'current', name: 'Current' },
      other: { workspace_id: 'other', name: 'Other' },
    }
    const sessionsById = {
      one: { session_uuid: 'one', session_type: 'agent', workspace_id: 'current' },
      two: { session_uuid: 'two', session_type: 'agent', workspace_id: 'other' },
    }

    expect(activeAgentWorkspaces({
      workspaceOrder: ['current', 'other'],
      workspacesById,
      sessionOrder: ['one', 'two'],
      sessionsById,
      excludeWorkspaceId: 'current',
    }).map((workspace) => workspace.id)).toEqual(['other'])
  })
})
