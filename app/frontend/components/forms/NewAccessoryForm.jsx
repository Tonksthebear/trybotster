import React, { useState, useEffect, useRef } from 'react'
import { Dialog, DialogTitle, DialogDescription, DialogBody, DialogActions } from '../catalyst/dialog'
import { Field, Label, Description } from '../catalyst/fieldset'
import { Select } from '../catalyst/select'
import { Button } from '../catalyst/button'
import { useDialogStore } from '../../store/dialog-store'
import { getHub } from '../../lib/hub-bridge'
import WorkspacePicker from './WorkspacePicker'

export default function NewAccessoryForm({ hubId }) {
  const { activeDialog, context, close } = useDialogStore()
  const open = activeDialog === 'newAccessory'

  const unsubscribersRef = useRef([])

  const [spawnTargets, setSpawnTargets] = useState([])
  const [accessories, setAccessories] = useState([])
  const [workspaces, setWorkspaces] = useState([])

  const [selectedTargetId, setSelectedTargetId] = useState('')
  const [selectedAccessory, setSelectedAccessory] = useState(null)
  // { id: string|null, name: string|null } | null
  const [workspaceChoice, setWorkspaceChoice] = useState(null)

  // Subscribe to hub data
  useEffect(() => {
    if (!open || !hubId) return

    const hub = getHub(hubId)
    if (!hub) return

    const unsubs = []

    setSpawnTargets(hub.spawnTargets.current())
    setWorkspaces(hub.openWorkspaces.current())
    hub.spawnTargets.load().catch(() => {})
    hub.openWorkspaces.load().catch(() => {})

    unsubs.push(
      hub.spawnTargets.onChange((targets) => {
        setSpawnTargets(Array.isArray(targets) ? targets : [])
      })
    )

    unsubs.push(
      hub.on('agentConfig', ({ targetId, accessories: accs }) => {
        setSelectedTargetId((currentTarget) => {
          if (targetId && currentTarget && targetId !== currentTarget) return currentTarget
          setAccessories(Array.isArray(accs) ? accs : [])
          return currentTarget
        })
      })
    )

    unsubs.push(
      hub.openWorkspaces.onChange((wss) => {
        setWorkspaces(Array.isArray(wss) ? wss : [])
      })
    )

    unsubscribersRef.current = unsubs

    return () => {
      unsubs.forEach((unsub) => unsub())
      unsubscribersRef.current = []
    }
  }, [open, hubId])

  // Apply pre-selected target from context
  useEffect(() => {
    if (open && context.targetId) {
      applyTarget(context.targetId)
    }
  }, [open, context.targetId])

  // Reset on close
  useEffect(() => {
    if (!open) {
      setSelectedTargetId('')
      setSelectedAccessory(null)
      setWorkspaceChoice(null)
      setAccessories([])
    }
  }, [open])

  function applyTarget(targetId) {
    setSelectedTargetId(targetId)
    setSelectedAccessory(null)

    const hub = getHub(hubId)
    if (!hub || !targetId) return

    const config = hub.getAgentConfig(targetId)
    setAccessories(Array.isArray(config.accessories) ? config.accessories : [])
    hub.ensureAgentConfig(targetId, { force: true }).catch(() => {})
  }

  function handleTargetChange(e) {
    applyTarget(e.target.value || null)
  }

  function handleSubmit() {
    if (!selectedAccessory || !selectedTargetId) return

    const hub = getHub(hubId)
    if (!hub) return

    hub.createAccessory(
      selectedAccessory,
      workspaceChoice?.id || null,
      workspaceChoice?.name || null,
      selectedTargetId
    )

    close()
  }

  const targetPrompt = selectedTargetId
    ? 'Spawn target selected. Now choose an accessory configuration.'
    : spawnTargets.length === 0
      ? 'Add a spawn target in Device Settings before starting an accessory.'
      : 'Choose a spawn target to unlock accessory configuration.'

  return (
    <Dialog open={open} onClose={close} size="md">
      <DialogTitle>New Accessory</DialogTitle>
      <DialogDescription>{targetPrompt}</DialogDescription>

      <DialogBody>
        {/* Target selection */}
        <Field>
          <Label>Spawn target</Label>
          <Select value={selectedTargetId} onChange={handleTargetChange}>
            <option value="">
              {spawnTargets.length ? 'Select a spawn target' : 'No admitted spawn targets'}
            </option>
            {spawnTargets.map((target) => {
              const branchSuffix = target.current_branch ? ` (${target.current_branch})` : ''
              return (
                <option key={target.id} value={target.id}>
                  {(target.name || target.path) + branchSuffix}
                </option>
              )
            })}
          </Select>
        </Field>

        {/* Accessory list */}
        {selectedTargetId && (
          <div className="mt-6">
            {accessories.length > 0 ? (
              <>
                <p className="text-sm/6 font-medium text-zinc-950 dark:text-white">Accessory configuration</p>
                <div className="mt-2 space-y-2">
                  {accessories.map((name) => (
                    <button
                      key={name}
                      type="button"
                      onClick={() => setSelectedAccessory(name)}
                      data-selected={selectedAccessory === name ? 'true' : undefined}
                      className="w-full text-left px-3 py-2.5 rounded-lg border transition-colors border-zinc-700 hover:border-indigo-500/50 hover:bg-zinc-800/50 data-[selected=true]:border-indigo-500 data-[selected=true]:bg-indigo-500/10"
                    >
                      <div className="flex items-center gap-3">
                        <span className="size-8 rounded-md bg-zinc-700/50 text-zinc-400 flex items-center justify-center border border-zinc-600/30 shrink-0 font-mono text-xs">
                          &gt;
                        </span>
                        <div className="flex-1 min-w-0">
                          <div className="text-sm font-medium text-zinc-200 font-mono">{name}</div>
                        </div>
                      </div>
                    </button>
                  ))}
                </div>
              </>
            ) : (
              <div className="rounded-lg border border-amber-500/20 bg-amber-500/5 px-4 py-3">
                <p className="text-sm text-amber-300">
                  No accessory configurations found for this target.
                  Add <code className="text-amber-200">.botster/accessories/</code> configs to customize.
                </p>
              </div>
            )}
          </div>
        )}

        {/* Workspace */}
        {selectedTargetId && (
          <div className="mt-6">
            <WorkspacePicker
              workspaces={workspaces}
              value={workspaceChoice}
              onChange={setWorkspaceChoice}
              description="Group this accessory with agents in a workspace, or leave as Default."
            />
          </div>
        )}
      </DialogBody>

      <DialogActions>
        <Button plain onClick={close}>
          Cancel
        </Button>
        <Button
          color="indigo"
          onClick={handleSubmit}
          disabled={!selectedAccessory || !selectedTargetId}
        >
          Create Accessory
        </Button>
      </DialogActions>
    </Dialog>
  )
}
