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
import { IconGlyph } from '../../ui_contract/icons'

// SessionActionsMenu uses Headless UI's Menu via the Catalyst Dropdown
// wrapper. Per the v1 primitive inventory in
// `docs/specs/web-ui-primitives-runtime.md`, Menu and MenuItem are supported
// primitives but NOT Lua-public; they're available to composites like this
// one. The open/close state machine, portal positioning, and focus trap live
// in Headless UI. Icons and action dispatch flow through the ui_contract
// registry: IconGlyph is the same SVG library the `icon` primitive uses.
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

  const iconSize = isSidebar ? 'size-3.5' : 'size-4'

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
        <span
          data-slot="icon"
          className={clsx('inline-flex items-center justify-center', iconSize)}
        >
          <IconGlyph name="ellipsis-vertical" className="h-full w-full" />
        </span>
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
            <MenuIcon name="globe" />
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
            <MenuIcon name="external-link" />
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
            <MenuIcon name="arrows-right-left" />
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
            <MenuIcon name="trash" danger />
            <DropdownLabel>Delete session</DropdownLabel>
          </DropdownItem>
        )}
      </DropdownMenu>
    </Dropdown>
  )
}

function MenuIcon({ name, danger = false }) {
  return (
    <span
      data-slot="icon"
      className={clsx(
        'inline-flex items-center justify-center',
        danger && 'text-red-400',
      )}
    >
      <IconGlyph name={name} className="h-full w-full" />
    </span>
  )
}
