import React, { useState, useEffect, useRef } from 'react'
import clsx from 'clsx'
import { useSettingsStore, isDirty } from '../../store/settings-store'
import { Button } from '../catalyst/button'
import {
  Dialog,
  DialogTitle,
  DialogDescription,
  DialogBody,
  DialogActions,
} from '../catalyst/dialog'
import { Input } from '../catalyst/input'
import { Select } from '../catalyst/select'
import { Switch } from '../catalyst/switch'
import { Badge } from '../catalyst/badge'
import { Field, Label, ErrorMessage } from '../catalyst/fieldset'
import { Text } from '../catalyst/text'
import { configTreeSections } from '../../store/selectors/settings-selectors'

// ─── Name validation ───────────────────────────────────────────────

function validateName(value) {
  if (!/^[a-z][a-z0-9_-]*$/.test(value)) {
    return 'Must start with a letter. Only lowercase letters, numbers, hyphens, or underscores.'
  }
  return null
}

function validatePath(value) {
  if (value.startsWith('/') || value.includes('..')) return 'Invalid path'
  if (!/^[a-z0-9._/-]+$/.test(value)) {
    return 'Only lowercase letters, numbers, dots, hyphens, slashes, or underscores.'
  }
  return null
}

// ─── Prompt Dialog ─────────────────────────────────────────────────

function PromptDialog({
  open,
  onClose,
  title,
  message,
  validate,
  defaultValue,
  confirmLabel = 'Create',
  onConfirm,
}) {
  const [value, setValue] = useState('')
  const [error, setError] = useState('')

  useEffect(() => {
    if (open) {
      setValue(defaultValue || '')
      setError('')
    }
  }, [open, defaultValue])

  function handleConfirm() {
    const trimmed = value.trim()
    if (!trimmed) {
      setError('Value is required.')
      return
    }
    const validationFn = validate || validateName
    const err = validationFn(trimmed)
    if (err) {
      setError(err)
      return
    }
    onConfirm(trimmed)
  }

  return (
    <Dialog open={open} onClose={onClose} size="sm">
      <DialogTitle>{title}</DialogTitle>
      <DialogDescription>{message}</DialogDescription>
      <DialogBody>
        <Field>
          <Input
            value={value}
            onChange={(e) => {
              setValue(e.target.value)
              setError('')
            }}
            onKeyDown={(e) => e.key === 'Enter' && handleConfirm()}
            autoFocus
            autoComplete="off"
            spellCheck={false}
            className="font-mono"
          />
          {error && <ErrorMessage>{error}</ErrorMessage>}
        </Field>
      </DialogBody>
      <DialogActions>
        <Button plain onClick={onClose}>
          Cancel
        </Button>
        <Button onClick={handleConfirm}>{confirmLabel}</Button>
      </DialogActions>
    </Dialog>
  )
}

// ─── Confirm Dialog ────────────────────────────────────────────────

function ConfirmDialog({ open, onClose, title, message, onConfirm }) {
  return (
    <Dialog open={open} onClose={onClose} size="sm">
      <DialogTitle>{title}</DialogTitle>
      <DialogDescription>{message}</DialogDescription>
      <DialogActions>
        <Button plain onClick={onClose}>
          Cancel
        </Button>
        <Button color="red" onClick={onConfirm}>
          Delete
        </Button>
      </DialogActions>
    </Dialog>
  )
}

// ─── File Entry ────────────────────────────────────────────────────

function FileEntry({ filePath, label, exists, selected, onSelect }) {
  return (
    <button
      type="button"
      onClick={() => onSelect(filePath)}
      data-file-path={filePath}
      className={clsx(
        'w-full text-left px-2.5 py-1.5 rounded-md border',
        'border-transparent hover:border-zinc-700 hover:bg-zinc-800/50 transition-colors',
        selected && 'bg-zinc-800/70 border-primary-500/40'
      )}
    >
      <div className="flex items-center justify-between">
        <span className="text-xs font-mono text-zinc-300 truncate">
          {label}
        </span>
        {!exists && (
          <Badge color="amber" className="shrink-0 ml-2 text-[10px]">
            missing
          </Badge>
        )}
      </div>
    </button>
  )
}

// ─── Configuration Unit ───────────────────────────────────────────

