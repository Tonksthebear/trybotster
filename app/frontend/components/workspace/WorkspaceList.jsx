import React from 'react'
import { useWorkspaceStore } from '../../store/workspace-store'
import { useDialogStore } from '../../store/dialog-store'
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
    return <EmptyState density={density} onNewSession={openNewSession} disabled={!connected} />
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
      <NewSessionButton density={density} onClick={openNewSession} disabled={!connected} />
    </div>
  )
}

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
        <svg className="size-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 4v16m8-8H4" />
        </svg>
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
      <svg className="size-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 4v16m8-8H4" />
      </svg>
      {disabled ? 'Connecting...' : 'New session'}
    </button>
  )
}

function EmptyState({ density, onNewSession, disabled }) {
  if (density === 'sidebar') {
    return (
      <div className="px-2 pb-2">
        <p className="px-2 py-4 text-center text-xs text-zinc-600">
          No sessions running
        </p>
        <NewSessionButton density={density} onClick={onNewSession} disabled={disabled} />
      </div>
    )
  }

  return (
    <div className="py-8 text-center">
      <svg
        className="size-12 text-zinc-700 mx-auto mb-4"
        fill="none"
        stroke="currentColor"
        viewBox="0 0 24 24"
      >
        <path
          strokeLinecap="round"
          strokeLinejoin="round"
          strokeWidth={1.5}
          d="M9.813 15.904L9 18.75l-.813-2.846a4.5 4.5 0 00-3.09-3.09L2.25 12l2.846-.813a4.5 4.5 0 003.09-3.09L9 5.25l.813 2.846a4.5 4.5 0 003.09 3.09L15.75 12l-2.846.813a4.5 4.5 0 00-3.09 3.09zM18.259 8.715L18 9.75l-.259-1.035a3.375 3.375 0 00-2.455-2.456L14.25 6l1.036-.259a3.375 3.375 0 002.455-2.456L18 2.25l.259 1.035a3.375 3.375 0 002.455 2.456L21.75 6l-1.036.259a3.375 3.375 0 00-2.455 2.456zM16.894 20.567L16.5 21.75l-.394-1.183a2.25 2.25 0 00-1.423-1.423L13.5 18.75l1.183-.394a2.25 2.25 0 001.423-1.423l.394-1.183.394 1.183a2.25 2.25 0 001.423 1.423l1.183.394-1.183.394a2.25 2.25 0 00-1.423 1.423z"
        />
      </svg>
      <h3 className="text-lg font-medium text-zinc-300 mb-2">
        No sessions running
      </h3>
      <p className="text-sm text-zinc-500 mb-4">
        Start a new agent or accessory to begin working
      </p>
      <NewSessionButton density={density} onClick={onNewSession} disabled={disabled} />
    </div>
  )
}
