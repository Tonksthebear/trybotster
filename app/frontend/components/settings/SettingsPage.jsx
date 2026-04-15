import React from 'react'
import { Link } from 'react-router-dom'
import clsx from 'clsx'
import { useSettingsStore } from '../../store/settings-store'
import ConnectionStatus from '../hub/ConnectionStatus'
import ConfigEditor from './ConfigEditor'
import TemplateCatalog from './TemplateCatalog'
import HubInfoPanel from './HubInfoPanel'

const TABS = [
  { id: 'config', label: 'Config' },
  { id: 'templates', label: 'Templates' },
  { id: 'hub', label: 'Hub' },
]

export default function SettingsPage({
  hubId,
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
      {/* Header */}
      <div className="shrink-0 border-b border-zinc-800 bg-zinc-900/50">
        <div className="px-4 py-4 lg:py-6">
          <div className="flex items-start justify-between gap-4">
            <div className="min-w-0 flex items-center gap-3">
              <Link
                to={hubPath}
                className="text-zinc-500 hover:text-zinc-300 transition-colors"
              >
                <svg
                  className="size-5"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M10.5 19.5L3 12m0 0l7.5-7.5M3 12h18"
                  />
                </svg>
              </Link>
              <div>
                <h1 className="text-xl lg:text-2xl font-bold text-zinc-100 font-mono">
                  Settings
                </h1>
                <p className="text-sm text-zinc-500 mt-1 truncate">
                  {hubName}
                </p>
              </div>
            </div>
            <ConnectionStatus hubId={hubId} />
          </div>
        </div>

        {/* Tab bar */}
        <div className="px-4 flex gap-1">
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
