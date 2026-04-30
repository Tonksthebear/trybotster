// Pure-function selectors for rendering one session row.
//
// These pure selectors take a session entity record and return derived
// display strings so they can be reused by:
//
//   * `<SessionList>` — the main multi-row composite
//   * `<SessionRow>`  — the single-row variant (`ui.session_row{}`)
//
// No Zustand store. No side effects. The session record itself is the same
// shape `ClientSessionPayload.build` ships under the wire protocol — every field referenced
// below is already projected through `Session.info()` in lib/session.lua.

/**
 * @typedef {object} SessionRecord
 * @property {string} [id]                 generic entity-store id
 * @property {string} [session_uuid]
 * @property {string} [label]              user-overridden display name
 * @property {string} [display_name]       hub-derived display name
 * @property {string} [title]              live OSC title from the PTY
 * @property {string} [task]               agent task string
 * @property {string} [target_name]        spawn target friendly name
 * @property {string} [branch_name]
 * @property {string} [agent_name]
 * @property {string} [session_type]       'agent' | 'accessory'
 * @property {boolean} [is_idle]
 * @property {boolean} [notification]
 * @property {boolean} [in_worktree]
 * @property {object} [close_actions]      { can_delete_worktree, ... }
 */

/**
 * Primary display name. Preference order:
 *   1. user-set `label` (trimmed)
 *   2. `display_name` (hub-derived)
 *   3. `session_uuid` as last-resort identifier
 *
 * @param {SessionRecord} session
 * @returns {string}
 */
export function displayName(session) {
  if (!session) return ''
  const label = typeof session.label === 'string' ? session.label.trim() : ''
  if (label) return label
  return session.display_name || session.session_uuid || ''
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
  const configName = session.agent_name
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
 * One-shot row-props selector: composes everything `<SessionList>` /
 * `<SessionRow>` need to render a single row. Includes the close_actions field
 * so the delete dialog can render without re-deriving.
 *
 * `selected` and `density` come in via the caller (browser-local state),
 * not the session record itself.
 *
 * @param {SessionRecord} session
 * @param {{ selected?: boolean, density?: 'sidebar' | 'panel' }} [opts]
 */
export function selectSessionRowProps(session, opts = {}) {
  if (!session) return null
  const sessionUuid = session.session_uuid || ''
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
    closeActions: session.close_actions || {},
    canMoveWorkspace: true,
    canDelete: true,
    inWorktree: session.in_worktree ?? true,
  }
}
