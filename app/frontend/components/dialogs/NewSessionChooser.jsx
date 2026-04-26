import React, { useState, useEffect, useCallback } from 'react'
import { Dialog, DialogTitle, DialogDescription, DialogBody, DialogActions } from '../catalyst/dialog'
import { Field, Label } from '../catalyst/fieldset'
import { Select } from '../catalyst/select'
import { Button } from '../catalyst/button'
import { useDialogStore } from '../../store/dialog-store'
import { waitForHub } from '../../lib/hub-bridge'
import { useSpawnTargetStore } from '../../store/entities'
import { entityId, spawnTargetLabel } from '../../lib/entity-selectors'

export default function NewSessionChooser({ hubId }) {
  const { activeDialog, close, openNewAgent, openNewAccessory } = useDialogStore()
  const open = activeDialog === 'newSession'

  const spawnTargetOrder = useSpawnTargetStore((state) => state.order)
  const spawnTargetsById = useSpawnTargetStore((state) => state.byId)
  const spawnTargets = React.useMemo(
    () => spawnTargetOrder.map((id) => spawnTargetsById[id]).filter(Boolean),
    [spawnTargetOrder, spawnTargetsById],
  )
  const [selectedTargetId, setSelectedTargetId] = useState('')
  const [hubReady, setHubReady] = useState(false)

  useEffect(() => {
    if (!open || !hubId) return

    setHubReady(false)
    let cancelled = false

    function attachToHub(hub) {
      if (cancelled) return
      setHubReady(true)
      hub.requestSpawnTargets?.()
    }

    waitForHub(hubId).then(attachToHub)

    return () => {
      cancelled = true
    }
  }, [open, hubId])

  // Reset selection when opening
  useEffect(() => {
    if (open) setSelectedTargetId('')
  }, [open])

  const chooseAgent = useCallback(() => {
    if (!selectedTargetId) return
    close()
    // Small delay so the close animation finishes before opening the next dialog
    setTimeout(() => openNewAgent({ targetId: selectedTargetId }), 100)
  }, [selectedTargetId, close, openNewAgent])

  const chooseAccessory = useCallback(() => {
    if (!selectedTargetId) return
    close()
    setTimeout(() => openNewAccessory({ targetId: selectedTargetId }), 100)
  }, [selectedTargetId, close, openNewAccessory])

  const prompt = !hubReady
    ? 'Connecting to hub...'
    : selectedTargetId
      ? 'Spawn target selected. Now choose whether to start an agent or an accessory.'
      : spawnTargets.length === 0
        ? 'Add a spawn target in Device Settings before creating a session.'
        : 'Choose a spawn target first. Session type comes after location.'

  return (
    <Dialog open={open} onClose={close} size="sm" data-testid="new-session-chooser-modal">
      <DialogTitle>New Session</DialogTitle>
      <DialogDescription>{prompt}</DialogDescription>

      <DialogBody>
        <Field disabled={!hubReady} data-new-session-chooser-target="targetSection">
          <Label>Spawn target</Label>
          <Select
            data-testid="spawn-target-select"
            data-new-session-chooser-target="targetSelect"
            value={selectedTargetId}
            onChange={(e) => setSelectedTargetId(e.target.value)}
          >
            <option value="">
              {spawnTargets.length ? 'Select a spawn target' : 'No admitted spawn targets'}
            </option>
            {spawnTargets.map((target, index) => {
              const id = entityId(target, `target:${index}`)
              return (
                <option key={id} value={id}>
                  {spawnTargetLabel(target)}
                </option>
              )
            })}
          </Select>
        </Field>

        <div className="mt-6 grid grid-cols-2 gap-3">
          <button
            type="button"
            data-testid="choose-agent"
            data-new-session-chooser-target="agentButton"
            disabled={!selectedTargetId}
            onClick={chooseAgent}
            className="group flex flex-col items-center gap-3 rounded-lg border border-zinc-700 bg-zinc-900 p-4 transition-colors hover:border-indigo-500/50 hover:bg-zinc-800 disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:border-zinc-700 disabled:hover:bg-zinc-900"
          >
            <span className="flex size-10 items-center justify-center rounded-lg bg-indigo-500/10 text-indigo-400 border border-indigo-500/20">
              <svg className="size-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M9.75 3.104v5.714a2.25 2.25 0 01-.659 1.591L5 14.5M9.75 3.104c-.251.023-.501.05-.75.082m.75-.082a24.301 24.301 0 014.5 0m0 0v5.714c0 .597.237 1.17.659 1.591L19.8 15.3M14.25 3.104c.251.023.501.05.75.082M19.8 15.3l-1.57.393A9.065 9.065 0 0112 15a9.065 9.065 0 00-6.23.693L5 14.5m14.8.8l1.402 1.402c1.232 1.232.65 3.318-1.067 3.611A48.309 48.309 0 0112 21c-2.773 0-5.491-.235-8.135-.687-1.718-.293-2.3-2.379-1.067-3.61L5 14.5" />
              </svg>
            </span>
            <div className="text-center">
              <div className="text-sm font-medium text-zinc-100">Agent</div>
              <div className="text-xs text-zinc-500 mt-1">AI-powered session with Claude</div>
            </div>
          </button>

          <button
            type="button"
            data-testid="choose-accessory"
            data-new-session-chooser-target="accessoryButton"
            disabled={!selectedTargetId}
            onClick={chooseAccessory}
            className="group flex flex-col items-center gap-3 rounded-lg border border-zinc-700 bg-zinc-900 p-4 transition-colors hover:border-emerald-500/50 hover:bg-zinc-800 disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:border-zinc-700 disabled:hover:bg-zinc-900"
          >
            <span className="flex size-10 items-center justify-center rounded-lg bg-emerald-500/10 text-emerald-400 border border-emerald-500/20">
              <svg className="size-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M6.75 7.5l3 2.25-3 2.25m4.5 0h3m-9 8.25h13.5A2.25 2.25 0 0021 18V6a2.25 2.25 0 00-2.25-2.25H5.25A2.25 2.25 0 003 6v12a2.25 2.25 0 002.25 2.25z" />
              </svg>
            </span>
            <div className="text-center">
              <div className="text-sm font-medium text-zinc-100">Accessory</div>
              <div className="text-xs text-zinc-500 mt-1">Plain terminal session</div>
            </div>
          </button>
        </div>
      </DialogBody>

      <DialogActions>
        <Button plain onClick={close}>
          Cancel
        </Button>
      </DialogActions>
    </Dialog>
  )
}