function ConfigUnit({
  name,
  kind,
  basePath,
  files,
  requiredFile,
  requiredExists = true,
  currentFilePath,
  onSelect,
  onRemove,
  children,
}) {
  const existingFiles = files?.length ? files : requiredFile ? [requiredFile] : []
  const fileEntries = existingFiles.map((file) => ({ name: file, exists: true }))
  if (
    requiredFile &&
    !requiredExists &&
    !fileEntries.some((file) => file.name === requiredFile)
  ) {
    fileEntries.unshift({ name: requiredFile, exists: false })
  }
  const existingCount = fileEntries.filter((file) => file.exists).length
  const displayName = name.charAt(0).toUpperCase() + name.slice(1)

  return (
    <div className="py-3 border-t border-zinc-800/80 first:border-t-0 group/unit">
      <div className="flex items-start justify-between gap-3 mb-2">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <h3 className="text-sm font-medium text-zinc-200 truncate">
              {displayName}
            </h3>
            <Badge color="zinc" className="shrink-0 text-[10px]">
              {kind}
            </Badge>
            {requiredFile && !requiredExists && (
              <Badge color="amber" className="shrink-0 text-[10px]">
                missing {requiredFile}
              </Badge>
            )}
          </div>
          <p className="mt-0.5 text-xs text-zinc-500">
            {existingCount} {existingCount === 1 ? 'file' : 'files'}
          </p>
        </div>
        {onRemove && (
          <button
            type="button"
            onClick={onRemove}
            className="text-zinc-700 hover:text-red-400 transition-colors opacity-0 group-hover/unit:opacity-100"
            title={`Remove ${kind}`}
          >
            <svg className="size-3.5" fill="none" viewBox="0 0 24 24" strokeWidth={1.5} stroke="currentColor">
              <path strokeLinecap="round" strokeLinejoin="round" d="m14.74 9-.346 9m-4.788 0L9.26 9m9.968-3.21c.342.052.682.107 1.022.166m-1.022-.165L18.16 19.673a2.25 2.25 0 0 1-2.244 2.077H8.084a2.25 2.25 0 0 1-2.244-2.077L4.772 5.79m14.456 0a48.108 48.108 0 0 0-3.478-.397m-12 .562c.34-.059.68-.114 1.022-.165m0 0a48.11 48.11 0 0 1 3.478-.397m7.5 0v-.916c0-1.18-.91-2.164-2.09-2.201a51.964 51.964 0 0 0-3.32 0c-1.18.037-2.09 1.022-2.09 2.201v.916m7.5 0a48.667 48.667 0 0 0-7.5 0" />
            </svg>
          </button>
        )}
      </div>

      <div className="space-y-1">
        {fileEntries.map((file) => {
          const filePath = `${basePath}/${file.name}`
          return (
            <FileEntry
              key={file.name}
              filePath={filePath}
              label={file.name}
              exists={file.exists}
              selected={currentFilePath === filePath}
              onSelect={onSelect}
            />
          )
        })}
        {children}
      </div>
    </div>
  )
}

// ─── Port Forward Toggle ───────────────────────────────────────────

function PortForwardToggle({ filePath, enabled }) {
  const togglePortForward = useSettingsStore((s) => s.togglePortForward)

  return (
    <div className="flex items-center justify-between px-2.5 py-1">
      <div className="flex items-center gap-2">
        <span className="text-xs text-zinc-500">port_forward</span>
        {enabled && (
          <span className="text-[10px] text-emerald-400/70">$PORT available</span>
        )}
      </div>
      <Switch
        color="emerald"
        checked={enabled}
        onChange={(checked) => togglePortForward(filePath, checked)}
      />
    </div>
  )
}

// ─── Named Section (Agent / Accessory) ─────────────────────────────

