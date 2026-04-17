import React from 'react'
import RenameWorkspaceDialog from './dialogs/RenameWorkspaceDialog'
import MoveSessionDialog from './dialogs/MoveSessionDialog'
import DeleteSessionDialog from './dialogs/DeleteSessionDialog'
import NewSessionChooser from './dialogs/NewSessionChooser'
import NewAgentForm from './forms/NewAgentForm'
import NewAccessoryForm from './forms/NewAccessoryForm'

/**
 * Renders all dialog components exactly once.
 * Mounted from the entrypoint (application.jsx) on a dedicated DOM element,
 * independent of App instances. The dialog store is global — no hub prop
 * needed here since dialogs read hubId from the modal-bridge singleton.
 */
export default function DialogHost({ hubId }) {
  if (!hubId) return null

  return (
    <>
      <RenameWorkspaceDialog hubId={hubId} />
      <MoveSessionDialog hubId={hubId} />
      <DeleteSessionDialog hubId={hubId} />
      <NewSessionChooser hubId={hubId} />
      <NewAgentForm hubId={hubId} />
      <NewAccessoryForm hubId={hubId} />
    </>
  )
}
