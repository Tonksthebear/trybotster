import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react'
import { Field, Label, Description } from '../catalyst/fieldset'
import { Input } from '../catalyst/input'
import { Button } from '../catalyst/button'
import {
  Dialog,
  DialogTitle,
  DialogDescription,
  DialogBody,
  DialogActions,
} from '../catalyst/dialog'
import { waitForHub } from '../../lib/hub-bridge'
import { useSpawnTargetStore } from '../../store/entities'

function normalizePath(path) {
  if (!path) return ''
  if (path === '/') return path
  return path.replace(/\/+$/, '')
}

function defaultNameFromPath(path) {
  if (!path) return 'Untitled target'
  return path.split('/').filter(Boolean).pop() || path
}

function buildCapabilities(target) {
  const caps = []
  caps.push(target?.is_git_repo ? 'Git: ready' : 'Git: not detected')
  caps.push(target?.has_botster_dir ? '.botster: present' : '.botster: not detected')
  if (target?.current_branch) caps.push(`Branch: ${target.current_branch}`)
  return caps
}

function normalizeTarget(target, index) {
  const path = normalizePath(target?.path || '')
  return {
    id: target?.id || `target:${index}`,
    name: target?.name || defaultNameFromPath(path),
    path,
    draft: Boolean(target?.draft),
    enabled: target?.enabled !== false,
    statusLabel: target?.statusLabel || (target?.enabled === false ? 'Disabled' : 'Admitted'),
    statusTone: target?.statusTone || (target?.enabled === false ? 'disabled' : 'live'),
    capabilities: Array.isArray(target?.capabilities) && target.capabilities.length > 0
      ? target.capabilities
      : buildCapabilities(target),
  }
}

function StatusBadge({ target }) {
  const toneClass = {
    draft: 'border-amber-500/20 bg-amber-500/10 text-amber-300',
    live: 'border-emerald-500/20 bg-emerald-500/10 text-emerald-300',
    disabled: 'border-zinc-700 bg-zinc-800 text-zinc-400',
  }[target.statusTone] || 'border-zinc-700 bg-zinc-800 text-zinc-400'

  return (
    <span className={`inline-flex items-center rounded-full border px-2.5 py-1 text-[11px] font-medium ${toneClass}`}>
      {target.statusLabel}
    </span>
  )
}