function NamedSection({ name, type, prefix, tree, currentFilePath, onSelect }) {
  const [confirmRemove, setConfirmRemove] = useState(false)
  const removeAgent = useSettingsStore((s) => s.removeAgent)
  const removeAccessory = useSettingsStore((s) => s.removeAccessory)

  const item = tree[type][name]
  if (!item) return null

  const itemPath = `${prefix}${type}/${name}`
  const isAgent = type === 'agents'

  async function handleRemove() {
    setConfirmRemove(false)
    if (isAgent) await removeAgent(name)
    else await removeAccessory(name)
  }

  return (
    <>
      <ConfigUnit
        name={name}
        kind={isAgent ? 'agent' : 'accessory'}
        basePath={itemPath}
        files={(item.files?.length ? item.files : ['initialization'])
          .filter((file) => type !== 'accessories' || file !== 'port_forward')}
        requiredFile="initialization"
        requiredExists={item.initialization}
        currentFilePath={currentFilePath}
        onSelect={onSelect}
        onRemove={() => setConfirmRemove(true)}
      >
        {type === 'accessories' && (
          <PortForwardToggle
            filePath={`${itemPath}/port_forward`}
            enabled={item.port_forward}
          />
        )}
      </ConfigUnit>

      <ConfirmDialog
        open={confirmRemove}
        onClose={() => setConfirmRemove(false)}
        title={`Remove ${isAgent ? 'Agent' : 'Accessory'}`}
        message={`Delete ${isAgent ? 'agent' : 'accessory'} "${name}" and its configuration? This cannot be undone.`}
        onConfirm={handleRemove}
      />
    </>
  )
}

// ─── Empty State ───────────────────────────────────────────────────

function EmptyState({ agentTemplates }) {
  const quickSetup = useSettingsStore((s) => s.quickSetup)
  const initBotster = useSettingsStore((s) => s.initBotster)
  const [installing, setInstalling] = useState(null)

  async function handleQuickSetup(dest, content) {
    setInstalling(dest)
    await quickSetup(dest, content)
    setInstalling(null)
  }

  return (
    <div className="py-4" data-hub-setup-banner-target="banner">
      <h3 className="text-sm font-medium text-zinc-300 mb-1">Get Started</h3>
      <Text className="!text-xs mb-4">
        Choose a session template to initialize your hub:
      </Text>

      {agentTemplates?.length > 0 && (
        <div className="space-y-2 mb-4">
          {agentTemplates.map((template) => (
            <button
              key={template.dest}
              type="button"
              disabled={installing === template.dest}
              onClick={() => handleQuickSetup(template.dest, template.content)}
              data-action="hub-setup-banner#quickSetup"
              className="w-full text-left px-3 py-2.5 rounded-lg border border-zinc-700/50 hover:border-primary-500/30 hover:bg-zinc-800/50 transition-colors group/card disabled:opacity-50"
            >
              <div className="flex items-center justify-between">
                <div className="min-w-0">
                  <div className="text-sm font-medium text-zinc-200 group-hover/card:text-primary-300 transition-colors">
                    {installing === template.dest ? 'Installing...' : template.name}
                  </div>
                  <div className="text-xs text-zinc-500 mt-0.5 truncate">
                    {template.description}
                  </div>
                </div>
              </div>
            </button>
          ))}
        </div>
      )}

      <button
        type="button"
        onClick={initBotster}
        data-action="hub-settings#initBotster"
        className="w-full text-center px-3 py-1.5 text-xs text-zinc-600 hover:text-zinc-400 transition-colors"
      >
        Initialize empty
      </button>
    </div>
  )
}

// ─── Tree View ─────────────────────────────────────────────────────

