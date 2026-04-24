import React, { useState, useEffect } from 'react'
import { Dialog, DialogTitle, DialogDescription, DialogBody, DialogActions } from '../catalyst/dialog'
import { Field, Label, Description } from '../catalyst/fieldset'
import { Input } from '../catalyst/input'
import { Button } from '../catalyst/button'
import { useDialogStore } from '../../store/dialog-store'
import {
  useSessionStore,
  useWorkspaceEntityStore,
} from '../../store/entities'
import { getHub } from '../../lib/hub-bridge'

// Wire protocol v2: workspace.agents arrays are gone — membership is
// derived client-side by joining sessions where workspace_id matches. The
// displayName selector lives here too (used to be on workspace-store).
function displayName(session) {
  if (!session) return ''
  const label = typeof session.label === 'string' ? session.label.trim() : ''
  if (label) return label
  return session.display_name || session.title || session.session_uuid || ''
}

export default function MoveSessionDialog({ hubId }) {
  const { activeDialog, context, close } = useDialogStore()
  const open = activeDialog === 'move'
  const [newWorkspaceName, setNewWorkspaceName] = useState('')

  const workspacesById = useWorkspaceEntityStore((s) => s.byId)
  const sessionsById = useSessionStore((s) => s.byId)

  const session = sessionsById[context.sessionId]
  const sessionName = session ? displayName(session) : 'this session'

  // Wire protocol v2: derive membership client-side. Active workspaces are
  // any with status==='active'; current workspace is the one whose id
  // matches this session's workspace_id.
  const workspaces = Object.values(workspacesById).filter(
    (ws) => ws && ws.status === 'active'
  )
  const currentWorkspaceId = session?.workspace_id ?? null
  const otherWorkspaces = workspaces.filter(
    (ws) => (ws?.workspace_id ?? ws?.id) !== currentWorkspaceId
  )

  useEffect(() => {
    if (open) setNewWorkspaceName('')
  }, [open])

  function moveToExisting(workspaceId, workspaceName) {
    const hub = getHub(hubId)
    if (hub) hub.moveAgentWorkspace(context.sessionId, workspaceId, workspaceName)
    close()
  }

  function moveToNew(e) {
    e.preventDefault()
    const target = newWorkspaceName.trim()
    if (!target) return
    const hub = getHub(hubId)
    if (hub) hub.moveAgentWorkspace(context.sessionId, null, target)
    close()
  }

  return (
    <Dialog open={open} onClose={close} size="sm">
      <DialogTitle>Move Session</DialogTitle>
      <DialogDescription>
        Move <strong className="text-white">{sessionName}</strong> to another workspace.
      </DialogDescription>

      <DialogBody>
        {otherWorkspaces.length > 0 && (
          <div className="space-y-2">
            <Label>Existing workspaces</Label>
            <div className="space-y-2">
              {otherWorkspaces.map((ws) => {
                const id = ws.workspace_id ?? ws.id
                return (
                <button
                  key={id}
                  type="button"
                  onClick={() => moveToExisting(id, ws.name)}
                  className="w-full text-left px-4 py-3 rounded-lg border border-zinc-700 bg-zinc-900 hover:bg-zinc-800 hover:border-zinc-600 transition-colors"
                >
                  <div className="text-sm font-medium text-zinc-100">
                    {ws.name || id}
                  </div>
                </button>
                )
              })}
            </div>
          </div>
        )}

        {otherWorkspaces.length > 0 && (
          <div className="relative my-6">
            <div className="absolute inset-0 flex items-center">
              <div className="w-full border-t border-zinc-700" />
            </div>
            <div className="relative flex justify-center text-sm">
              <span className="bg-zinc-900 px-2 text-zinc-500">or create new</span>
            </div>
          </div>
        )}

        <form onSubmit={moveToNew}>
          <Field>
            <Label>New workspace name</Label>
            <Input
              autoFocus={otherWorkspaces.length === 0}
              value={newWorkspaceName}
              onChange={(e) => setNewWorkspaceName(e.target.value)}
              placeholder="Enter workspace name"
            />
          </Field>
          <div className="mt-4 flex justify-end">
            <Button type="submit" color="indigo" disabled={!newWorkspaceName.trim()}>
              Move to new workspace
            </Button>
          </div>
        </form>
      </DialogBody>

      <DialogActions>
        <Button plain onClick={close}>
          Cancel
        </Button>
      </DialogActions>
    </Dialog>
  )
}
