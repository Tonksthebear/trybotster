import React, { useState } from 'react'
import clsx from 'clsx'
import {
  useSettingsStore,
  pluginName,
  getInstallScope,
} from '../../store/settings-store'
import { Button } from '../catalyst/button'
import { Badge } from '../catalyst/badge'
import { Select } from '../catalyst/select'
import { Text } from '../catalyst/text'

// ─── Scope Buttons ─────────────────────────────────────────────────

function ScopeButtons({ slug, defaultScope }) {
  const scopeOverrides = useSettingsStore((s) => s.scopeOverrides)
  const setScopeOverride = useSettingsStore((s) => s.setScopeOverride)

  const activeScope = scopeOverrides[slug] || defaultScope || 'device'

  return (
    <div className="flex gap-1">
      {['device', 'repo'].map((scope) => (
        <button
          key={scope}
          type="button"
          onClick={() => setScopeOverride(slug, scope)}
          className={clsx(
            'px-2 py-1 text-xs font-medium rounded transition-colors',
            activeScope === scope
              ? 'bg-zinc-700 text-zinc-200'
              : 'text-zinc-500 hover:text-zinc-300'
          )}
        >
          {scope === 'device' ? 'Device' : 'Repo'}
        </button>
      ))}
    </div>
  )
}

// ─── Install Badge ─────────────────────────────────────────────────

function InstallBadge({ dest }) {
  const installedDevice = useSettingsStore((s) => s.installedDevice)
  const installedRepo = useSettingsStore((s) => s.installedRepo)

  const name = pluginName(dest)
  const deviceInstalled = installedDevice.has(name)
  const repoInstalled = installedRepo.has(name)
  const anyInstalled = deviceInstalled || repoInstalled

  if (!anyInstalled) {
    return <Badge color="zinc" className="shrink-0 text-[10px]">available</Badge>
  }

  const scopes = []
  if (deviceInstalled) scopes.push('device')
  if (repoInstalled) scopes.push('repo')

  return (
    <Badge color="emerald" className="shrink-0 text-[10px]">
      installed ({scopes.join(', ')})
    </Badge>
  )
}

// ─── Install Button ────────────────────────────────────────────────

function InstallButton({ template }) {
  const installedDevice = useSettingsStore((s) => s.installedDevice)
  const installedRepo = useSettingsStore((s) => s.installedRepo)
  const selectedTargetId = useSettingsStore((s) => s.selectedTargetId)
  const spawnTargets = useSettingsStore((s) => s.spawnTargets)
  const scopeOverrides = useSettingsStore((s) => s.scopeOverrides)
  const installTemplate = useSettingsStore((s) => s.installTemplate)
  const uninstallTemplate = useSettingsStore((s) => s.uninstallTemplate)
  const checkInstalled = useSettingsStore((s) => s.checkInstalled)

  const [busy, setBusy] = useState(false)

  const scope = getInstallScope(
    { scopeOverrides },
    template.slug,
    template.scope
  )
  const name = pluginName(template.dest)
  const isInstalled =
    scope === 'repo'
      ? installedRepo.has(name)
      : installedDevice.has(name)
  const targetId = scope === 'repo' ? selectedTargetId : undefined
  const repoSelectable = scope !== 'repo' || Boolean(selectedTargetId)

  async function handleClick() {
    if (scope === 'repo' && !selectedTargetId) return

    setBusy(true)
    if (isInstalled) {
      await uninstallTemplate(template.dest, scope, targetId)
    } else {
      await installTemplate(template.dest, template.content, scope, targetId)
    }
    setBusy(false)
  }

  const label = busy
    ? isInstalled
      ? 'Uninstalling...'
      : 'Installing...'
    : scope === 'repo' && !repoSelectable
      ? 'Select Target'
      : isInstalled
        ? 'Uninstall'
        : 'Install'

  return (
    <Button
      color={isInstalled ? 'red' : 'emerald'}
      disabled={busy || (scope === 'repo' && !repoSelectable)}
      onClick={handleClick}
      className="shrink-0 !text-sm !px-4 !py-2"
    >
      {label}
    </Button>
  )
}

// ─── Reload Button ─────────────────────────────────────────────────

function ReloadButton({ template }) {
  const reloadPlugin = useSettingsStore((s) => s.reloadPlugin)
  const selectedTargetId = useSettingsStore((s) => s.selectedTargetId)
  const installedDevice = useSettingsStore((s) => s.installedDevice)
  const installedRepo = useSettingsStore((s) => s.installedRepo)

  const [label, setLabel] = useState('Reload')

  const name = pluginName(template.dest)
  const isInstalled = installedDevice.has(name) || installedRepo.has(name)
  if (!isInstalled) return null

  async function handleReload() {
    setLabel('Reloading...')
    try {
      await reloadPlugin(name, selectedTargetId)
      setLabel('Reloaded')
      setTimeout(() => setLabel('Reload'), 1500)
    } catch {
      setLabel('Failed')
      setTimeout(() => setLabel('Reload'), 2000)
    }
  }

  return (
    <Button plain onClick={handleReload} className="!text-xs !px-3 !py-1.5">
      {label}
    </Button>
  )
}

