import { afterEach, describe, expect, it, vi } from 'vitest'

import { HubTransport } from '../lib/connections/hub_connection'
import {
  _resetUiActionLifecycleForTests,
  beginUiActionLifecycle,
  getUiActionLifecycleSnapshot,
  receiveUiActionResult,
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

  it('clears pending only for the matching repeated-row source key', () => {
    const firstRequestId = beginUiActionLifecycle({
      actionId: 'project_pipelines.answer_question',
      targetSurface: 'project_pipelines',
      sourceKey: 'question-1-answer',
    })
    const secondRequestId = beginUiActionLifecycle({
      actionId: 'project_pipelines.answer_question',
      targetSurface: 'project_pipelines',
      sourceKey: 'question-2-answer',
    })

    receiveUiActionResult({
      type: 'ui_action_result',
      v: 1,
      action_request_id: firstRequestId,
      action_id: 'project_pipelines.answer_question',
      target_surface: 'project_pipelines',
      ok: true,
      handled: true,
      via: 'handler',
      message: 'Answered',
    })

    expect(getUiActionLifecycleSnapshot(
      'project_pipelines.answer_question',
      'project_pipelines',
      'question-1-answer',
    )).toMatchObject({
      pending: false,
      result: {
        action_request_id: firstRequestId,
        ok: true,
        message: 'Answered',
      },
    })
    expect(getUiActionLifecycleSnapshot(
      'project_pipelines.answer_question',
      'project_pipelines',
      'question-2-answer',
    )).toMatchObject({
      pending: true,
      result: null,
    })
    expect(firstRequestId).not.toBe(secondRequestId)
  })
})
