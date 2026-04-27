import { describe, expect, it } from 'vitest'

import { activeAgentWorkspaces } from '../lib/entity-selectors'

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