// ─── Template Preview ──────────────────────────────────────────────

function TemplatePreview({ template }) {
  const setPreviewSlug = useSettingsStore((s) => s.setPreviewSlug)

  return (
    <div>
      <div className="mb-4">
        <button
          type="button"
          onClick={() => setPreviewSlug(null)}
          className="text-sm text-zinc-500 hover:text-zinc-300 transition-colors flex items-center gap-1"
        >
          <svg className="size-4" fill="none" viewBox="0 0 24 24" strokeWidth={1.5} stroke="currentColor">
            <path strokeLinecap="round" strokeLinejoin="round" d="M10.5 19.5 3 12m0 0 7.5-7.5M3 12h18" />
          </svg>
          Back to catalog
        </button>
      </div>

      <div className="mb-4 flex items-start justify-between gap-4">
        <div>
          <h2 className="text-lg font-semibold text-zinc-100">
            {template.name}
          </h2>
          <Text className="mt-1">{template.description}</Text>
        </div>
        <div className="flex items-center gap-2">
          <ScopeButtons slug={template.slug} defaultScope={template.scope} />
          <InstallButton template={template} />
          <ReloadButton template={template} />
        </div>
      </div>

      <div className="bg-zinc-900/50 border border-zinc-800 rounded-lg overflow-hidden">
        <div className="px-4 py-2 border-b border-zinc-800">
          <span className="text-xs text-zinc-500 font-mono">
            {template.dest}
          </span>
        </div>
        <pre className="p-4 overflow-x-auto text-sm font-mono text-zinc-300 max-h-[500px] overflow-y-auto">
          <code>{template.content}</code>
        </pre>
      </div>
    </div>
  )
}

// ─── Catalog Card ──────────────────────────────────────────────────

function CatalogCard({ template }) {
  const setPreviewSlug = useSettingsStore((s) => s.setPreviewSlug)

  return (
    <button
      type="button"
      onClick={() => setPreviewSlug(template.slug)}
      className="w-full text-left px-3 py-3 rounded-lg border border-zinc-700/50 hover:border-zinc-600 hover:bg-zinc-800/50 transition-colors"
    >
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="text-sm font-medium text-zinc-200">
            {template.name}
          </div>
          <div className="text-xs text-zinc-500 mt-0.5 truncate">
            {template.description}
          </div>
        </div>
        <InstallBadge dest={template.dest} />
      </div>
    </button>
  )
}

// ─── Target Selector ───────────────────────────────────────────────

function TemplateTargetSelector() {
  const spawnTargets = useSettingsStore((s) => s.spawnTargets)
  const selectedTargetId = useSettingsStore((s) => s.selectedTargetId)
  const setSelectedTargetId = useSettingsStore((s) => s.setSelectedTargetId)
  const checkInstalled = useSettingsStore((s) => s.checkInstalled)

  function handleChange(e) {
    setSelectedTargetId(e.target.value)
    setTimeout(() => useSettingsStore.getState().checkInstalled(), 0)
  }

  const hint = selectedTargetId
    ? spawnTargets.find((t) => t.id === selectedTargetId)?.path || ''
    : spawnTargets.length === 0
      ? 'Repo installs are unavailable until the device has an admitted spawn target.'
      : 'Repo installs and repo template status are scoped to the selected spawn target.'

  return (
    <div className="mb-4">
      <label className="block text-[11px] font-medium uppercase tracking-wider text-zinc-500 mb-2">
        Repo Target
      </label>
      <Select
        value={selectedTargetId || ''}
        onChange={handleChange}
        disabled={spawnTargets.length === 0}
        className="max-w-md"
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

export default function TemplateCatalog({ templates }) {
  const previewSlug = useSettingsStore((s) => s.previewSlug)
  const templateFeedback = useSettingsStore((s) => s.templateFeedback)

  if (!templates || Object.keys(templates).length === 0) {
    return (
      <div className="max-w-4xl mx-auto px-4 py-6 lg:py-8">
        <Text className="text-center py-8">No templates available</Text>
      </div>
    )
  }

  // Flatten templates for preview lookup
  const allTemplates = Object.values(templates).flat()
  const previewTemplate = previewSlug
    ? allTemplates.find((t) => t.slug === previewSlug)
    : null

  return (
    <div className="max-w-4xl mx-auto px-4 py-6 lg:py-8">
      <TemplateTargetSelector />

      {previewTemplate ? (
        <TemplatePreview template={previewTemplate} />
      ) : (
        <div>
          {Object.keys(templates)
            .sort()
            .map((category) => (
              <div key={category} className="mb-6">
                <h3 className="text-xs font-medium text-zinc-500 uppercase tracking-wider mb-3">
                  {category}
                </h3>
                <div className="space-y-2">
                  {templates[category].map((template) => (
                    <CatalogCard key={template.slug} template={template} />
                  ))}
                </div>
              </div>
            ))}
        </div>
      )}

      {templateFeedback && (
        <p className="text-xs text-zinc-600 text-center mt-4">
          {templateFeedback}
        </p>
      )}
    </div>
  )
}
