import React from 'react'
import { useWorkspaceStore } from '../../store/workspace-store'
import { useDialogStore } from '../../store/dialog-store'
import { UiTree, createRawDispatch } from '../../ui_contract'
import { IconGlyph } from '../../ui_contract/icons'
import WorkspaceGroup from './WorkspaceGroup'
import SessionRow from './SessionRow'

export default function WorkspaceList({ hubId, surface }) {
  const workspaceOrder = useWorkspaceStore((s) => s.workspaceOrder)
  const ungroupedSessionIds = useWorkspaceStore((s) => s.ungroupedSessionIds)
  const sessionCount = useWorkspaceStore((s) => s.sessionOrder.length)
  const connected = useWorkspaceStore((s) => s.connected)
  const openNewSession = useDialogStore((s) => s.openNewSession)
  const density = surface === 'sidebar' ? 'sidebar' : 'panel'

  if (sessionCount === 0) {
    return (
      <EmptyState
        density={density}
        onNewSession={openNewSession}
        disabled={!connected}
      />
    )
  }

  return (
    <div className={density === 'sidebar' ? 'space-y-0.5' : 'space-y-2'}>
      {workspaceOrder.map((wsId) => (
        <WorkspaceGroup
          key={wsId}
          workspaceId={wsId}
          hubId={hubId}
          surface={surface}
        />
      ))}
      {ungroupedSessionIds.map((sessionId) => (
        <SessionRow
          key={sessionId}
          sessionId={sessionId}
          hubId={hubId}
          surface={surface}
        />
      ))}
      <NewSessionButton
        density={density}
        onClick={openNewSession}
        disabled={!connected}
      />
    </div>
  )
}

// EmptyState uses manual Stack + Text + Button composition (not the
// empty_state primitive) because v1 EmptyStatePropsV1 has no label field
// on primaryAction. See note in registry.tsx renderEmptyState. The NewSession
// button below uses a labeled Button primitive so the "New session" text is
// author-controlled.
function EmptyState({ density, onNewSession, disabled }) {
  const isSidebar = density === 'sidebar'
  const tree = {
    type: 'stack',
    props: {
      direction: 'vertical',
      gap: '3',
      align: 'center',
    },
    children: [
      {
        type: 'icon',
        props: { name: 'sparkle', size: 'md', tone: 'muted' },
      },
      {
        type: 'text',
        props: {
          text: 'No sessions running',
          size: isSidebar ? 'sm' : 'md',
          weight: 'medium',
          tone: 'muted',
        },
      },
      ...(!isSidebar
        ? [
            {
              type: 'text',
              props: {
                text: 'Start a new agent or accessory to begin working',
                size: 'sm',
                tone: 'muted',
              },
            },
          ]
        : []),
    ],
  }

  return (
    <div className={isSidebar ? 'px-2 pb-2 pt-2' : 'py-8'}>
      <div className="flex flex-col items-center text-center">
        <UiTree node={tree} dispatch={createRawDispatch(() => {})} />
        <div className="mt-4 w-full">
          <NewSessionButton
            density={density}
            onClick={onNewSession}
            disabled={disabled}
          />
        </div>
      </div>
    </div>
  )
}

// The NewSession button has a hub-specific Rails modal binding (`commandfor`)
// and test-id that the registry Button primitive doesn't model. Keeping the
// button as JSX preserves those bindings without leaking a Rails-specific
// `commandfor` prop into the shared primitive contract.
function NewSessionButton({ density, onClick, disabled }) {
  if (density === 'sidebar') {
    return (
      <button
        type="button"
        onClick={onClick}
        disabled={disabled}
        data-testid="new-session-button"
        commandfor="new-session-chooser-modal"
        className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-xs text-zinc-500 hover:text-zinc-300 hover:bg-zinc-800 transition-colors disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:text-zinc-500 disabled:hover:bg-transparent"
      >
        <span className="inline-flex size-3.5 items-center justify-center">
          <IconGlyph name="plus" className="h-full w-full" />
        </span>
        {disabled ? 'Connecting...' : 'New session'}
      </button>
    )
  }

  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      data-testid="new-session-button"
      commandfor="new-session-chooser-modal"
      className="flex w-full items-center justify-center gap-2 rounded-lg border border-dashed border-zinc-700 py-3 text-sm text-zinc-500 hover:text-zinc-300 hover:border-zinc-500 transition-colors disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:text-zinc-500 disabled:hover:border-zinc-700"
    >
      <span className="inline-flex size-4 items-center justify-center">
        <IconGlyph name="plus" className="h-full w-full" />
      </span>
      {disabled ? 'Connecting...' : 'New session'}
    </button>
  )
}