function TreeView({ agentTemplates }) {
  const tree = useSettingsStore((s) => s.tree)
  const configScope = useSettingsStore((s) => s.configScope)
  const currentFilePath = useSettingsStore((s) => s.currentFilePath)
  const selectFile = useSettingsStore((s) => s.selectFile)
  const addAgent = useSettingsStore((s) => s.addAgent)
  const addAccessory = useSettingsStore((s) => s.addAccessory)

  const [addAgentOpen, setAddAgentOpen] = useState(false)
  const [addAccessoryOpen, setAddAccessoryOpen] = useState(false)

  if (!tree) return null

  const prefix = configScope === 'device' ? '' : '.botster/'
  const { agentNames, accessoryNames, workspaceNames, pluginNames } = configTreeSections(tree)

  async function handleAddAgent(name) {
    setAddAgentOpen(false)
    await addAgent(name)
  }

  async function handleAddAccessory(name) {
    setAddAccessoryOpen(false)
    await addAccessory(name)
  }

  return (
    <div className="space-y-4" data-hub-settings-target="treeContainer">
      {/* Agents */}
      {agentNames.length > 0 && (
        <div className="mt-2">
          <SectionHeading title="Agents" count={agentNames.length} />
          {agentNames.map((name) => (
            <NamedSection
              key={name}
              name={name}
              type="agents"
              prefix={prefix}
              tree={tree}
              currentFilePath={currentFilePath}
              onSelect={selectFile}
            />
          ))}
        </div>
      )}

      <button
        type="button"
        onClick={() => setAddAgentOpen(true)}
        className="w-full mt-2 px-3 py-2 text-xs font-medium text-zinc-500 hover:text-zinc-300 border border-dashed border-zinc-700 hover:border-zinc-600 rounded-lg transition-colors"
      >
        + Add Agent
      </button>

      {/* Accessories */}
      {accessoryNames.length > 0 && (
        <div>
          <SectionHeading title="Accessories" count={accessoryNames.length} />
          {accessoryNames.map((name) => (
            <NamedSection
              key={name}
              name={name}
              type="accessories"
              prefix={prefix}
              tree={tree}
              currentFilePath={currentFilePath}
              onSelect={selectFile}
            />
          ))}
        </div>
      )}

      <button
        type="button"
        onClick={() => setAddAccessoryOpen(true)}
        className="w-full mt-2 px-3 py-2 text-xs font-medium text-zinc-500 hover:text-zinc-300 border border-dashed border-zinc-700 hover:border-zinc-600 rounded-lg transition-colors"
      >
        + Add Accessory
      </button>

      {/* Workspaces */}
      {workspaceNames.length > 0 && (
        <div>
          <SectionHeading title="Workspaces" count={workspaceNames.length} />
          <div className="space-y-1">
            {workspaceNames.map((name) => (
              <FileEntry
                key={name}
                filePath={`${prefix}workspaces/${name}.json`}
                label={`${name}.json`}
                exists={true}
                selected={currentFilePath === `${prefix}workspaces/${name}.json`}
                onSelect={selectFile}
              />
            ))}
          </div>
        </div>
      )}

      {/* Plugins */}
      <div>
        <SectionHeading title="Plugins" count={pluginNames.length} />
        {pluginNames.length > 0 ? (
          <div>
            {pluginNames.map((name) => (
              <ConfigUnit
                key={name}
                name={name}
                kind="plugin"
                basePath={`${prefix}plugins/${name}`}
                files={tree.plugins[name]?.files?.length
                  ? tree.plugins[name].files
                  : ['init.lua']}
                requiredFile="init.lua"
                requiredExists={tree.plugins[name]?.init}
                currentFilePath={currentFilePath}
                onSelect={selectFile}
              />
            ))}
          </div>
        ) : (
          <p className="text-xs text-zinc-600 italic">
            No plugins installed. Browse the template catalog to add one.
          </p>
        )}
      </div>

      {/* Dialogs */}
      <PromptDialog
        open={addAgentOpen}
        onClose={() => setAddAgentOpen(false)}
        title="Add Agent"
        message="Enter a name for the new agent (lowercase, no spaces):"
        onConfirm={handleAddAgent}
      />

      <PromptDialog
        open={addAccessoryOpen}
        onClose={() => setAddAccessoryOpen(false)}
        title="Add Accessory"
        message="Enter a name for the new accessory (lowercase, no spaces):"
        onConfirm={handleAddAccessory}
      />
    </div>
  )
}

function SectionHeading({ title, count }) {
  return (
    <div className="flex items-center justify-between mb-1">
      <h2 className="text-sm font-medium text-zinc-400 uppercase tracking-wider">
        {title}
      </h2>
      <span className="text-[10px] text-zinc-600">
        {count}
      </span>
    </div>
  )
}

// ─── Editor Panel ──────────────────────────────────────────────────

