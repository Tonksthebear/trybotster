import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import { createPortal } from 'react-dom'
import {
  Dropdown,
  DropdownButton,
  DropdownMenu,
  DropdownItem,
  DropdownLabel,
  DropdownDivider,
} from '../catalyst/dropdown'
import { useUiActionInterceptor, useUiTreeDispatch } from '../UiTree'
import { useSessionStore } from '../../store/entities'
import { previewState } from '../../store/selectors/session-row'
import { IconGlyph } from '../../ui_contract/icons'

// SessionActionsMenu intercepts the placeholder action emitted by Phase 2a's
// `web/layout.lua:actions_menu_trigger` (`botster.session.menu.open`). Phase
// 2c keeps the Menu / MenuItem primitives non-Lua-public per the current spec
// (`docs/specs/web-ui-primitives-runtime.md:179`); instead this Rails-owned
// composite captures the action + click event and renders a Catalyst Dropdown
// anchored to the triggering button.
//
// Wiring contract:
// - The hub emits `ui.icon_button{ icon = "ellipsis-vertical",
//   action = ui.action("botster.session.menu.open", { sessionId,
//   sessionUuid }) }` on each session row.
// - This composite registers an interceptor for that action id, captures
//   the event's currentTarget, derives availability flags from the
//   workspace store (canPreview / canMove / canDelete), and opens a
//   Headless UI Menu via a programmatically-clicked invisible MenuButton
//   positioned on top of the trigger.
// - Menu items dispatch follow-up actions back through the same UiTree
//   dispatch; hub-directed actions flow over `ui_action`, while browser-only
//   actions stay local.
export default function SessionActionsMenu() {
  const dispatch = useUiTreeDispatch()
  const sessionsById = useSessionStore((s) => s.byId)
  const [openState, setOpenState] = useState(null)
  const buttonRef = useRef(null)

  const handleMenuOpen = useCallback((action, source) => {
    const element = source?.element
    if (!element) return false
    const rect = element.getBoundingClientRect()
    setOpenState({
      anchorRect: rect,
      sessionId: action.payload?.sessionId,
      sessionUuid: action.payload?.sessionUuid,
      requestId: typeof globalThis.crypto?.randomUUID === 'function'
        ? globalThis.crypto.randomUUID()
        : `${Date.now()}-${Math.random()}`,
    })
    return true
  }, [])

  useUiActionInterceptor('botster.session.menu.open', handleMenuOpen)

  // Trigger the invisible MenuButton once it's been positioned at the anchor.
  // Two passes of layout: render with anchor, then click on next tick.
  useEffect(() => {
    if (!openState) return
    const timer = window.setTimeout(() => {
      buttonRef.current?.click()
    }, 0)
    return () => window.clearTimeout(timer)
  }, [openState?.requestId])

  const close = useCallback(() => setOpenState(null), [])

  const session = openState?.sessionId
    ? sessionsById[openState.sessionId]
    : null
  const preview = useMemo(
    () => (session ? previewState(session) : null),
    [session],
  )

  if (!openState) return null

  const isAccessory = session?.session_type === 'accessory'
  const canPreview = preview?.canPreview === true
  const previewStatus = preview?.status ?? 'inactive'
  const previewUrl = preview?.url ?? null
  const previewRunning = previewStatus === 'running'
  const previewReady = previewRunning && previewUrl
  const canMove = !isAccessory
  const canDelete = true
  const hasPreviewItems = canPreview || previewReady
  const hasManageItems = canMove || canDelete

  function previewLabel() {
    if (previewRunning) return 'Disable Cloudflare preview'
    if (previewStatus === 'starting') return 'Starting\u2026'
    if (previewStatus === 'error') return 'Retry Cloudflare preview'
    return 'Enable Cloudflare preview'
  }

  function fireAction(id, payload) {
    dispatch({ id, payload })
    close()
  }

  // Position the invisible Headless UI MenuButton ON TOP of the original
  // trigger so its dropdown anchors to the visible button location.
  // Rendered into a portal so it escapes any clipping containers.
  const anchorStyle = {
    position: 'fixed',
    top: `${openState.anchorRect.top}px`,
    left: `${openState.anchorRect.left}px`,
    width: `${openState.anchorRect.width}px`,
    height: `${openState.anchorRect.height}px`,
    opacity: 0,
    pointerEvents: 'none',
    zIndex: 50,
  }

  const menu = (
    <Dropdown key={openState.requestId}>
      <DropdownButton
        ref={buttonRef}
        as="button"
        type="button"
        style={anchorStyle}
        aria-hidden="true"
        tabIndex={-1}
        data-testid="session-actions-menu-trigger"
      />
      <DropdownMenu
        anchor="bottom end"
        // Headless UI handles outside-click + Escape close natively. When the
        // menu closes (any reason), drop our anchored state so the invisible
        // trigger unmounts and we're ready for the next request.
        onClose={close}
      >
        {canPreview && (
          <DropdownItem
            onClick={() =>
              fireAction('botster.session.preview.toggle', {
                sessionUuid: openState.sessionUuid,
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
              fireAction('botster.session.preview.open', {
                sessionUuid: openState.sessionUuid,
                url: previewUrl,
              })
            }
          >
            <MenuIcon name="external-link" />
            <DropdownLabel>Open Cloudflare preview</DropdownLabel>
          </DropdownItem>
        )}

        {hasPreviewItems && hasManageItems && <DropdownDivider />}

        {canMove && (
          <DropdownItem
            onClick={() =>
              fireAction('botster.session.move.request', {
                sessionId: openState.sessionId,
                sessionUuid: openState.sessionUuid,
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
              fireAction('botster.session.delete.request', {
                sessionId: openState.sessionId,
                sessionUuid: openState.sessionUuid,
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

  if (typeof document === 'undefined') return menu
  return createPortal(menu, document.body)
}

function MenuIcon({ name, danger = false }) {
  return (
    <span
      data-slot="icon"
      className={
        danger
          ? 'inline-flex items-center justify-center text-red-400'
          : 'inline-flex items-center justify-center'
      }
    >
      <IconGlyph name={name} className="h-full w-full" />
    </span>
  )
}
