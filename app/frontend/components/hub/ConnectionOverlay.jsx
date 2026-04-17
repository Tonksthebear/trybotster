import React, { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { useHubStore } from '../../store/hub-store'

export default function ConnectionOverlay({ suppress = false }) {
  const connectionState = useHubStore((s) => s.connectionState)
  const connectionDetail = useHubStore((s) => s.connectionDetail)
  const selectedHubId = useHubStore((s) => s.selectedHubId)
  const hubList = useHubStore((s) => s.hubList)
  const retryConnection = useHubStore((s) => s.retryConnection)
  const navigate = useNavigate()

  const shouldHide = connectionState === 'connected' || suppress
  const [visible, setVisible] = useState(!shouldHide)
  const [fading, setFading] = useState(false)

  const selectedHub = hubList.find((h) => String(h.id) === String(selectedHubId))
  const hubName = selectedHub?.name || selectedHub?.identifier || 'Hub'

  useEffect(() => {
    if (shouldHide) {
      // Fade out then hide
      setFading(true)
      const timer = setTimeout(() => {
        setVisible(false)
        setFading(false)
      }, 300)
      return () => clearTimeout(timer)
    }
    // Should show: display immediately
    setVisible(true)
    setFading(false)
  }, [shouldHide])

  if (!selectedHubId || !visible) return null

  return (
    <div
      className={`fixed inset-0 lg:left-64 z-30 flex items-center justify-center bg-zinc-950/80 backdrop-blur-sm transition-opacity duration-300 ${
        fading ? 'opacity-0' : 'opacity-100'
      }`}
    >
      <div className="max-w-sm w-full mx-4 text-center">
        {connectionState === 'connecting' && (
          <ConnectingContent hubName={hubName} detail={connectionDetail} />
        )}
        {connectionState === 'pairing_needed' && (
          <PairingContent hubName={hubName} hubId={selectedHubId} onNavigate={navigate} />
        )}
        {connectionState === 'error' && (
          <ErrorContent hubName={hubName} detail={connectionDetail} onRetry={retryConnection} />
        )}
        {connectionState === 'disconnected' && (
          <DisconnectedContent hubName={hubName} detail={connectionDetail} onRetry={retryConnection} />
        )}
      </div>
    </div>
  )
}

function ConnectingContent({ hubName, detail }) {
  return (
    <>
      <Spinner />
      <h2 className="mt-4 text-lg font-semibold text-zinc-100">{hubName}</h2>
      <p className="mt-2 text-sm text-zinc-400">{detail || 'Connecting...'}</p>
    </>
  )
}

function PairingContent({ hubName, hubId, onNavigate }) {
  return (
    <>
      <QrIcon />
      <h2 className="mt-4 text-lg font-semibold text-zinc-100">{hubName}</h2>
      <p className="mt-2 text-sm text-zinc-400">
        This device needs to be paired with your hub.
      </p>
      <button
        type="button"
        onClick={() => onNavigate(`/hubs/${hubId}/pairing`)}
        className="mt-4 inline-flex items-center gap-2 rounded-lg bg-primary-600 px-4 py-2 text-sm font-medium text-white hover:bg-primary-500 transition-colors"
      >
        Start pairing
      </button>
    </>
  )
}

function ErrorContent({ hubName, detail, onRetry }) {
  return (
    <>
      <ErrorIcon />
      <h2 className="mt-4 text-lg font-semibold text-zinc-100">{hubName}</h2>
      <p className="mt-2 text-sm text-red-400">{detail || 'Connection error'}</p>
      <button
        type="button"
        onClick={onRetry}
        className="mt-4 inline-flex items-center gap-2 rounded-lg bg-zinc-800 px-4 py-2 text-sm font-medium text-zinc-200 hover:bg-zinc-700 border border-zinc-700 transition-colors"
      >
        <RetryIcon />
        Retry connection
      </button>
    </>
  )
}

function DisconnectedContent({ hubName, detail, onRetry }) {
  return (
    <>
      <OfflineIcon />
      <h2 className="mt-4 text-lg font-semibold text-zinc-100">{hubName}</h2>
      <p className="mt-2 text-sm text-zinc-400">{detail || 'Hub offline'}</p>
      <button
        type="button"
        onClick={onRetry}
        className="mt-4 inline-flex items-center gap-2 rounded-lg bg-zinc-800 px-4 py-2 text-sm font-medium text-zinc-200 hover:bg-zinc-700 border border-zinc-700 transition-colors"
      >
        <RetryIcon />
        Retry connection
      </button>
    </>
  )
}

function Spinner() {
  return (
    <svg className="mx-auto size-10 text-primary-500 animate-spin" fill="none" viewBox="0 0 24 24">
      <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
      <path
        className="opacity-75"
        fill="currentColor"
        d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
      />
    </svg>
  )
}

function QrIcon() {
  return (
    <svg className="mx-auto size-10 text-amber-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth={1.5}
        d="M3.75 4.875c0-.621.504-1.125 1.125-1.125h4.5c.621 0 1.125.504 1.125 1.125v4.5c0 .621-.504 1.125-1.125 1.125h-4.5A1.125 1.125 0 013.75 9.375v-4.5zM3.75 14.625c0-.621.504-1.125 1.125-1.125h4.5c.621 0 1.125.504 1.125 1.125v4.5c0 .621-.504 1.125-1.125 1.125h-4.5a1.125 1.125 0 01-1.125-1.125v-4.5zM13.5 4.875c0-.621.504-1.125 1.125-1.125h4.5c.621 0 1.125.504 1.125 1.125v4.5c0 .621-.504 1.125-1.125 1.125h-4.5A1.125 1.125 0 0113.5 9.375v-4.5z"
      />
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth={1.5}
        d="M6.75 6.75h.75v.75h-.75v-.75zM6.75 16.5h.75v.75h-.75v-.75zM16.5 6.75h.75v.75h-.75v-.75zM13.5 13.5h.75v.75h-.75v-.75zM13.5 19.5h.75v.75h-.75v-.75zM19.5 13.5h.75v.75h-.75v-.75zM19.5 19.5h.75v.75h-.75v-.75zM16.5 16.5h.75v.75h-.75v-.75z"
      />
    </svg>
  )
}

function ErrorIcon() {
  return (
    <svg className="mx-auto size-10 text-red-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth={1.5}
        d="M12 9v3.75m-9.303 3.376c-.866 1.5.217 3.374 1.948 3.374h14.71c1.73 0 2.813-1.874 1.948-3.374L13.949 3.378c-.866-1.5-3.032-1.5-3.898 0L2.697 16.126zM12 15.75h.007v.008H12v-.008z"
      />
    </svg>
  )
}

function OfflineIcon() {
  return (
    <svg className="mx-auto size-10 text-zinc-500" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth={1.5}
        d="M3 3l8.735 8.735m0 0a.374.374 0 11.53.53m-.53-.53l.53.53m0 0L21 21M6.168 6.168A7.478 7.478 0 004.5 12c0 1.658.538 3.19 1.449 4.431L18.569 3.811A7.478 7.478 0 0012 2.25c-2.16 0-4.127.914-5.5 2.379m0 0L3 3m3.168 3.168A7.478 7.478 0 004.5 12c0 4.142 3.358 7.5 7.5 7.5a7.478 7.478 0 005.832-2.793"
      />
    </svg>
  )
}

function RetryIcon() {
  return (
    <svg className="size-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeWidth={2}
        d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0l3.181 3.183a8.25 8.25 0 0013.803-3.7M4.031 9.865a8.25 8.25 0 0113.803-3.7l3.181 3.182"
      />
    </svg>
  )
}
