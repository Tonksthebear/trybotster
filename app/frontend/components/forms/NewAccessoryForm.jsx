import React, { useState, useEffect, useMemo } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { Dialog, DialogTitle, DialogDescription, DialogBody, DialogActions } from '../catalyst/dialog'
import { Field, Label, Description } from '../catalyst/fieldset'
import { Select } from '../catalyst/select'
import { Button } from '../catalyst/button'
import { useDialogStore } from '../../store/dialog-store'
import { waitForHub } from '../../lib/hub-bridge'
import { agentConfigQueryOptions, useAgentConfigQuery } from '../../lib/queries'
import WorkspacePicker from './WorkspacePicker'
import {
  useSpawnTargetStore,
  useSessionStore,
  useWorktreeStore,
  useWorkspaceEntityStore,
} from '../../store/entities'
import {
  activeAgentWorkspaces,
  entityId,
  normalizedWorktree,
  spawnTargetLabel,
} from '../../lib/entity-selectors'

export default function NewAccessoryForm({ hubId }) {
  const { activeDialog, context, close } = useDialogStore()
  const open = activeDialog === 'newAccessory'
  const queryClient = useQueryClient()

  const spawnTargetOrder = useSpawnTargetStore((state) => state.order)
  const spawnTargetsById = useSpawnTargetStore((state) => state.byId)
  const spawnTargets = useMemo(
    () => spawnTargetOrder.map((id) => spawnTargetsById[id]).filter(Boolean),
    [spawnTargetOrder, spawnTargetsById],
  )
  const [selectedTargetId, setSelectedTargetId] = useState('')
  const [selectedAccessory, setSelectedAccessory] = useState(null)
  const [selectedWorktree, setSelectedWorktree] = useState(null)
  // { id: string|null, name: string|null } | null
  const [workspaceChoice, setWorkspaceChoice] = useState(null)
  const [submitting, setSubmitting] = useState(false)
  const worktreeOrder = useWorktreeStore((state) => state.order)
  const worktreesById = useWorktreeStore((state) => state.byId)
  const worktrees = useMemo(() => {
    return worktreeOrder
      .map((id) => worktreesById[id])
      .filter((worktree) => worktree && (!selectedTargetId || worktree.target_id === selectedTargetId))
      .map(normalizedWorktree)
  }, [selectedTargetId, worktreeOrder, worktreesById])
  const workspaceOrder = useWorkspaceEntityStore((state) => state.order)
  const workspacesById = useWorkspaceEntityStore((state) => state.byId)
  const sessionOrder = useSessionStore((state) => state.order)
  const sessionsById = useSessionStore((state) => state.byId)
  const workspaces = useMemo(
    () => activeAgentWorkspaces({
      workspaceOrder,
      workspacesById,
      sessionOrder,
      sessionsById,
    }),
    [workspaceOrder, workspacesById, sessionOrder, sessionsById],
  )
  const agentConfigQuery = useAgentConfigQuery(hubId, selectedTargetId, {
    enabled: open && !!selectedTargetId,
  })
  const accessories = useMemo(
    () => Array.isArray(agentConfigQuery.data?.accessories) ? agentConfigQuery.data.accessories : [],
    [agentConfigQuery.data],
  )
  const configStatus = !selectedTargetId
    ? 'idle'
    : agentConfigQuery.isError
      ? 'error'
      : agentConfigQuery.isPending
        ? 'loading'
        : 'loaded'

  // Subscribe to hub data
  useEffect(() => {
    if (!open || !hubId) return

    let cancelled = false
    const unsubs = []

    waitForHub(hubId).then((hub) => {
      if (cancelled || !hub) return
    })

    return () => {
      cancelled = true
      unsubs.forEach((unsub) => unsub())
    }
  }, [open, hubId])

  useEffect(() => {
    if (!open || !hubId || spawnTargets.length === 0) return
    let cancelled = false

    waitForHub(hubId).then((hub) => {
      if (cancelled || !hub) return
      spawnTargets.forEach((target, index) => {
        const targetId = entityId(target, `target:${index}`)
        if (!targetId) return
        queryClient.prefetchQuery(agentConfigQueryOptions(hubId, targetId))
      })
    })

    return () => {
      cancelled = true
    }
  }, [open, hubId, spawnTargets, queryClient])

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
      setSelectedWorktree(null)
      setWorkspaceChoice(null)
      setSubmitting(false)
    }
  }, [open])

  async function applyTarget(targetId) {
    setSelectedTargetId(targetId)
    setSelectedAccessory(null)
    setSelectedWorktree(null)

    const hub = await waitForHub(hubId)
    if (!hub || !targetId) return
  }

  function handleTargetChange(e) {
    applyTarget(e.target.value || null)
  }

  async function handleSubmit() {
    if (!selectedAccessory || !selectedTargetId) return

    const hub = await waitForHub(hubId)
    if (!hub) return

    setSubmitting(true)

    const sent = await hub.createAccessory(
      selectedAccessory,
      workspaceChoice?.id || null,
      workspaceChoice?.name || null,
      selectedTargetId,
      selectedWorktree
        ? { fromWorktree: selectedWorktree.path, branch: selectedWorktree.branch }
        : {},
    )

    if (!sent) {
      setSubmitting(false)
      return
    }

    close()
  }

  const targetPrompt = selectedTargetId
    ? 'Spawn target selected. Now choose main, an existing worktree, and an accessory configuration.'
    : spawnTargets.length === 0
      ? 'Add a spawn target in Device Settings before starting an accessory.'
      : 'Choose a spawn target to unlock accessory configuration.'

  return (
    <Dialog open={open} onClose={close} size="lg">
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

        {/* Worktree/branch options — visible when target selected */}
        {selectedTargetId && (
          <div className="mt-6 space-y-4" data-new-accessory-form-target="worktreeOptions">
            <button
              type="button"
              onClick={() => setSelectedWorktree(null)}
              data-selected={!selectedWorktree ? 'true' : undefined}
              className="w-full text-left px-4 py-3 rounded-lg border border-zinc-700 bg-zinc-900 hover:bg-zinc-800 hover:border-zinc-600 transition-colors data-[selected=true]:border-indigo-500 data-[selected=true]:bg-indigo-500/10"
            >
              <div className="flex items-center gap-2">
                <svg className="size-4 text-blue-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M13 10V3L4 14h7v7l9-11h-7z" />
                </svg>
                <span className="text-sm font-medium text-zinc-100">Main branch</span>
              </div>
              <div className="text-xs text-zinc-500 mt-1">Start the accessory in the admitted spawn target directory</div>
            </button>

            {worktrees.length > 0 && (
              <div>
                <div className="flex items-center justify-between mb-2">
                  <p className="text-xs font-medium text-zinc-400 uppercase tracking-wider">
                    Connect to existing git worktree
                  </p>
                </div>
                <div className="space-y-1">
                  {worktrees.map((wt) => {
                    const label = wt.issue_number ? `Issue #${wt.issue_number}` : (wt.branch || wt.path)
                    return (
                      <button
                        key={wt.path}
                        type="button"
                        onClick={() => setSelectedWorktree(wt)}
                        data-selected={selectedWorktree?.path === wt.path ? 'true' : undefined}
                        className="w-full text-left px-3 py-2 rounded-lg hover:bg-zinc-700 text-zinc-300 transition-colors data-[selected=true]:bg-indigo-500/10 data-[selected=true]:text-zinc-100"
                      >
                        <div className="flex items-center gap-2">
                          <svg className="size-4 text-emerald-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z" />
                          </svg>
                          <span className="font-mono text-sm">{label}</span>
                          {(wt.active_sessions || 0) > 0 && (
                            <span className="rounded-full border border-zinc-600 px-2 py-0.5 text-[11px] text-zinc-300">
                              {wt.active_sessions} active
                            </span>
                          )}
                        </div>
                        <div className="text-xs text-zinc-500 mt-1 truncate">{wt.path}</div>
                      </button>
                    )
                  })}
                </div>
              </div>
            )}

            {worktrees.length === 0 && (
              <div className="text-center py-4 text-zinc-500 text-sm">No existing worktrees</div>
            )}
          </div>
        )}

        {/* Accessory list */}
        {selectedTargetId && (
          <div className="mt-6">
            {configStatus === 'loading' ? (
              <div className="rounded-lg border border-zinc-700 bg-zinc-900/70 px-4 py-3">
                <p className="text-sm text-zinc-300">
                  Loading accessory configurations for this spawn target...
                </p>
              </div>
            ) : accessories.length > 0 ? (
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
            ) : configStatus === 'error' ? (
              <div className="rounded-lg border border-red-500/20 bg-red-500/5 px-4 py-3">
                <p className="text-sm text-red-300">
                  Could not load accessory configurations for this target.
                </p>
              </div>
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
          disabled={submitting || !selectedAccessory || !selectedTargetId}
        >
          Create Accessory
        </Button>
      </DialogActions>
    </Dialog>
  )
}
