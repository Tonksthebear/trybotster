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
        {inWorktree && ' This session has a worktree on disk.'}
      </DialogDescription>

      <DialogBody>
        {inWorktree && (
          <p className="text-sm text-zinc-400">
            You can keep the worktree for later reuse, or delete it to free disk space.
            Deleting the worktree removes the branch and all uncommitted changes.
          </p>
        )}
      </DialogBody>

      <DialogActions>
        <Button plain onClick={close}>
          Cancel
        </Button>
        {inWorktree && (
          <Button color="red" onClick={confirmDelete}>
            Close &amp; delete worktree
          </Button>
        )}
        <Button outline onClick={confirmKeep}>
          {inWorktree ? 'Close, keep worktree' : 'Close session'}
        </Button>
      </DialogActions>
    </Dialog>
  )
}
