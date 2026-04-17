import React from 'react'
import { Dialog, DialogTitle, DialogDescription, DialogBody, DialogActions } from '../catalyst/dialog'
import { Button } from '../catalyst/button'
import { useDialogStore } from '../../store/dialog-store'
import { useWorkspaceStore, displayName } from '../../store/workspace-store'
import { getHub } from '../../lib/hub-bridge'

export default function DeleteSessionDialog({ hubId }) {
  const { activeDialog, context, close } = useDialogStore()
  const open = activeDialog === 'delete'

  const session = useWorkspaceStore((s) => s.sessionsById[context.sessionId])
  const sessionName = session ? displayName(session) : 'this agent'
  const inWorktree = session?.in_worktree ?? true
  const closeActions = session?.close_actions || {}
  const canDeleteWorktree = closeActions.can_delete_worktree === true
  const deleteWorktreeReason = closeActions.delete_worktree_reason || null
  const otherActiveSessions = Number(closeActions.other_active_sessions || 0)

  function confirmKeep() {
    const hub = getHub(hubId)
    if (hub) hub.deleteAgent(context.sessionId, false)
    close()
  }

  function confirmDelete() {
    const hub = getHub(hubId)
    if (hub) hub.deleteAgent(context.sessionId, true)
    close()
  }

  return (
    <Dialog open={open} onClose={close} size="sm">
      <DialogTitle>Close Session</DialogTitle>
      <DialogDescription>
        Close <strong className="text-white">{sessionName}</strong>?
        {inWorktree && canDeleteWorktree && ' This session has a worktree on disk.'}
        {inWorktree && deleteWorktreeReason === 'other_sessions_active' &&
          ' The workspace stays open because other sessions are still active.'}
        {deleteWorktreeReason === 'not_in_worktree' &&
          ' This session is using the repo checkout, not a detachable worktree.'}
      </DialogDescription>

      <DialogBody>
        {canDeleteWorktree && (
          <p className="text-sm text-zinc-400">
            You can keep the worktree for later reuse, or delete it to free disk space.
            Deleting the worktree removes the branch and all uncommitted changes.
          </p>
        )}
        {inWorktree && deleteWorktreeReason === 'other_sessions_active' && (
          <p className="text-sm text-zinc-400">
            {otherActiveSessions === 1
              ? 'Another session is still active in this workspace, so closing this session will keep the workspace on disk.'
              : `${otherActiveSessions} other sessions are still active in this workspace, so closing this session will keep the workspace on disk.`}
          </p>
        )}
        {deleteWorktreeReason === 'not_in_worktree' && (
          <p className="text-sm text-zinc-400">
            Closing this session will leave the repository untouched because it is not running in a separate worktree.
          </p>
        )}
      </DialogBody>

      <DialogActions>
        <Button plain onClick={close}>
          Cancel
        </Button>
        {canDeleteWorktree && (
          <Button color="red" onClick={confirmDelete}>
            Close &amp; delete worktree
          </Button>
        )}
        <Button outline onClick={confirmKeep}>
          {canDeleteWorktree ? 'Close, keep worktree' : 'Close session'}
        </Button>
      </DialogActions>
    </Dialog>
  )
}