function EditorPanel() {
  const editorState = useSettingsStore((s) => s.editorState)
  const editorError = useSettingsStore((s) => s.editorError)
  const currentFilePath = useSettingsStore((s) => s.currentFilePath)
  const editorContent = useSettingsStore((s) => s.editorContent)
  const dirty = useSettingsStore(isDirty)
  const setEditorContent = useSettingsStore((s) => s.setEditorContent)
  const saveFile = useSettingsStore((s) => s.saveFile)
  const revertFile = useSettingsStore((s) => s.revertFile)
  const createFile = useSettingsStore((s) => s.createFile)
  const deleteFile = useSettingsStore((s) => s.deleteFile)
  const renameFile = useSettingsStore((s) => s.renameFile)

  const [saveLabel, setSaveLabel] = useState('Save')
  const [createLabel, setCreateLabel] = useState('Create')
  const [renameOpen, setRenameOpen] = useState(false)
  const [deleteOpen, setDeleteOpen] = useState(false)

  async function handleSave() {
    setSaveLabel('Saving...')
    const ok = await saveFile()
    if (ok) {
      setSaveLabel('Saved')
      setTimeout(() => setSaveLabel('Save'), 1500)
    } else {
      setSaveLabel('Save')
    }
  }

  async function handleCreate() {
    setCreateLabel('Creating...')
    await createFile()
    setCreateLabel('Create')
  }

  async function handleDelete() {
    setDeleteOpen(false)
    await deleteFile()
  }

  async function handleRename(newPath) {
    setRenameOpen(false)
    await renameFile(newPath)
  }

  return (
    <div
      className="bg-zinc-900/50 border border-zinc-800 rounded-lg"
      data-hub-settings-target="editorPanel"
      data-editor={editorState}
    >
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b border-zinc-800">
        <span
          className="text-sm font-mono text-zinc-400 min-w-0 truncate"
          data-hub-settings-target="editorTitle"
        >
          {currentFilePath || 'Select a file'}
        </span>
        <div className="flex items-center gap-2 shrink-0">
          {editorState === 'editing' && (
            <>
              <Button plain onClick={revertFile} className="!text-xs !px-3 !py-1.5">
                Revert
              </Button>
              <Button
                color="emerald"
                disabled={!dirty}
                onClick={handleSave}
                data-hub-settings-target="saveBtn"
                className="!text-xs !px-3 !py-1.5"
              >
                {saveLabel}
              </Button>
              <Button plain onClick={() => setRenameOpen(true)} className="!text-xs !px-3 !py-1.5">
                Rename
              </Button>
              <Button color="red" onClick={() => setDeleteOpen(true)} className="!text-xs !px-3 !py-1.5">
                Delete
              </Button>
            </>
          )}
          {editorState === 'creating' && (
            <Button onClick={handleCreate} className="!text-xs !px-3 !py-1.5">
              {createLabel}
            </Button>
          )}
        </div>
      </div>

      {/* Content */}
      {editorState === 'empty' && (
        <div className="py-16 text-center">
          <svg className="size-12 text-zinc-700 mx-auto mb-4" fill="none" viewBox="0 0 24 24" strokeWidth={1} stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" d="M19.5 14.25v-2.625a3.375 3.375 0 0 0-3.375-3.375h-1.5A1.125 1.125 0 0 1 13.5 7.125v-1.5a3.375 3.375 0 0 0-3.375-3.375H8.25m2.25 0H5.625c-.621 0-1.125.504-1.125 1.125v17.25c0 .621.504 1.125 1.125 1.125h12.75c.621 0 1.125-.504 1.125-1.125V11.25a9 9 0 0 0-9-9Z" />
          </svg>
          <Text>Select a config file to edit</Text>
        </div>
      )}

      {editorState === 'loading' && (
        <div className="py-16 text-center">
          <Text>Loading...</Text>
        </div>
      )}

      {(editorState === 'editing' || editorState === 'creating') && (
        <textarea
          value={editorContent}
          onChange={(e) => setEditorContent(e.target.value)}
          data-hub-settings-target="editor"
          className="w-full min-h-[400px] p-4 bg-transparent text-sm font-mono text-zinc-200 placeholder-zinc-600 border-0 focus:ring-0 focus:outline-none resize-y"
          spellCheck="false"
          placeholder="Empty file"
        />
      )}

      {editorState === 'error' && (
        <div className="py-16 text-center">
          <p className="text-sm text-red-400">{editorError}</p>
        </div>
      )}

      {/* Dialogs */}
      <ConfirmDialog
        open={deleteOpen}
        onClose={() => setDeleteOpen(false)}
        title="Delete File"
        message={`Delete ${currentFilePath}?`}
        onConfirm={handleDelete}
      />

      <PromptDialog
        open={renameOpen}
        onClose={() => setRenameOpen(false)}
        title="Rename"
        message="Enter the new path:"
        validate={validatePath}
        defaultValue={currentFilePath}
        confirmLabel="Rename"
        onConfirm={handleRename}
      />
    </div>
  )
}

// ─── Scope & Target Selectors ──────────────────────────────────────

