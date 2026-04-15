import React from 'react'
import { useNavigate } from 'react-router-dom'
import {
  Dropdown,
  DropdownButton,
  DropdownMenu,
  DropdownItem,
  DropdownLabel,
  DropdownDivider,
} from '../catalyst/dropdown'
import { SidebarItem, SidebarLabel } from '../catalyst/sidebar'
import { useHubStore } from '../../store/hub-store'

const STATE_DOT = {
  connected: 'bg-emerald-500',
  connecting: 'bg-amber-500 animate-pulse',
  disconnected: 'bg-zinc-500',
  error: 'bg-red-500',
  pairing_needed: 'bg-amber-500',
}

function HubIcon() {
  return (
    <svg data-slot="icon" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth={2}
        d="M21.75 17.25v-.228a4.5 4.5 0 00-.12-1.03l-2.268-9.64a3.375 3.375 0 00-3.285-2.602H7.923a3.375 3.375 0 00-3.285 2.602l-2.268 9.64a4.5 4.5 0 00-.12 1.03v.228m19.5 0a3 3 0 01-3 3H5.25a3 3 0 01-3-3m19.5 0a3 3 0 00-3-3H5.25a3 3 0 00-3 3m16.5 0h.008v.008h-.008v-.008zm-3 0h.008v.008h-.008v-.008z"
      />
    </svg>
  )
}

function ChevronDown() {
  return (
    <svg
      data-slot="icon"
      className="size-4 text-zinc-500 dark:text-zinc-400"
      fill="none"
      stroke="currentColor"
      viewBox="0 0 24 24"
    >
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19.5 8.25l-7.5 7.5-7.5-7.5" />
    </svg>
  )
}

export default function HubSwitcher() {
  const navigate = useNavigate()
  const hubList = useHubStore((s) => s.hubList)
  const selectedHubId = useHubStore((s) => s.selectedHubId)
  const connectionState = useHubStore((s) => s.connectionState)
  const selectHub = useHubStore((s) => s.selectHub)

  const selectedHub = hubList.find((h) => String(h.id) === String(selectedHubId))
  const dotClass = STATE_DOT[connectionState] || STATE_DOT.disconnected

  return (
    <Dropdown>
      <DropdownButton as={SidebarItem} aria-label="Switch hub">
        <HubIcon />
        <SidebarLabel className="flex items-center gap-2">
          <span className="truncate">
            {selectedHub ? (selectedHub.name || selectedHub.identifier) : 'Select a hub'}
          </span>
          <span className={`size-2 shrink-0 rounded-full ${dotClass}`} />
        </SidebarLabel>
        <ChevronDown />
      </DropdownButton>

      <DropdownMenu anchor="bottom start" className="min-w-64">
        {hubList.length === 0 ? (
          <div className="px-3.5 py-2.5 text-sm text-zinc-500 dark:text-zinc-400">
            No hubs available
          </div>
        ) : (
          hubList.map((hub) => {
            const isSelected = String(hub.id) === String(selectedHubId)
            return (
              <DropdownItem
                key={hub.id}
                onClick={() => {
                  if (!isSelected) {
                    selectHub(hub.id)
                    navigate(`/hubs/${hub.id}`)
                  }
                }}
              >
                <HubItemIcon active={hub.active} />
                <DropdownLabel>{hub.name || hub.identifier}</DropdownLabel>
                {isSelected && (
                  <svg
                    className="ml-auto size-4 text-emerald-500"
                    viewBox="0 0 16 16"
                    fill="currentColor"
                  >
                    <path
                      fillRule="evenodd"
                      d="M12.416 3.376a.75.75 0 01.208 1.04l-5 7.5a.75.75 0 01-1.154.114l-3-3a.75.75 0 011.06-1.06l2.353 2.353 4.493-6.74a.75.75 0 011.04-.207z"
                      clipRule="evenodd"
                    />
                  </svg>
                )}
              </DropdownItem>
            )
          })
        )}
        <DropdownDivider />
        <DropdownItem href="/users/hubs/new">
          <PlusIcon />
          <DropdownLabel>Connect new hub</DropdownLabel>
        </DropdownItem>
      </DropdownMenu>
    </Dropdown>
  )
}

function HubItemIcon({ active }) {
  return (
    <span data-slot="icon" className="relative">
      <svg className="size-5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth={2}
          d="M21.75 17.25v-.228a4.5 4.5 0 00-.12-1.03l-2.268-9.64a3.375 3.375 0 00-3.285-2.602H7.923a3.375 3.375 0 00-3.285 2.602l-2.268 9.64a4.5 4.5 0 00-.12 1.03v.228m19.5 0a3 3 0 01-3 3H5.25a3 3 0 01-3-3m19.5 0a3 3 0 00-3-3H5.25a3 3 0 00-3 3"
        />
      </svg>
      <span
        className={`absolute -bottom-0.5 -right-0.5 size-2 rounded-full ring-2 ring-white dark:ring-zinc-900 ${
          active ? 'bg-emerald-500' : 'bg-zinc-500'
        }`}
      />
    </span>
  )
}

function PlusIcon() {
  return (
    <svg data-slot="icon" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 4v16m8-8H4" />
    </svg>
  )
}
