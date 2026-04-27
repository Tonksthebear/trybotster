// Pure-fn tests for app/frontend/store/selectors/session-row.js.
//
// These selectors used to live on workspace-store.js (deleted in commit
// 25b6900d). Tests assert the fidelity contract: same shapes returned
// for the same inputs, just driven off the session entity record.

import { describe, expect, it } from 'vitest'

import {
  activityState,
  displayName,
  previewState,
  selectHostedPreviewIndicatorProps,
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
  it('falls back to id when label and display_name are missing', () => {
    expect(displayName({ id: 'sess-a' })).toBe('sess-a')
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
        agent_name: 'claude',
      }),
    ).toBe('backend · feature/api · claude')
  })
  it('falls back to profile_name when agent_name is absent', () => {
    expect(subtext({ profile_name: 'engineer' })).toBe('engineer')
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
  it('marks accessory regardless of is_idle', () => {
    expect(
      activityState({ session_type: 'accessory', is_idle: false }),
    ).toBe('accessory')
  })
  it('returns "active" only when is_idle === false', () => {
    expect(activityState({ is_idle: false })).toBe('active')
  })
  it('returns "idle" when is_idle is true', () => {
    expect(activityState({ is_idle: true })).toBe('idle')
  })
  it('returns "idle" by default (missing is_idle)', () => {
    expect(activityState({})).toBe('idle')
  })
  it('returns "idle" for null', () => {
    expect(activityState(null)).toBe('idle')
  })
})

describe('previewState', () => {
  it('returns canPreview=false for sessions without a port', () => {
    expect(previewState({ id: 'no-port' })).toMatchObject({
      canPreview: false,
      status: 'inactive',
      url: null,
    })
  })
  it('returns bare {canPreview:false} for null/undefined', () => {
    expect(previewState(null)).toEqual({ canPreview: false })
    expect(previewState(undefined)).toEqual({ canPreview: false })
  })
  it('passes through hosted_preview shape when port is set', () => {
    expect(
      previewState({
        port: 8080,
        hosted_preview: {
          status: 'running',
          url: 'https://x.test',
          error: null,
          install_url: 'https://install.test',
        },
      }),
    ).toEqual({
      canPreview: true,
      status: 'running',
      url: 'https://x.test',
      error: null,
      installUrl: 'https://install.test',
    })
  })
  it('defaults status to "inactive" when hosted_preview is absent', () => {
    expect(previewState({ port: 8080 })).toMatchObject({
      canPreview: true,
      status: 'inactive',
      url: null,
      error: null,
      installUrl: null,
    })
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
    agent_name: 'claude',
    is_idle: false,
    notification: true,
    session_type: 'agent',
    port: 8080,
    hosted_preview: { status: 'running', url: 'https://preview.test' },
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
      subtext: 'backend · feature/api · claude',
      selected: true,
      notification: true,
      sessionType: 'agent',
      activityState: 'active',
      previewError: null,
      canMoveWorkspace: true,
      canDelete: true,
      inWorktree: true,
    })
    expect(props.hostedPreview).toMatchObject({
      canPreview: true,
      status: 'running',
      url: 'https://preview.test',
    })
    expect(props.actionsMenu).toMatchObject({
      canPreview: true,
      previewStatus: 'running',
      previewUrl: 'https://preview.test',
      canMove: true,
      canDelete: true,
    })
    expect(props.closeActions).toEqual({ can_delete_worktree: true })
  })
  it('returns null for missing session', () => {
    expect(selectSessionRowProps(null)).toBeNull()
  })
  it('defaults density to "panel" when caller does not specify', () => {
    expect(selectSessionRowProps(session).density).toBe('panel')
  })
  it('only sets previewError when status === "error"', () => {
    const errored = {
      ...session,
      hosted_preview: { status: 'error', error: 'cloudflared down' },
    }
    expect(selectSessionRowProps(errored).previewError).toBe('cloudflared down')
  })
  it('hostedPreview is null when canPreview is false', () => {
    const noPort = { ...session, port: undefined }
    expect(selectSessionRowProps(noPort).hostedPreview).toBeNull()
  })
})

describe('selectHostedPreviewIndicatorProps', () => {
  it('returns null when canPreview is false', () => {
    expect(
      selectHostedPreviewIndicatorProps({ id: 's' }),
    ).toBeNull()
  })
  it('returns the preview slice when canPreview is true', () => {
    expect(
      selectHostedPreviewIndicatorProps({
        port: 8080,
        hosted_preview: { status: 'running', url: 'https://x.test' },
      }),
    ).toMatchObject({ status: 'running', url: 'https://x.test' })
  })
})
