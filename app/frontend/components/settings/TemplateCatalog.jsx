import React, { useState } from 'react'
import clsx from 'clsx'
import { useSettingsStore } from '../../store/settings-store'
import { pluginName } from '../../lib/settings-store-helpers'
import { Button } from '../catalyst/button'
import { Badge } from '../catalyst/badge'
import { Select } from '../catalyst/select'
import { Text } from '../catalyst/text'
import { templateCategories } from '../../store/selectors/settings-selectors'

// ─── Install Badge ─────────────────────────────────────────────────

function InstallBadge({ template }) {
  const templateScope = useSettingsStore((s) => s.templateScope)
  const installedDevice = useSettingsStore((s) => s.installedDevice)
  const installedRepo = useSettingsStore((s) => s.installedRepo)

  const state = templateInstallState(
    template,
    templateScope === 'repo' ? installedRepo : installedDevice
  )

  if (state.installed === 0) {
    return (
      <Badge
        color="zinc"
        className="shrink-0 text-[10px]"
        data-hub-templates-target="badge"
        data-badge-for={template.slug}
      >
        available
      </Badge>
    )
  }

  return (
    <Badge color={state.partial ? 'amber' : 'emerald'} className="shrink-0 text-[10px]">
      <span data-hub-templates-target="badge" data-badge-for={template.slug}>
        {state.partial ? `partial ${state.installed}/${state.total}` : 'installed'}
      </span>
    </Badge>
  )
}

// ─── Install Button ────────────────────────────────────────────────

