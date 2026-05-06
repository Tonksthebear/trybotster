// Pure-fn tests for app/frontend/store/selectors/session-row.js.
//
// These selectors used to live on workspace-store.js (deleted in commit
// 25b6900d). Tests assert the fidelity contract: same shapes returned
// for the same inputs, just driven off the session entity record.

import { describe, expect, it } from 'vitest'

import {
  activityState,
  displayName,
  selectSessionRowProps,
  subtext,
  titleLine,
} from '../store/selectors/session-row'

describe('displayName', () => {
  it('prefers a trimmed user-set label', () => {
    expect(
      displayName({ label: '  My Session  ', display_name: 'fallback' }),
    ).toBe('My Session')
  })
  it('falls back to display_name', () => {
    expect(displayName({ display_name: 'beta' })).toBe('beta')
  })
  it('does not use generic entity id as display fallback', () => {
    expect(displayName({ id: 'sess-a' })).toBe('')
  })
  it('falls back to session_uuid as last resort', () => {
    expect(displayName({ session_uuid: 'uuid-z' })).toBe('uuid-z')
  })
  it('returns empty string for null', () => {
    expect(displayName(null)).toBe('')
    expect(displayName(undefined)).toBe('')
  })
  it('treats whitespace-only label as missing', () => {
    expect(displayName({ label: '   ', display_name: 'beta' })).toBe('beta')
  })
})

describe('subtext', () => {
  it('joins target_name, branch_name, and agent_name with middle dot', () => {
    expect(
      subtext({
        target_name: 'backend',
        branch_name: 'feature/api',
        agent_name: 'codex',
      }),
    ).toBe('backend · feature/api · codex')
  })
  it('omits config name when agent_name is absent', () => {
    expect(subtext({ branch_name: 'feature/api' })).toBe('feature/api')
  })
  it('returns "accessory" subtext when accessory has no parts', () => {
    expect(subtext({ session_type: 'accessory' })).toBe('accessory')
  })
  it('does NOT inject "accessory" when accessory has parts', () => {
    expect(
      subtext({ session_type: 'accessory', target_name: 'editor' }),
    ).toBe('editor')
  })
  it('returns empty string for missing record', () => {
    expect(subtext(null)).toBe('')
  })
})

describe('titleLine', () => {
  it('combines title and task with middle dot', () => {
    expect(
      titleLine({
        title: 'Refactor request path',
        task: 'Trim dead routes',
      }),
    ).toBe('Refactor request path · Trim dead routes')
  })
  it('omits title when it equals the primary display name', () => {
    expect(
      titleLine({ label: 'api-work', title: 'api-work', task: 'cleanup' }),
    ).toBe('cleanup')
  })
  it('emits just the task when title is absent', () => {
    expect(titleLine({ task: 'standalone task' })).toBe('standalone task')
  })
  it('returns empty string when both title and task are empty', () => {
    expect(titleLine({})).toBe('')
  })
  it('trims whitespace title', () => {
    expect(titleLine({ title: '   ' })).toBe('')
  })
})

describe('activityState', () => {
  it('marks accessory regardless of output activity', () => {
    expect(
      activityState({ session_type: 'accessory', output_activity: 'active' }),
    ).toBe('accessory')
  })
  it('returns "active" only when output_activity is active', () => {
    expect(activityState({ output_activity: 'active' })).toBe('active')
  })
  it('returns "idle" when output_activity is idle', () => {
    expect(activityState({ output_activity: 'idle' })).toBe('idle')
  })
  it('returns "idle" by default (missing output_activity)', () => {
    expect(activityState({})).toBe('idle')
  })
  it('returns "idle" for null', () => {
    expect(activityState(null)).toBe('idle')
  })
})

describe('selectSessionRowProps', () => {
  const session = {
    id: 'sess-1',
    session_uuid: 'uuid-1',
    label: 'api',
    display_name: 'api',
    title: 'Refactor request path',
    target_name: 'backend',
    branch_name: 'feature/api',
    agent_name: 'codex',
    output_activity: 'active',
    notification: true,
    session_type: 'agent',
    in_worktree: true,
    close_actions: { can_delete_worktree: true },
  }

  it('composes all row props', () => {
    const props = selectSessionRowProps(session, {
      selected: true,
      density: 'sidebar',
    })
    expect(props).toMatchObject({
      sessionId: 'sess-1',
      sessionUuid: 'uuid-1',
      density: 'sidebar',
      primaryName: 'api',
      titleLine: 'Refactor request path',
      subtext: 'backend · feature/api · codex',
      selected: true,
      notification: true,
      sessionType: 'agent',
      activityState: 'active',
      canMoveWorkspace: true,
      canDelete: true,
      inWorktree: true,
    })
    expect(props.closeActions).toEqual({ can_delete_worktree: true })
  })
  it('returns null for missing session', () => {
    expect(selectSessionRowProps(null)).toBeNull()
  })
  it('defaults density to "panel" when caller does not specify', () => {
    expect(selectSessionRowProps(session).density).toBe('panel')
  })
})