export default function SpawnTargetBrowser({ hubId, homePath = '/' }) {
  const targetOrder = useSpawnTargetStore((state) => state.order)
  const targetsById = useSpawnTargetStore((state) => state.byId)
  const targets = useMemo(
    () => targetOrder.map((id) => targetsById[id]).filter(Boolean).map(normalizeTarget),
    [targetOrder, targetsById],
  )
  const [selectedTargetId, setSelectedTargetId] = useState('')
  const [filterQuery, setFilterQuery] = useState('')
  const [pathInput, setPathInput] = useState('')
  const [nameInput, setNameInput] = useState('')
  const [pathSuggestions, setPathSuggestions] = useState([])
  const [feedback, setFeedback] = useState({ message: '', tone: 'neutral' })

  const [renameTargetId, setRenameTargetId] = useState(null)
  const [renameValue, setRenameValue] = useState('')

  const browseTokenRef = useRef(0)
  const browseTimerRef = useRef(null)

  useEffect(() => {
    if (!hubId) return

    let cancelled = false
    const unsubs = []

    function attach(hub) {
      if (cancelled) return
      hub.requestSpawnTargets?.()

      unsubs.push(
        hub.on('spawnTargetFeedback', ({ tone, message }) => {
          setFeedback({
            message,
            tone: tone === 'error' ? 'error' : tone === 'success' ? 'success' : 'neutral',
          })
        })
      )
    }

    waitForHub(hubId).then(attach)

    return () => {
      cancelled = true
      unsubs.forEach((unsub) => unsub())
      if (browseTimerRef.current) clearTimeout(browseTimerRef.current)
    }
  }, [hubId])

  useEffect(() => {
    setSelectedTargetId((prev) => {
      if (targets.some((target) => target.id === prev)) return prev
      return targets[0]?.id || ''
    })
  }, [targets])

  // Path browsing with debounce
  const handlePathInputChange = useCallback((e) => {
    const value = e.target.value
    setPathInput(value)

    if (browseTimerRef.current) clearTimeout(browseTimerRef.current)
    browseTimerRef.current = setTimeout(() => refreshPathSuggestions(value), 120)
  }, [hubId])

  async function refreshPathSuggestions(rawInput) {
    const hub = await waitForHub(hubId)
    if (!hub) return

    const ctx = browseContext(rawInput, homePath)
    if (!ctx) {
      setPathSuggestions([])
      return
    }

    const token = ++browseTokenRef.current

    try {
      const result = await hub.browseHostDir(ctx.directory, true)
      if (token !== browseTokenRef.current) return

      const entries = Array.isArray(result.entries) ? result.entries : []
      const suggestions = entries
        .filter((e) => e?.type === 'directory')
        .filter((e) => !ctx.fragment || e.name.toLowerCase().startsWith(ctx.fragment.toLowerCase()))
        .sort((a, b) => a.name.localeCompare(b.name))
        .slice(0, 25)
        .map((e) => joinBrowsePath(ctx.directory, e.name))

      setPathSuggestions(suggestions)
    } catch {
      if (token === browseTokenRef.current) setPathSuggestions([])
    }
  }

  async function admitTarget(e) {
    e.preventDefault()
    const path = normalizePath(pathInput.trim())

    if (!path || !path.startsWith('/')) {
      setFeedback({ message: 'Enter an absolute path for the spawn target.', tone: 'error' })
      return
    }

    const hub = await waitForHub(hubId)
    if (!hub) {
      setFeedback({ message: 'Hub is not ready yet.', tone: 'error' })
      return
    }

    const name = nameInput.trim() || defaultNameFromPath(path)
    setFeedback({ message: `Admitting ${path}...`, tone: 'neutral' })
    hub.addSpawnTarget(path, name)
    setPathInput('')
    setNameInput('')
    setPathSuggestions([])
  }

  async function removeTarget(targetId) {
    const hub = await waitForHub(hubId)
    if (!hub) {
      setFeedback({ message: 'Hub is not ready yet.', tone: 'error' })
      return
    }
    setFeedback({ message: 'Removing spawn target...', tone: 'neutral' })
    hub.removeSpawnTarget(targetId)
  }

  function openRenameDialog(targetId, currentName) {
    setRenameTargetId(targetId)
    setRenameValue(currentName || '')
  }

  function closeRenameDialog() {
    setRenameTargetId(null)
    setRenameValue('')
  }

  async function confirmRename() {
    if (!renameTargetId) return
    const target = targets.find((t) => t.id === renameTargetId)
    const newName = renameValue.trim()
    if (!newName || newName === target?.name) {
      closeRenameDialog()
      return
    }

    const hub = await waitForHub(hubId)
    if (!hub) {
      setFeedback({ message: 'Hub is not ready yet.', tone: 'error' })
      closeRenameDialog()
      return
    }
    hub.renameSpawnTarget(renameTargetId, newName)
    closeRenameDialog()
  }

  const visibleTargets = targets.filter((target) => {
    if (!filterQuery) return true
    const q = filterQuery.toLowerCase()
    return [target.name, target.path].filter(Boolean).some((v) => v.toLowerCase().includes(q))
  })

  const selectedTarget = targets.find((t) => t.id === selectedTargetId) || null

  const feedbackClass =
    feedback.tone === 'error' ? 'text-red-300'
      : feedback.tone === 'success' ? 'text-emerald-300'
        : 'text-zinc-500'

  return (
    <div className="space-y-6">
      {/* Admit new target */}
      <form onSubmit={admitTarget} className="space-y-3">
        <Field>
          <Label>Path</Label>
          <Input
            value={pathInput}
            onChange={handlePathInputChange}
            placeholder="/path/to/project"
            list="spawn-target-path-suggestions"
          />
          <datalist id="spawn-target-path-suggestions">
            {pathSuggestions.map((p) => (
              <option key={p} value={p} />
            ))}
          </datalist>
        </Field>

        <Field>
          <Label>Name (optional)</Label>
          <Input
            value={nameInput}
            onChange={(e) => setNameInput(e.target.value)}
            placeholder="Display name"
          />
        </Field>

        <Button type="submit" outline>
          Admit Target
        </Button>

        {feedback.message && (
          <p className={`text-xs mt-2 min-h-4 ${feedbackClass}`}>{feedback.message}</p>
        )}
      </form>

      {/* Filter */}
      {targets.length > 3 && (
        <Field>
          <Input
            value={filterQuery}
            onChange={(e) => setFilterQuery(e.target.value)}
            placeholder="Filter targets..."
          />
        </Field>
      )}

      {/* Target list */}
      <div className="space-y-3">
        {visibleTargets.map((target) => {
          const isSelected = target.id === selectedTargetId
          return (
            <div
              key={target.id}
              className={`rounded-lg border px-4 py-3 transition-colors ${
                isSelected
                  ? 'border-indigo-500/50 bg-indigo-500/10'
                  : 'border-zinc-800 bg-zinc-950/60 hover:border-zinc-700 hover:bg-zinc-950'
              }`}
            >
              <button
                type="button"
                onClick={() => setSelectedTargetId(target.id)}
                className="w-full text-left"
              >
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0">
                    <p className="text-sm font-medium text-zinc-100 truncate">{target.name}</p>
                    <p className="text-xs text-zinc-500 mt-1 font-mono break-all">{target.path}</p>
                  </div>
                  <StatusBadge target={target} />
                </div>
                <div className="mt-3 flex flex-wrap gap-2">
                  {target.capabilities.map((label) => (
                    <span
                      key={label}
                      className="inline-flex items-center rounded-full border border-zinc-700 bg-zinc-900 px-2.5 py-1 text-[11px] text-zinc-400"
                    >
                      {label}
                    </span>
                  ))}
                </div>
              </button>

              <div className="mt-3 flex items-center justify-between gap-3">
                <p className="text-[11px] text-zinc-500">
                  {target.enabled === false ? 'Disabled target' : 'Admitted target'}
                </p>
                <div className="flex items-center gap-3">
                  <button
                    type="button"
                    onClick={() => openRenameDialog(target.id, target.name)}
                    className="text-xs text-zinc-500 hover:text-zinc-200 transition-colors"
                  >
                    Rename
                  </button>
                  <button
                    type="button"
                    onClick={() => removeTarget(target.id)}
                    className="text-xs text-zinc-500 hover:text-red-300 transition-colors"
                  >
                    Remove
                  </button>
                </div>
              </div>
            </div>
          )
        })}

        {visibleTargets.length === 0 && (
          <div className="text-center py-8 text-zinc-500 text-sm">
            No spawn targets. Admit a directory above to get started.
          </div>
        )}
      </div>

      {/* Selected summary */}
      {selectedTarget && (
        <div className="border-t border-zinc-800 pt-4">
          <p className="text-sm font-medium text-zinc-100">{selectedTarget.name}</p>
          <p className="text-xs text-zinc-500 font-mono mt-1">{selectedTarget.path}</p>
          <p className="text-xs text-zinc-500 mt-2">
            Admitted target selected. Runtime actions now require explicit target selection.
          </p>
        </div>
      )}

      <Dialog open={renameTargetId !== null} onClose={closeRenameDialog} size="sm">
        <DialogTitle>Rename spawn target</DialogTitle>
        <DialogDescription>
          Choose a new display name. The directory path stays the same.
        </DialogDescription>
        <DialogBody>
          <Field>
            <Label>Name</Label>
            <Input
              value={renameValue}
              onChange={(e) => setRenameValue(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && confirmRename()}
              autoFocus
              autoComplete="off"
              spellCheck={false}
            />
          </Field>
        </DialogBody>
        <DialogActions>
          <Button plain onClick={closeRenameDialog}>Cancel</Button>
          <Button onClick={confirmRename}>Rename</Button>
        </DialogActions>
      </Dialog>
    </div>
  )
}

function browseContext(rawInput, fallbackDirectory) {
  const raw = rawInput?.trim() || ''
  if (!raw) return { directory: fallbackDirectory, fragment: '' }
  if (!raw.startsWith('/')) return null
  if (raw === '/') return { directory: '/', fragment: '' }
  if (raw.endsWith('/')) return { directory: normalizePath(raw), fragment: '' }
  const lastSlash = raw.lastIndexOf('/')
  if (lastSlash < 0) return null
  const directory = lastSlash === 0 ? '/' : normalizePath(raw.slice(0, lastSlash))
  const fragment = raw.slice(lastSlash + 1)
  return { directory, fragment }
}

function joinBrowsePath(directory, name) {
  return directory === '/' ? `/${name}/` : `${directory}/${name}/`
}
