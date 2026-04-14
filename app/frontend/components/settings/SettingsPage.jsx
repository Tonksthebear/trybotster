import React from 'react'
import clsx from 'clsx'
import { useSettingsStore } from '../../store/settings-store'
import ConfigEditor from './ConfigEditor'
import TemplateCatalog from './TemplateCatalog'
import HubInfoPanel from './HubInfoPanel'

const TABS = [
  { id: 'config', label: 'Config' },
  { id: 'templates', label: 'Templates' },
  { id: 'hub', label: 'Hub' },
]

export default function SettingsPage({
  templates,
  agentTemplates,
  hubName,
  hubIdentifier,
  hubSettingsPath,
  hubPath,
}) {
  const activeTab = useSettingsStore((s) => s.activeTab)
  const setActiveTab = useSettingsStore((s) => s.setActiveTab)

  return (
    <div className="flex flex-col h-full overflow-hidden">
      {/* Tab bar */}
      <div className="shrink-0 px-4 flex gap-1">
        {TABS.map((tab) => (
          <button
            key={tab.id}
            type="button"
            onClick={() => setActiveTab(tab.id)}
            className={clsx(
              'px-4 py-2 text-sm font-medium transition-colors border-b-2',
              activeTab === tab.id
                ? 'text-zinc-100 border-primary-500'
                : 'text-zinc-500 border-transparent hover:text-zinc-300'
            )}
          >
            {tab.label}
          </button>
        ))}
      </div>

      {/* Tab panels */}
      <div className="flex-1 overflow-y-auto">
        {activeTab === 'config' && (
          <ConfigEditor agentTemplates={agentTemplates} />
        )}
        {activeTab === 'templates' && (
          <TemplateCatalog templates={templates} />
        )}
        {activeTab === 'hub' && (
          <HubInfoPanel
            hubName={hubName}
            hubIdentifier={hubIdentifier}
            hubSettingsPath={hubSettingsPath}
            hubPath={hubPath}
          />
        )}
      </div>
    </div>
  )
}
