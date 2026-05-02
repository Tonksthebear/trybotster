import { afterEach, describe, expect, it, vi } from 'vitest'

import { HubTransport } from '../lib/connections/hub_connection'
import {
  _resetUiActionLifecycleForTests,
  beginUiActionLifecycle,
  getUiActionLifecycleSnapshot,
} from '../ui_contract/action_lifecycle_store'

function transport() {
  return new HubTransport('hub-1', { hubId: 'hub-1' }, {
    hasActiveConnectionForHub: () => true,
  })
}

describe('HubTransport ui_action_result frames', () => {
  afterEach(() => {
    _resetUiActionLifecycleForTests()
  })

  it('routes ui_action_result to the lifecycle store and dedicated event only', () => {
    const hub = transport()
    const generic = vi.fn()
    const specific = vi.fn()
    hub.on('message', generic)
    hub.on('uiActionResult', specific)
    const requestId = beginUiActionLifecycle({
      actionId: 'workflow.project.save',
      targetSurface: 'project_pipelines',
      sourceKey: 'save-button',
    })

    hub.handleMessage({
      type: 'ui_action_result',
      v: 1,
      action_request_id: requestId,
      action_id: 'workflow.project.save',
      target_surface: 'project_pipelines',
      ok: true,
      handled: true,
      via: 'handler',
      message: 'Saved',
    })

    expect(getUiActionLifecycleSnapshot(
      'workflow.project.save',
      'project_pipelines',
      'save-button',
    )).toMatchObject({
      pending: false,
      result: {
        action_request_id: requestId,
        ok: true,
        message: 'Saved',
      },
    })
    expect(specific).toHaveBeenCalledOnce()
    expect(generic).not.toHaveBeenCalled()
  })
})
