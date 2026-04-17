import React from 'react'
import clsx from 'clsx'
import {
  Dropdown,
  DropdownButton,
  DropdownMenu,
  DropdownItem,
  DropdownLabel,
  DropdownDivider,
} from '../catalyst/dropdown'
import { dispatch, ACTION } from '../../lib/actions'

export default function SessionActionsMenu({
  sessionId,
  sessionUuid,
  hubId,
  actionsMenu,
  isAccessory,
  density,
}) {
  const isSidebar = density === 'sidebar'
  const { canPreview, previewStatus, previewUrl, canMove, canDelete } = actionsMenu

  const previewRunning = previewStatus === 'running'
  const previewReady = previewRunning && previewUrl

  function previewLabel() {
    if (previewRunning) return 'Disable Cloudflare preview'
    if (previewStatus === 'starting') return 'Starting\u2026'
    if (previewStatus === 'error') return 'Retry Cloudflare preview'
    return 'Enable Cloudflare preview'
  }

  const hasPreviewItems = canPreview || previewReady
  const hasManageItems = (canMove && !isAccessory) || canDelete

  return (
    <Dropdown>
      <DropdownButton
        plain
        className={clsx(
          'shrink-0 transition-colors',
          isSidebar
            ? 'p-1.5 text-zinc-600 hover:text-zinc-300 opacity-0 group-hover:opacity-100 [@media(pointer:coarse)]:opacity-100'
            : 'p-3 text-zinc-600 hover:text-zinc-300'
        )}
      >
        <span className="sr-only">Open session options</span>
        <EllipsisVerticalIcon className={isSidebar ? 'size-3.5' : 'size-4'} />
      </DropdownButton>

      <DropdownMenu anchor="bottom end">
        {canPreview && (
          <DropdownItem
            onClick={() =>
              dispatch({
                action: ACTION.PREVIEW_TOGGLE,
                payload: { hubId, sessionUuid },
              })
            }
          >
            <GlobeIcon />
            <DropdownLabel>{previewLabel()}</DropdownLabel>
          </DropdownItem>
        )}

        {previewReady && (
          <DropdownItem
            onClick={() =>
              dispatch({
                action: ACTION.PREVIEW_OPEN,
                payload: { url: previewUrl },
              })
            }
          >
            <ExternalLinkIcon />
            <DropdownLabel>Open Cloudflare preview</DropdownLabel>
          </DropdownItem>
        )}

        {hasPreviewItems && hasManageItems && <DropdownDivider />}

        {canMove && !isAccessory && (
          <DropdownItem
            onClick={() =>
              dispatch({
                action: ACTION.SESSION_MOVE,
                payload: { sessionId, sessionUuid },
              })
            }
          >
            <ArrowsIcon />
            <DropdownLabel>Move to workspace</DropdownLabel>
          </DropdownItem>
        )}

        {canDelete && (
          <DropdownItem
            onClick={() =>
              dispatch({
                action: ACTION.SESSION_DELETE,
                payload: { sessionId, sessionUuid },
              })
            }
          >
            <TrashIcon />
            <DropdownLabel>Delete session</DropdownLabel>
          </DropdownItem>
        )}
      </DropdownMenu>
    </Dropdown>
  )
}

/* Inline Heroicon SVGs — mini (20×20) to match existing sidebar icons */

function EllipsisVerticalIcon({ className }) {
  return (
    <svg className={className} viewBox="0 0 20 20" fill="currentColor" data-slot="icon">
      <path d="M10 3a1.5 1.5 0 110 3 1.5 1.5 0 010-3zM10 8.5a1.5 1.5 0 110 3 1.5 1.5 0 010-3zM11.5 15.5a1.5 1.5 0 10-3 0 1.5 1.5 0 003 0z" />
    </svg>
  )
}

function GlobeIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="currentColor" data-slot="icon">
      <path
        fillRule="evenodd"
        d="M10 18a8 8 0 100-16 8 8 0 000 16zM4.332 8.027a6.012 6.012 0 011.912-2.706C6.512 5.73 6.974 6 7.5 6A1.5 1.5 0 009 7.5V8a2 2 0 004 0 2 2 0 011.523-1.943A5.977 5.977 0 0116 10c0 .34-.028.675-.083 1H15a2 2 0 00-2 2v2.197A5.973 5.973 0 0110 16v-2a2 2 0 00-2-2 2 2 0 01-2-2 2 2 0 00-1.668-1.973z"
        clipRule="evenodd"
      />
    </svg>
  )
}

function ExternalLinkIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="currentColor" data-slot="icon">
      <path
        fillRule="evenodd"
        d="M4.25 5.5a.75.75 0 00-.75.75v8.5c0 .414.336.75.75.75h8.5a.75.75 0 00.75-.75v-4a.75.75 0 011.5 0v4A2.25 2.25 0 0112.75 17h-8.5A2.25 2.25 0 012 14.75v-8.5A2.25 2.25 0 014.25 4h5a.75.75 0 010 1.5h-5zm7.25-.75a.75.75 0 01.75-.75h3.5a.75.75 0 01.75.75v3.5a.75.75 0 01-1.5 0V6.31l-5.47 5.47a.75.75 0 11-1.06-1.06l5.47-5.47H12.25a.75.75 0 01-.75-.75z"
        clipRule="evenodd"
      />
    </svg>
  )
}

function ArrowsIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="currentColor" data-slot="icon">
      <path
        fillRule="evenodd"
        d="M13.2 2.24a.75.75 0 00.04 1.06l2.1 1.95H6.75a.75.75 0 000 1.5h8.59l-2.1 1.95a.75.75 0 101.02 1.1l3.5-3.25a.75.75 0 000-1.1l-3.5-3.25a.75.75 0 00-1.06.04zm-6.4 8a.75.75 0 00-1.06-.04l-3.5 3.25a.75.75 0 000 1.1l3.5 3.25a.75.75 0 101.02-1.1l-2.1-1.95h8.59a.75.75 0 000-1.5H4.66l2.1-1.95a.75.75 0 00.04-1.06z"
        clipRule="evenodd"
      />
    </svg>
  )
}

function TrashIcon() {
  return (
    <svg viewBox="0 0 20 20" fill="currentColor" data-slot="icon" className="text-red-400">
      <path
        fillRule="evenodd"
        d="M8.75 1A2.75 2.75 0 006 3.75v.443c-.795.077-1.584.176-2.365.298a.75.75 0 10.23 1.482l.149-.022.841 10.518A2.75 2.75 0 007.596 19h4.807a2.75 2.75 0 002.742-2.53l.841-10.519.149.023a.75.75 0 00.23-1.482A41.03 41.03 0 0014 4.193V3.75A2.75 2.75 0 0011.25 1h-2.5zM10 4c.84 0 1.673.025 2.5.075V3.75c0-.69-.56-1.25-1.25-1.25h-2.5c-.69 0-1.25.56-1.25 1.25v.325C8.327 4.025 9.16 4 10 4zM8.58 7.72a.75.75 0 01.7.797l-.5 6a.75.75 0 01-1.497-.124l.5-6a.75.75 0 01.797-.672zm3.638.797a.75.75 0 10-1.497-.124l-.5 6a.75.75 0 101.497.124l.5-6z"
        clipRule="evenodd"
      />
    </svg>
  )
}
