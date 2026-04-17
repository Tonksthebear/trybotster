import React, { useState, useEffect, useRef } from 'react'
import { Dialog, DialogTitle, DialogBody, DialogActions } from '../catalyst/dialog'
import { Field, Label } from '../catalyst/fieldset'
import { Input } from '../catalyst/input'
import { Button } from '../catalyst/button'
import { useDialogStore } from '../../store/dialog-store'
import { getHub } from '../../lib/hub-bridge'

export default function RenameWorkspaceDialog({ hubId }) {
  const { activeDialog, context, close } = useDialogStore()
  const open = activeDialog === 'rename'
  const inputRef = useRef(null)
  const [name, setName] = useState('')

  useEffect(() => {
    if (open) {
      setName(context.title || '')
    }
  }, [open, context.title])

  // Select text when dialog opens
  useEffect(() => {
    if (open) {
      // Small delay to let the dialog animate in and the input mount
      const timer = setTimeout(() => inputRef.current?.select(), 50)
      return () => clearTimeout(timer)
    }
  }, [open])

  function handleSubmit(e) {
    e.preventDefault()
    const trimmed = name.trim()
    if (!trimmed || trimmed === context.title) return

    const hub = getHub(hubId)
    if (hub) hub.renameWorkspace(context.workspaceId, trimmed)
    close()
  }

  return (
    <Dialog open={open} onClose={close} size="sm">
      <form onSubmit={handleSubmit}>
        <DialogTitle>Rename Workspace</DialogTitle>
        <DialogBody>
          <Field>
            <Label>Workspace name</Label>
            <Input
              ref={inputRef}
              autoFocus
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </Field>
        </DialogBody>
        <DialogActions>
          <Button plain onClick={close}>
            Cancel
          </Button>
          <Button type="submit" color="indigo">
            Rename
          </Button>
        </DialogActions>
      </form>
    </Dialog>
  )
}