function ScopeSelector() {
  const configScope = useSettingsStore((s) => s.configScope)
  const setConfigScope = useSettingsStore((s) => s.setConfigScope)
  const scanTree = useSettingsStore((s) => s.scanTree)

  function handleSwitch(scope) {
    if (scope === configScope) return
    setConfigScope(scope)
    // Use cached tree or trigger scan
    const { tree } = useSettingsStore.getState()
    if (!tree) scanTree()
  }

  return (
    <div className="flex gap-1 mb-3">
      {['repo', 'device'].map((scope) => (
        <button
          key={scope}
          type="button"
          onClick={() => handleSwitch(scope)}
          className={clsx(
            'flex-1 px-3 py-1.5 text-xs font-medium rounded transition-colors',
            configScope === scope
              ? 'bg-zinc-800 text-zinc-200'
              : 'text-zinc-500 hover:text-zinc-300'
          )}
        >
          {scope === 'repo' ? 'Repository' : 'Device'}
        </button>
      ))}
    </div>
  )
}

function TargetSelector() {
  const configScope = useSettingsStore((s) => s.configScope)
  const spawnTargets = useSettingsStore((s) => s.spawnTargets)
  const selectedTargetId = useSettingsStore((s) => s.selectedTargetId)
  const setSelectedTargetId = useSettingsStore((s) => s.setSelectedTargetId)
  const scanTree = useSettingsStore((s) => s.scanTree)

  if (configScope !== 'repo') return null

  function handleChange(e) {
    setSelectedTargetId(e.target.value)
    useSettingsStore.getState().scanTree()
  }

  const hint = selectedTargetId
    ? spawnTargets.find((t) => t.id === selectedTargetId)?.path || ''
    : spawnTargets.length === 0
      ? 'Add a spawn target from the device page before editing target-local .botster files.'
      : 'Target-local .botster editing is locked to the selected admitted spawn target.'

  return (
    <div className="mb-3">
      <label className="block text-[11px] font-medium uppercase tracking-wider text-zinc-500 mb-2">
        Spawn Target
      </label>
      <Select
        value={selectedTargetId || ''}
        onChange={handleChange}
        disabled={spawnTargets.length === 0}
      >
        <option value="">
          {spawnTargets.length === 0
            ? 'No admitted spawn targets'
            : 'Choose a spawn target'}
        </option>
        {spawnTargets.map((target) => (
          <option key={target.id} value={target.id}>
            {target.name || target.path || target.id}
          </option>
        ))}
      </Select>
      <p className="mt-2 text-xs text-zinc-500">{hint}</p>
    </div>
  )
}

// ─── Main Component ────────────────────────────────────────────────

export default function ConfigEditor({ agentTemplates }) {
  const treeState = useSettingsStore((s) => s.treeState)
  const treeFeedback = useSettingsStore((s) => s.treeFeedback)
  const configScope = useSettingsStore((s) => s.configScope)

  // System-test readiness signal: the scope-specific tree scan has completed.
  // Absent during 'loading'/'disconnected'; present when 'tree' (files found)
  // or 'empty' (scan done, nothing there). The `data-settings-ready-state`
  // lets helpers optionally narrow to one. See test/support/system_readiness_helpers.rb.
  const settingsReadyState =
    treeState === 'tree' || treeState === 'empty' ? treeState : null

  return (
    <div
      className="max-w-4xl mx-auto px-4 py-6 lg:py-8"
      data-settings-ready={settingsReadyState ? configScope : undefined}
      data-settings-ready-state={settingsReadyState || undefined}
    >
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
        {/* Tree Navigation (left) */}
        <div className="lg:col-span-1">
          <ScopeSelector />
          <TargetSelector />

          <div
            className="bg-zinc-900/50 border border-zinc-800 rounded-lg p-4"
            data-hub-settings-target="treePanel"
            data-view={treeState}
          >
            {(treeState === 'loading' || treeState === 'disconnected') && (
              <div className="py-4 text-center">
                <Text className="!text-sm">{treeFeedback}</Text>
              </div>
            )}

            {treeState === 'empty' && (
              <EmptyState agentTemplates={agentTemplates} />
            )}

            {treeState === 'tree' && (
              <TreeView agentTemplates={agentTemplates} />
            )}
          </div>
        </div>

        {/* Editor (right) */}
        <div className="lg:col-span-2">
          <EditorPanel />
        </div>
      </div>
    </div>
  )
}