function InstallButton({ template }) {
  const templateScope = useSettingsStore((s) => s.templateScope)
  const installedDevice = useSettingsStore((s) => s.installedDevice)
  const installedRepo = useSettingsStore((s) => s.installedRepo)
  const selectedTargetId = useSettingsStore((s) => s.selectedTargetId)
  const installTemplate = useSettingsStore((s) => s.installTemplate)
  const uninstallTemplate = useSettingsStore((s) => s.uninstallTemplate)

  const [busy, setBusy] = useState(false)

  const scope = templateScope
  const installState = templateInstallState(
    template,
    scope === 'repo' ? installedRepo : installedDevice
  )
  const isInstalled = installState.complete
  const isPartial = installState.partial
  const targetId = scope === 'repo' ? selectedTargetId : undefined
  const repoSelectable = scope !== 'repo' || Boolean(selectedTargetId)

  async function handleClick() {
    if (scope === 'repo' && !selectedTargetId) return

    setBusy(true)
    const files = template.files || [template]
    if (isInstalled) {
      for (const file of files.slice().reverse()) {
        await uninstallTemplate(file.dest, scope, targetId)
      }
    } else {
      for (const file of files) {
        await installTemplate(file.dest, file.content, scope, targetId)
      }
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
        : isPartial
          ? 'Repair'
        : 'Install'

  return (
    <Button
      color={isInstalled ? 'red' : 'emerald'}
      disabled={busy || (scope === 'repo' && !repoSelectable)}
      onClick={handleClick}
      data-hub-templates-target="installBtn"
      data-slug={template.slug}
      className="shrink-0 !text-sm !px-4 !py-2"
    >
      {label}
    </Button>
  )
}

// ─── Reload Button ─────────────────────────────────────────────────

function ReloadButton({ template }) {
  const templateScope = useSettingsStore((s) => s.templateScope)
  const reloadPlugin = useSettingsStore((s) => s.reloadPlugin)
  const selectedTargetId = useSettingsStore((s) => s.selectedTargetId)
  const installedDevice = useSettingsStore((s) => s.installedDevice)
  const installedRepo = useSettingsStore((s) => s.installedRepo)

  const [label, setLabel] = useState('Reload')

  const name = pluginName(template.dest)
  if (!name) return null
  const isInstalled = templateInstallState(
    template,
    templateScope === 'repo' ? installedRepo : installedDevice
  ).installed > 0
  if (!isInstalled) return null

  async function handleReload() {
    setLabel('Reloading...')
    try {
      await reloadPlugin(name, templateScope === 'repo' ? selectedTargetId : undefined)
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
          data-action="hub-templates#backToCatalog"
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
          <InstallButton template={template} />
          <ReloadButton template={template} />
        </div>
      </div>

      {(template.files || [template]).map((file) => (
        <div
          key={file.dest}
          className="mb-4 bg-zinc-900/50 border border-zinc-800 rounded-lg overflow-hidden"
        >
          <div className="px-4 py-2 border-b border-zinc-800">
            <span className="text-xs text-zinc-500 font-mono">
              {file.dest}
            </span>
          </div>
          <pre className="p-4 overflow-x-auto text-sm font-mono text-zinc-300 max-h-[500px] overflow-y-auto">
            <code>{file.content}</code>
          </pre>
        </div>
      ))}
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
      data-hub-templates-target="card"
      data-dest={template.dest}
      className="w-full text-left px-3 py-3 rounded-lg border border-zinc-700/50 hover:border-zinc-600 hover:bg-zinc-800/50 transition-colors"
    >
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="text-sm font-medium text-zinc-200">
            {template.name}
          </div>
          <div className="text-xs text-zinc-500 mt-0.5 truncate">
            {template.description}
            {template.files?.length > 1 ? ` (${template.files.length} files)` : ''}
          </div>
        </div>
        <InstallBadge template={template} />
      </div>
    </button>
  )
}

export function displayTemplatesFor(categoryTemplates) {
  const out = []
  const groups = new Map()

  for (const template of categoryTemplates) {
    const match = template.dest?.match(/^(plugins|agents|accessories)\/([^/]+)\//)
    if (!match) {
      out.push(template)
      continue
    }
    const [, kind, name] = match
    const key = `${kind}/${name}`

    const existing = groups.get(key)
    if (existing) {
      existing.files.push(template)
    } else {
      const group = {
        ...template,
        slug: `${kind}-${name}`,
        groupKind: kind,
        groupName: name,
        files: [template],
      }
      groups.set(key, group)
      out.push(group)
    }
  }

  for (const group of groups.values()) {
    group.files.sort((a, b) => {
      if (a.dest.endsWith('/init.lua')) return -1
      if (b.dest.endsWith('/init.lua')) return 1
      if (a.dest.endsWith('/initialization')) return -1
      if (b.dest.endsWith('/initialization')) return 1
      return a.dest.localeCompare(b.dest)
    })
    const entry = group.files.find((file) =>
      file.dest.endsWith('/init.lua') || file.dest.endsWith('/initialization')
    )
    if (entry) {
      group.dest = entry.dest
      group.content = entry.content
      group.scope = entry.scope
    }
  }

  return out
}

export function templateFiles(template) {
  return template.files || [template]
}

export function templateInstallState(template, installedSet) {
  const files = templateFiles(template)
  const installed = files.filter((file) => installedSet.has(file.dest)).length
  return {
    total: files.length,
    installed,
    complete: installed === files.length && files.length > 0,
    partial: installed > 0 && installed < files.length,
  }
}

export function splitTemplatesByInstallState(templates, installedSet) {
  const installed = []
  const available = []

  for (const template of templates) {
    const state = templateInstallState(template, installedSet)
    if (state.installed > 0) installed.push(template)
    else available.push(template)
  }

  return { installed, available }
}

// ─── Target Selector ───────────────────────────────────────────────

function TemplateTargetSelector() {
  const templateScope = useSettingsStore((s) => s.templateScope)
  const spawnTargets = useSettingsStore((s) => s.spawnTargets)
  const selectedTargetId = useSettingsStore((s) => s.selectedTargetId)
  const setSelectedTargetId = useSettingsStore((s) => s.setSelectedTargetId)

  if (templateScope !== 'repo') return null

  function handleChange(e) {
    setSelectedTargetId(e.target.value)
    useSettingsStore.getState().checkInstalled()
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
        data-testid="template-target-select"
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

function TemplateScopeSelector() {
  const templateScope = useSettingsStore((s) => s.templateScope)
  const setTemplateScope = useSettingsStore((s) => s.setTemplateScope)

  return (
    <div className="mb-4">
      <label className="block text-[11px] font-medium uppercase tracking-wider text-zinc-500 mb-2">
        Browse Scope
      </label>
      <div className="inline-flex rounded-lg bg-zinc-900/70 border border-zinc-800 p-1">
        {[
          ['device', 'Device'],
          ['repo', 'Repository'],
        ].map(([scope, label]) => (
          <button
            key={scope}
            type="button"
            onClick={() => setTemplateScope(scope)}
            className={clsx(
              'px-3 py-1.5 text-sm font-medium rounded-md transition-colors',
              templateScope === scope
                ? 'bg-zinc-700 text-zinc-100'
                : 'text-zinc-500 hover:text-zinc-300'
            )}
          >
            {label}
          </button>
        ))}
      </div>
    </div>
  )
}

function TemplateScopeSection({ title, templates }) {
  if (templates.length === 0) return null

  return (
    <div className="mb-5">
      <div className="flex items-center justify-between mb-2">
        <h4 className="text-[11px] font-medium text-zinc-500 uppercase tracking-wider">
          {title}
        </h4>
        <span className="text-[10px] text-zinc-600">{templates.length}</span>
      </div>
      <div className="space-y-2">
        {templates.map((template) => (
          <CatalogCard key={template.slug} template={template} />
        ))}
      </div>
    </div>
  )
}

// ─── Main Component ────────────────────────────────────────────────

export default function TemplateCatalog({ templates }) {
  const previewSlug = useSettingsStore((s) => s.previewSlug)
  const templateFeedback = useSettingsStore((s) => s.templateFeedback)
  const installedStateLoaded = useSettingsStore((s) => s.installedStateLoaded)
  const templateScope = useSettingsStore((s) => s.templateScope)
  const installedDevice = useSettingsStore((s) => s.installedDevice)
  const installedRepo = useSettingsStore((s) => s.installedRepo)

  if (!templates || Object.keys(templates).length === 0) {
    return (
      <div className="max-w-4xl mx-auto px-4 py-6 lg:py-8">
        <Text className="text-center py-8">No templates available</Text>
      </div>
    )
  }

  const allTemplates = templateCategories(templates).flatMap((category) =>
    displayTemplatesFor(templates[category])
  )
  const previewTemplate = previewSlug
    ? allTemplates.find((t) => t.slug === previewSlug)
    : null
  const installedSet = templateScope === 'repo' ? installedRepo : installedDevice
  const scopeLabel = templateScope === 'repo' ? 'Repository' : 'Device'

  return (
    <div
      className="max-w-4xl mx-auto px-4 py-6 lg:py-8"
      data-hub-templates-target="catalog"
      data-hub-templates-ready={installedStateLoaded ? '' : undefined}
    >
      <TemplateScopeSelector />
      <TemplateTargetSelector />

      {previewTemplate ? (
        <TemplatePreview template={previewTemplate} />
      ) : (
        <div>
          {templateCategories(templates)
            .map((category) => {
              const grouped = displayTemplatesFor(templates[category])
              const byState = splitTemplatesByInstallState(grouped, installedSet)
              return (
                <div key={category} className="mb-8">
                  <h3 className="text-xs font-medium text-zinc-500 uppercase tracking-wider mb-3">
                    {category}
                  </h3>
                  <TemplateScopeSection
                    title={`Installed in ${scopeLabel}`}
                    templates={byState.installed}
                  />
                  <TemplateScopeSection
                    title={`Available for ${scopeLabel}`}
                    templates={byState.available}
                  />
                </div>
              )
            })}
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
