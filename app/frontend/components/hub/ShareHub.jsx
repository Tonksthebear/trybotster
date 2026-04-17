import React, { useState, useCallback, useRef, useEffect } from 'react'
import {
  Dialog,
  DialogTitle,
  DialogDescription,
  DialogBody,
  DialogActions,
} from '../catalyst/dialog'
import { Button } from '../catalyst/button'
import { getHub } from '../../lib/hub-bridge'

export default function ShareHub({ hubId }) {
  const [open, setOpen] = useState(false)
  const [status, setStatus] = useState('idle') // idle | loading | success | error
  const [errorMessage, setErrorMessage] = useState('')
  const [url, setUrl] = useState('')
  const [qrAscii, setQrAscii] = useState('')
  const [copyStatus, setCopyStatus] = useState('')
  const unsubRef = useRef(null)

  // Clean up listener on unmount
  useEffect(() => {
    return () => {
      unsubRef.current?.()
      unsubRef.current = null
    }
  }, [])

  const requestCode = useCallback(async () => {
    // Clean up any previous listener
    unsubRef.current?.()
    unsubRef.current = null

    setStatus('loading')
    setErrorMessage('')

    const hub = getHub(hubId)
    if (!hub) {
      setStatus('error')
      setErrorMessage('Connection unavailable')
      return
    }

    const unsub = hub.on('connectionCode', (message) => {
      unsub()
      unsubRef.current = null
      const { url: codeUrl, qr_ascii } = message
      if (!codeUrl || !qr_ascii) {
        setStatus('error')
        setErrorMessage('Invalid response from hub')
        return
      }
      setUrl(codeUrl)
      setQrAscii(
        Array.isArray(qr_ascii) ? qr_ascii.join('\n') : qr_ascii
      )
      setStatus('success')
    })

    unsubRef.current = unsub

    try {
      const sent = await hub.requestConnectionCode()
      if (!sent) {
        unsub()
        unsubRef.current = null
        setStatus('error')
        setErrorMessage('Failed to send request - not connected')
      }
    } catch (err) {
      unsub()
      unsubRef.current = null
      setStatus('error')
      setErrorMessage(err.message || 'Connection failed')
    }
  }, [hubId])

  function handleOpen() {
    setOpen(true)
    requestCode()
  }

  function handleClose() {
    unsubRef.current?.()
    unsubRef.current = null
    setOpen(false)
    setStatus('idle')
    setCopyStatus('')
  }

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(url)
      setCopyStatus('Copied!')
      setTimeout(() => setCopyStatus(''), 2000)
    } catch {
      setCopyStatus('Failed to copy')
    }
  }

  return (
    <>
      <button
        type="button"
        onClick={handleOpen}
        className="inline-flex items-center gap-1.5 px-3 py-1.5 text-sm font-medium text-zinc-400 hover:text-zinc-200 bg-zinc-800/50 hover:bg-zinc-800 border border-zinc-700/50 rounded-lg transition-colors"
      >
        <svg className="size-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
          <path
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2}
            d="M7.217 10.907a2.25 2.25 0 100 2.186m0-2.186c.18.324.283.696.283 1.093s-.103.77-.283 1.093m0-2.186l9.566-5.314m-9.566 7.5l9.566 5.314m0 0a2.25 2.25 0 103.935 2.186 2.25 2.25 0 00-3.935-2.186zm0-12.814a2.25 2.25 0 103.933-2.185 2.25 2.25 0 00-3.933 2.185z"
          />
        </svg>
        <span>Share</span>
      </button>

      <Dialog open={open} onClose={handleClose} size="sm">
        <DialogTitle>Share Hub Access</DialogTitle>
        <DialogDescription>
          Scan QR or copy link to connect another device
        </DialogDescription>

        <DialogBody>
          {status === 'loading' && (
            <div className="py-8 text-center">
              <svg
                className="size-8 text-zinc-600 mx-auto mb-3 animate-spin"
                fill="none"
                viewBox="0 0 24 24"
              >
                <circle
                  className="opacity-25"
                  cx="12"
                  cy="12"
                  r="10"
                  stroke="currentColor"
                  strokeWidth="4"
                />
                <path
                  className="opacity-75"
                  fill="currentColor"
                  d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
                />
              </svg>
              <p className="text-sm text-zinc-500">
                Generating connection code...
              </p>
            </div>
          )}

          {status === 'error' && (
            <div className="py-8 text-center">
              <svg
                className="size-8 text-red-500 mx-auto mb-3"
                fill="none"
                stroke="currentColor"
                viewBox="0 0 24 24"
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  strokeWidth={2}
                  d="M12 9v3.75m9-.75a9 9 0 11-18 0 9 9 0 0118 0zm-9 3.75h.008v.008H12v-.008z"
                />
              </svg>
              <p className="text-sm text-red-400">{errorMessage}</p>
              <button
                type="button"
                onClick={requestCode}
                className="mt-3 text-sm text-primary-400 hover:text-primary-300"
              >
                Try again
              </button>
            </div>
          )}

          {status === 'success' && (
            <div className="space-y-5">
              {/* QR Code */}
              <div className="flex justify-center">
                <pre
                  className="font-mono text-xs whitespace-pre leading-none tracking-tighter bg-white text-black p-3 rounded-lg select-none inline-block origin-center"
                  style={{ transform: 'scale(0.5) scaleY(1.1)' }}
                >
                  {qrAscii}
                </pre>
              </div>

              {/* URL with copy */}
              <div className="space-y-2">
                <label className="text-xs text-zinc-500 uppercase tracking-wider">
                  Connection URL
                </label>
                <div className="flex gap-2">
                  <input
                    type="text"
                    readOnly
                    value={url}
                    className="flex-1 px-3 py-2 bg-zinc-950 border border-zinc-700 rounded text-zinc-300 text-sm font-mono truncate focus:outline-none"
                  />
                  <button
                    type="button"
                    onClick={handleCopy}
                    className="px-3 py-2 bg-zinc-800 hover:bg-zinc-700 border border-zinc-700 rounded text-zinc-300 text-sm transition-colors"
                  >
                    <svg
                      className="size-4"
                      fill="none"
                      stroke="currentColor"
                      viewBox="0 0 24 24"
                    >
                      <path
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        strokeWidth={2}
                        d="M15.666 3.888A2.25 2.25 0 0013.5 2.25h-3c-1.03 0-1.9.693-2.166 1.638m7.332 0c.055.194.084.4.084.612v0a.75.75 0 01-.75.75H9.75a.75.75 0 01-.75-.75v0c0-.212.03-.418.084-.612m7.332 0c.646.049 1.288.11 1.927.184 1.1.128 1.907 1.077 1.907 2.185V19.5a2.25 2.25 0 01-2.25 2.25H6.75A2.25 2.25 0 014.5 19.5V6.257c0-1.108.806-2.057 1.907-2.185a48.208 48.208 0 011.927-.184"
                      />
                    </svg>
                  </button>
                </div>
                <p
                  className={`text-xs h-4 ${
                    copyStatus === 'Copied!'
                      ? 'text-emerald-400'
                      : copyStatus
                        ? 'text-red-400'
                        : 'text-zinc-600'
                  }`}
                >
                  {copyStatus}
                </p>
              </div>

              {/* Security note */}
              <div className="flex items-start gap-2 p-3 bg-zinc-900/50 border border-zinc-800 rounded-lg">
                <svg
                  className="size-4 text-emerald-500 mt-0.5 shrink-0"
                  fill="none"
                  stroke="currentColor"
                  viewBox="0 0 24 24"
                >
                  <path
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeWidth={2}
                    d="M9 12.75L11.25 15 15 9.75m-3-7.036A11.959 11.959 0 013.598 6 11.99 11.99 0 003 9.749c0 5.592 3.824 10.29 9 11.623 5.176-1.332 9-6.03 9-11.622 0-1.31-.21-2.571-.598-3.751h-.152c-3.196 0-6.1-1.248-8.25-3.285z"
                  />
                </svg>
                <p className="text-xs text-zinc-400">
                  This link contains encrypted keys. Anyone with this link can
                  connect to your hub.
                </p>
              </div>
            </div>
          )}
        </DialogBody>

        <DialogActions>
          <Button plain onClick={handleClose}>
            Close
          </Button>
        </DialogActions>
      </Dialog>
    </>
  )
}
