import React, { useState, useEffect, useCallback, useRef } from 'react'
import { Dialog, DialogTitle, DialogDescription, DialogBody, DialogActions } from '../catalyst/dialog'
import { Field, Label, Description } from '../catalyst/fieldset'
import { Input } from '../catalyst/input'
import { Select } from '../catalyst/select'
import { Button } from '../catalyst/button'
import { useDialogStore } from '../../store/dialog-store'
import { getHub } from '../../lib/hub-bridge'
import WorkspacePicker from './WorkspacePicker'

export default function NewAgentForm({ hubId }) {
  const { activeDialog, context, close } = useDialogStore()
  const open = activeDialog === 'newAgent'

  // Hub subscriptions
  const unsubscribersRef = useRef([])

  // Data from hub
  const [spawnTargets, setSpawnTargets] = useState([])
  const [worktrees, setWorktrees] = useState([])
  const [agents, setAgents] = useState([])
  const [workspaces, setWorkspaces] = useState([])

  // Form state
  const [selectedTargetId, setSelectedTargetId] = useState('')
  const [step, setStep] = useState(1)
  const [pendingSelection, setPendingSelection] = useState(null)
  const [selectionLabel, setSelectionLabel] = useState('')
  const [newBranchInput, setNewBranchInput] = useState('')
  const [promptInput, setPromptInput] = useState('')
  const [selectedAgent, setSelectedAgent] = useState('')
  // { id: string|null, name: string|null } | null
  const [workspaceChoice, setWorkspaceChoice] = useState(null)

  // Subscribe to hub data when dialog opens
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
      hub.on('worktreeList', ({ targetId, worktrees: wts }) => {
        // Only update if it matches our selected target
        setSelectedTargetId((currentTarget) => {
          if (targetId && currentTarget && targetId !== currentTarget) return currentTarget
          setWorktrees(Array.isArray(wts) ? wts : [])
          return currentTarget
        })
      })
    )

    unsubs.push(
      hub.on('agentConfig', ({ targetId, agents: ags }) => {
        setSelectedTargetId((currentTarget) => {
          if (targetId && currentTarget && targetId !== currentTarget) return currentTarget
          const list = Array.isArray(ags) ? ags : []
          setAgents(list)
          setSelectedAgent((prev) => (prev && list.includes(prev)) ? prev : (list[0] || ''))
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

  // Apply pre-selected target from context (from NewSessionChooser)
  useEffect(() => {
    if (open && context.targetId) {
      applyTarget(context.targetId)
    }
  }, [open, context.targetId])

  // Reset form on close
  useEffect(() => {
    if (!open) {
      setStep(1)
      setPendingSelection(null)
      setSelectionLabel('')
      setNewBranchInput('')
      setPromptInput('')
      setSelectedAgent('')
      setWorkspaceChoice(null)
      setSelectedTargetId('')
      setWorktrees([])
      setAgents([])
    }
  }, [open])

  function applyTarget(targetId) {
    setSelectedTargetId(targetId)
    setPendingSelection(null)

    const hub = getHub(hubId)
    if (!hub || !targetId) return

    const wts = hub.getWorktrees(targetId)
    setWorktrees(Array.isArray(wts) ? wts : [])

    const config = hub.getAgentConfig(targetId)
    const agentList = Array.isArray(config.agents) ? config.agents : []
    setAgents(agentList)
    setSelectedAgent(agentList[0] || '')

    if (!hub.hasWorktrees(targetId)) {
      hub.ensureWorktrees(targetId)
    }
    hub.ensureAgentConfig(targetId, { force: true }).catch(() => {})
  }

  function handleTargetChange(e) {
    applyTarget(e.target.value || null)
  }

  function selectWorktree(worktree) {
    setPendingSelection({ type: 'existing', path: worktree.path, branch: worktree.branch })
    const label = worktree.issue_number ? `Issue #${worktree.issue_number}` : worktree.branch
    goToStep2(label)
  }

  function selectMainBranch() {
    setPendingSelection({ type: 'main' })
    goToStep2('main branch')
  }

  function selectNewBranch() {
    const value = newBranchInput.trim()
    if (!value) return
    setPendingSelection({ type: 'new', issueOrBranch: value })
    goToStep2(value)
  }

  function goToStep2(label) {
    setSelectionLabel(label)
    setStep(2)
  }

  function goBackToStep1() {
    setStep(1)
    setPromptInput('')
  }

  function handleRefresh() {
    if (!selectedTargetId) return
    const hub = getHub(hubId)
    if (!hub) return
    hub.ensureWorktrees(selectedTargetId, { force: true })
    hub.ensureAgentConfig(selectedTargetId, { force: true }).catch(() => {})
  }

  function handleSubmit() {
    if (!pendingSelection || !selectedTargetId) return

    const hub = getHub(hubId)
    if (!hub) return

    const prompt = promptInput.trim() || null
    const agentName = selectedAgent || null

    if (pendingSelection.type === 'existing') {
      hub.send('reopen_worktree', {
        path: pendingSelection.path,
        branch: pendingSelection.branch,
        prompt,
        agent_name: agentName,
        target_id: selectedTargetId,
        workspace_id: workspaceChoice?.id || null,
        workspace_name: workspaceChoice?.name || null,
      })
    } else if (pendingSelection.type === 'main') {
      hub.send('create_agent', {
        prompt,
        agent_name: agentName,
        target_id: selectedTargetId,
        workspace_id: workspaceChoice?.id || null,
        workspace_name: workspaceChoice?.name || null,
      })
    } else {
      hub.send('create_agent', {
        issue_or_branch: pendingSelection.issueOrBranch,
        prompt,
        agent_name: agentName,
        target_id: selectedTargetId,
        workspace_id: workspaceChoice?.id || null,
        workspace_name: workspaceChoice?.name || null,
      })
    }

    close()
  }

  const targetPrompt = selectedTargetId
    ? 'Spawn target selected. Now choose main, an existing worktree, or a new branch.'
    : spawnTargets.length === 0
      ? 'Add a spawn target in Device Settings before creating an agent.'
      : 'Choose a spawn target to unlock worktree and branch selection.'

  return (
    <Dialog open={open} onClose={close} size="lg">
      <DialogTitle>New Agent</DialogTitle>

      {step === 1 ? (
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
            <Description>{targetPrompt}</Description>
          </Field>

          {/* Worktree/branch options — visible when target selected */}
          {selectedTargetId && (
            <div className="mt-6 space-y-4">
              {/* Main branch */}
              <button
                type="button"
                onClick={selectMainBranch}
                className="w-full text-left px-4 py-3 rounded-lg border border-zinc-700 bg-zinc-900 hover:bg-zinc-800 hover:border-zinc-600 transition-colors"
              >
                <div className="flex items-center gap-2">
                  <svg className="size-4 text-blue-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M13 10V3L4 14h7v7l9-11h-7z" />
                  </svg>
                  <span className="text-sm font-medium text-zinc-100">Main branch</span>
                </div>
                <div className="text-xs text-zinc-500 mt-1">Create a new worktree from the default branch</div>
              </button>

              {/* Existing worktrees */}
              {worktrees.length > 0 && (
                <div>
                  <div className="flex items-center justify-between mb-2">
                    <p className="text-xs font-medium text-zinc-400 uppercase tracking-wider">
                      Existing worktrees
                    </p>
                    <button
                      type="button"
                      onClick={handleRefresh}
                      className="text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
                    >
                      Refresh
                    </button>
                  </div>
                  <div className="space-y-1">
                    {worktrees.map((wt) => {
                      const label = wt.issue_number ? `Issue #${wt.issue_number}` : wt.branch
                      return (
                        <button
                          key={wt.path}
                          type="button"
                          onClick={() => selectWorktree(wt)}
                          className="w-full text-left px-3 py-2 rounded-lg hover:bg-zinc-700 text-zinc-300 transition-colors"
                        >
                          <div className="flex items-center gap-2">
                            <svg className="size-4 text-emerald-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z" />
                            </svg>
                            <span className="font-mono text-sm">{label}</span>
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

              {/* New branch input */}
              <div>
                <p className="text-xs font-medium text-zinc-400 uppercase tracking-wider mb-2">
                  New branch or issue
                </p>
                <div className="flex gap-2">
                  <div className="flex-1">
                    <Input
                      value={newBranchInput}
                      onChange={(e) => setNewBranchInput(e.target.value)}
                      placeholder="Branch name or issue #"
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') {
                          e.preventDefault()
                          selectNewBranch()
                        }
                      }}
                    />
                  </div>
                  <Button
                    outline
                    onClick={selectNewBranch}
                    disabled={!newBranchInput.trim()}
                  >
                    Go
                  </Button>
                </div>
              </div>
            </div>
          )}
        </DialogBody>
      ) : (
        <DialogBody>
          {/* Step 2: Agent config, workspace, prompt */}
          <div className="mb-4">
            <button
              type="button"
              onClick={goBackToStep1}
              className="text-sm text-zinc-400 hover:text-zinc-200 transition-colors flex items-center gap-1"
            >
              <svg className="size-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M15 19l-7-7 7-7" />
              </svg>
              Back
            </button>
            <p className="text-sm text-zinc-300 mt-2">
              Branch: <span className="font-mono text-white">{selectionLabel}</span>
            </p>
          </div>

          <div className="space-y-6">
            {/* Agent config */}
            {agents.length > 0 ? (
              <Field>
                <Label>Agent configuration</Label>
                <Select value={selectedAgent} onChange={(e) => setSelectedAgent(e.target.value)}>
                  {agents.map((name) => (
                    <option key={name} value={name}>
                      {name.charAt(0).toUpperCase() + name.slice(1)}
                    </option>
                  ))}
                </Select>
              </Field>
            ) : (
              <div className="rounded-lg border border-amber-500/20 bg-amber-500/5 px-4 py-3">
                <p className="text-sm text-amber-300">
                  No agent configurations found. The agent will use default settings.
                  Add <code className="text-amber-200">.botster/agents/</code> configs to customize.
                </p>
              </div>
            )}

            {/* Workspace */}
            <WorkspacePicker
              workspaces={workspaces}
              value={workspaceChoice}
              onChange={setWorkspaceChoice}
              description="Group this agent with others in a workspace, or leave as Default."
            />

            {/* Initial prompt */}
            <Field>
              <Label>Initial prompt</Label>
              <Input
                autoFocus
                value={promptInput}
                onChange={(e) => setPromptInput(e.target.value)}
                placeholder="Optional: what should the agent work on?"
              />
              <Description>The agent will receive this as its first instruction.</Description>
            </Field>
          </div>
        </DialogBody>
      )}

      <DialogActions>
        <Button plain onClick={close}>
          Cancel
        </Button>
        {step === 2 && (
          <Button color="indigo" onClick={handleSubmit}>
            Create Agent
          </Button>
        )}
      </DialogActions>
    </Dialog>
  )
}
