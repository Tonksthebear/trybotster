// Pure-function selectors for rendering one session row.
//
// These fns used to live on the v1 `app/frontend/store/workspace-store.js`
// (deleted in commit 25b6900d as part of the wire protocol v2 cold-turkey
// switch). They're ported here as pure fns — they take a session entity
// record and return derived display strings — so they can be reused by:
//
//   * `<SessionList>` — the main multi-row composite
//   * `<SessionRow>`  — the single-row variant (`ui.session_row{}`)
//   * `<SessionActionsMenu>` — derives `previewState` for menu enablement
//
// No Zustand store. No side effects. The session record itself is the same
// shape `ClientSessionPayload.build` ships under v2 — every field referenced
// below is already projected through `Session.info()` in lib/session.lua.

/**
 * @typedef {object} SessionRecord
 * @property {string} [id]                 deprecated alias for session_uuid
 * @property {string} [session_uuid]
 * @property {string} [label]              user-overridden display name
 * @property {string} [display_name]       hub-derived display name
 * @property {string} [title]              live OSC title from the PTY
 * @property {string} [task]               agent task string
 * @property {string} [target_name]        spawn target friendly name
 * @property {string} [branch_name]
 * @property {string} [agent_name]
 * @property {string} [profile_name]       legacy alias for agent_name
 * @property {string} [session_type]       'agent' | 'accessory'
 * @property {boolean} [is_idle]
 * @property {boolean} [notification]
 * @property {number} [port]               present iff a hosted preview can run
 * @property {boolean} [in_worktree]
 * @property {object} [hosted_preview]     { status, url, error, install_url }
 * @property {object} [close_actions]      { can_delete_worktree, ... }
 */

/**
 * Primary display name. Preference order:
 *   1. user-set `label` (trimmed)
 *   2. `display_name` (hub-derived)
 *   3. `id` / `session_uuid` as last-resort identifier
 *
 * @param {SessionRecord} session
 * @returns {string}
 */
export function displayName(session) {
  if (!session) return ''
  const label = typeof session.label === 'string' ? session.label.trim() : ''
  if (label) return label
  return session.display_name || session.id || session.session_uuid || ''
}

/**
 * Subtext composed from spawn-target / branch / agent-name parts. For
 * accessory sessions with no parts, returns 'accessory' as a single-word
 * subtext so the row carries a discriminator.
 *
 * @param {SessionRecord} session
 * @returns {string}
 */
export function subtext(session) {
  if (!session) return ''
  const parts = []
  if (session.target_name) parts.push(session.target_name)
  if (session.branch_name) parts.push(session.branch_name)
  const configName = session.agent_name || session.profile_name
  if (configName) parts.push(configName)
  if (session.session_type === 'accessory' && parts.length === 0) {
    parts.push('accessory')
  }
  return parts.join(' · ')
}

/**
 * Title line — the live OSC title plus the agent task. Suppressed when the
 * title equals the primary display name (avoids "Roadmap · Roadmap" rows
 * for sessions whose label and title are the same).
 *
 * @param {SessionRecord} session
 * @returns {string}
 */
export function titleLine(session) {
  if (!session) return ''
  const parts = []
  const title = typeof session.title === 'string' ? session.title.trim() : ''
  const primary = displayName(session)
  if (title && title !== primary) parts.push(title)
  if (session.task) parts.push(session.task)
  return parts.join(' · ')
}

/**
 * High-level activity bucket used to drive the dot color/visibility:
 *   - `accessory` for accessory sessions (no agent autonomy → no activity)
 *   - `idle` for agent sessions where `is_idle !== false`
 *     (default true so a brand-new session reads as idle, not active)
 *   - `active` only when `is_idle === false`
 *
 * @param {SessionRecord} session
 * @returns {'accessory' | 'idle' | 'active'}
 */
export function activityState(session) {
  if (!session) return 'idle'
  if (session.session_type === 'accessory') return 'accessory'
  return session.is_idle !== false ? 'idle' : 'active'
}

/**
 * Hosted-preview view-model. Always returns a `canPreview` boolean — even
 * for sessions that don't carry a preview, callers can early-return on
 * `!canPreview` instead of repeatedly checking `session.port`.
 *
 * @param {SessionRecord} session
 * @returns {{
 *   canPreview: boolean,
 *   status?: 'inactive' | 'starting' | 'running' | 'error' | 'unavailable',
 *   url?: string|null,
 *   error?: string|null,
 *   installUrl?: string|null,
 * }}
 */
export function previewState(session) {
  if (!session) return { canPreview: false }
  const hp = session.hosted_preview
  return {
    canPreview: !!session.port,
    status: hp?.status || 'inactive',
    url: typeof hp?.url === 'string' ? hp.url : null,
    error: hp?.error || null,
    installUrl: typeof hp?.install_url === 'string' ? hp.install_url : null,
  }
}

/**
 * One-shot row-props selector: composes everything `<SessionList>` /
 * `<SessionRow>` need to render a single row. Includes the actions-menu
 * availability flags and the close_actions field so the actions popover
 * and the delete dialog can render without re-deriving.
 *
 * `selected` and `density` come in via the caller (browser-local state),
 * not the session record itself.
 *
 * @param {SessionRecord} session
 * @param {{ selected?: boolean, density?: 'sidebar' | 'panel' }} [opts]
 */
export function selectSessionRowProps(session, opts = {}) {
  if (!session) return null
  const preview = previewState(session)
  const sessionUuid = session.session_uuid || session.id || ''
  return {
    sessionId: session.id || sessionUuid,
    sessionUuid,
    density: opts.density === 'sidebar' ? 'sidebar' : 'panel',
    primaryName: displayName(session),
    titleLine: titleLine(session),
    subtext: subtext(session),
    selected: opts.selected === true,
    notification: !!session.notification,
    sessionType: session.session_type || 'agent',
    activityState: activityState(session),
    hostedPreview: preview.canPreview ? preview : null,
    previewError: preview.status === 'error' ? preview.error : null,
    actionsMenu: {
      canPreview: preview.canPreview,
      previewStatus: preview.status,
      previewUrl: preview.url,
      canMove: true,
      canDelete: true,
    },
    closeActions: session.close_actions || {},
    canMoveWorkspace: true,
    canDelete: true,
    inWorktree: session.in_worktree ?? true,
  }
}

/**
 * Hosted-preview indicator props for surfaces that render the preview
 * affordance without the rest of the row (e.g. a future status-bar pill).
 * Returns `null` when the session can't preview.
 *
 * @param {SessionRecord} session
 */
export function selectHostedPreviewIndicatorProps(session) {
  if (!session) return null
  const preview = previewState(session)
  if (!preview.canPreview) return null
  return {
    status: preview.status,
    url: preview.url,
    error: preview.error,
    installUrl: preview.installUrl,
  }
}
