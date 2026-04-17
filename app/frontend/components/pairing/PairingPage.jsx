import React, { useEffect, useRef, useState } from 'react'
import { useNavigate, Link as RouterLink } from 'react-router-dom'
import { usePairingStore } from '../../store/pairing-store'
import {
  ensureCryptoReady,
  parseBundleFromFragment,
  parseBundleFromUrl,
  createSession,
} from '../../lib/crypto-bridge'
import { Button } from '../catalyst/button'
import { Input } from '../catalyst/input'
import { Heading } from '../catalyst/heading'
import { Text } from '../catalyst/text'
import { Badge } from '../catalyst/badge'

// SVG icons as small components to avoid external deps
function ShieldIcon({ className }) {
  return (
    <svg className={className} fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2}
        d="M9 12.75L11.25 15 15 9.75m-3-7.036A11.959 11.959 0 013.598 6 11.99 11.99 0 003 9.749c0 5.592 3.824 10.29 9 11.623 5.176-1.332 9-6.03 9-11.622 0-1.31-.21-2.571-.598-3.751h-.152c-3.196 0-6.1-1.248-8.25-3.285z" />
    </svg>
  )
}

function LockIcon({ className }) {
  return (
    <svg className={className} fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2}
        d="M16.5 10.5V6.75a4.5 4.5 0 10-9 0v3.75m-.75 11.25h10.5a2.25 2.25 0 002.25-2.25v-6.75a2.25 2.25 0 00-2.25-2.25H6.75a2.25 2.25 0 00-2.25 2.25v6.75a2.25 2.25 0 002.25 2.25z" />
    </svg>
  )
}

function LinkIcon({ className }) {
  return (
    <svg className={className} fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2}
        d="M13.19 8.688a4.5 4.5 0 011.242 7.244l-4.5 4.5a4.5 4.5 0 01-6.364-6.364l1.757-1.757m9.86-1.02a4.5 4.5 0 00-1.242-7.244l-4.5-4.5a4.5 4.5 0 00-6.364 6.364l1.757 1.757" />
    </svg>
  )
}

function SpinnerIcon({ className }) {
  return (
    <svg className={className} fill="none" viewBox="0 0 24 24">
      <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
      <path className="opacity-75" fill="currentColor"
        d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" />
    </svg>
  )
}

function CheckIcon({ className }) {
  return (
    <svg className={className} fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4.5 12.75l6 6 9-13.5" />
    </svg>
  )
}

function AlertIcon({ className }) {
  return (
    <svg className={className} fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2}
        d="M12 9v3.75m9-.75a9 9 0 11-18 0 9 9 0 0118 0zm-9 3.75h.008v.008H12v-.008z" />
    </svg>
  )
}

function ClipboardIcon({ className }) {
  return (
    <svg className={className} fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2}
        d="M15.666 3.888A2.25 2.25 0 0013.5 2.25h-3c-1.03 0-1.9.693-2.166 1.638m7.332 0c.055.194.084.4.084.612v0a.75.75 0 01-.75.75H9.75a.75.75 0 01-.75-.75v0c0-.212.03-.418.084-.612m7.332 0c.646.049 1.288.11 1.927.184 1.1.128 1.907 1.077 1.907 2.185V19.5a2.25 2.25 0 01-2.25 2.25H6.75A2.25 2.25 0 014.5 19.5V6.257c0-1.108.806-2.057 1.907-2.185a48.208 48.208 0 011.927-.184" />
    </svg>
  )
}

function ShieldCheckIcon({ className }) {
  return (
    <svg className={className} fill="none" stroke="currentColor" viewBox="0 0 24 24">
      <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2}
        d="M9 12.75L11.25 15 15 9.75m-3-7.036A11.959 11.959 0 013.598 6 11.99 11.99 0 003 9.749c0 5.592 3.824 10.29 9 11.623 5.176-1.332 9-6.03 9-11.622 0-1.31-.21-2.571-.598-3.751h-.152c-3.196 0-6.1-1.248-8.25-3.285z" />
    </svg>
  )
}

export default function PairingPage({ hubId, redirectUrl }) {
  const navigate = useNavigate()
  const store = usePairingStore()
  const inputRef = useRef(null)
  const [copyLabel, setCopyLabel] = useState('Copy Code')

  // Initialize on mount: parse bundle from URL fragment, init crypto
  useEffect(() => {
    let cancelled = false

    async function init() {
      // Check for bundle in URL fragment
      let bundle = parseBundleFromFragment()
      let rawCode = null

      if (bundle) {
        rawCode = location.hash?.startsWith('#') ? location.hash.slice(1) : null
        // Clean URL
        history.replaceState(null, '', location.pathname + location.search)
      }

      // Initialize crypto worker
      try {
        await ensureCryptoReady()
      } catch (error) {
        console.error('[PairingPage] Failed to initialize crypto:', error)
        if (!cancelled) {
          store.fail('Failed to initialize encryption. Please refresh and try again.')
        }
        return
      }

      if (cancelled) return

      if (bundle) {
        bundle.hubId = hubId
        store.showReady(bundle, rawCode)
      } else {
        store.showPaste()
      }
    }

    init()
    return () => { cancelled = true }
  }, [hubId])

  function handlePaste() {
    // Short delay to let the paste value populate
    setTimeout(() => processPastedUrl(), 0)
  }

  function processPastedUrl() {
    const input = inputRef.current?.value?.trim()
    if (!input) return

    store.clearPasteLinkError()

    try {
      const bundle = parseBundleFromUrl(input)
      if (!bundle) {
        store.setPasteLinkError('Invalid connection link. Make sure you copied the full URL including the # part.')
        return
      }

      bundle.hubId = hubId
      let rawCode = null
      try {
        const url = new URL(input)
        rawCode = url.hash?.startsWith('#') ? url.hash.slice(1) : null
      } catch (_) {}

      store.showReady(bundle, rawCode)
    } catch (error) {
      console.error('[PairingPage] Failed to parse pasted URL:', error)
      store.setPasteLinkError('Could not read connection code from that link. Please try copying again.')
    }
  }

  async function handlePair() {
    store.startVerifying()

    try {
      await createSession(hubId, store.bundle)
      store.succeed()

      setTimeout(() => {
        navigate(redirectUrl)
      }, 800)
    } catch (error) {
      console.error('[PairingPage] Session creation failed:', error)
      store.fail(`Pairing failed: ${error.message || 'Unknown error'}. Scan the QR code again to get a fresh code.`)
    }
  }

  async function handleCopyCode() {
    if (!store.rawCode) return

    try {
      await navigator.clipboard.writeText(store.rawCode)
      setCopyLabel('Copied!')
      setTimeout(() => setCopyLabel('Copy Code'), 2000)
    } catch (error) {
      console.error('[PairingPage] Copy failed:', error)
    }
  }

  const fingerprint = store.bundle?.identityKey?.slice(0, 8)

  return (
    <div className="min-h-[calc(100vh-8rem)] flex items-center justify-center px-4 py-12">
      <div className="max-w-md w-full">
        {/* Header */}
        <div className="text-center mb-8">
          <div className="inline-flex items-center justify-center w-16 h-16 bg-primary-500/10 rounded-2xl mb-6">
            <ShieldIcon className="w-8 h-8 text-primary-400" />
          </div>
          <Heading level={1} className="!text-3xl !font-bold mb-2">Secure Pairing</Heading>
          <Text>Establish an encrypted connection to your hub</Text>
        </div>

        {/* Paste link state */}
        {store.status === 'paste' && (
          <div data-testid="pairing-paste">
            <div className="bg-zinc-900 border border-zinc-800 rounded-lg p-6 mb-6">
              <div className="space-y-4">
                <div className="flex items-start gap-3">
                  <div className="shrink-0 mt-0.5">
                    <LinkIcon className="size-5 text-primary-400" />
                  </div>
                  <div>
                    <h3 className="text-sm font-medium text-zinc-200">Paste Connection Link</h3>
                    <p className="text-xs text-zinc-500 mt-1">
                      Copy the connection URL from your hub's share menu and paste it below.
                    </p>
                  </div>
                </div>

                <div className="pt-2">
                  <Input
                    ref={inputRef}
                    type="text"
                    placeholder="Paste connection link here..."
                    onPaste={handlePaste}
                    className="font-mono"
                  />
                  {store.pasteLinkError && (
                    <p className="text-xs text-red-400 mt-2">{store.pasteLinkError}</p>
                  )}
                </div>
              </div>
            </div>

            <div className="flex items-start gap-2 p-3 bg-zinc-900/50 border border-zinc-800 rounded-lg">
              <ShieldCheckIcon className="size-4 text-emerald-500 mt-0.5 shrink-0" />
              <p className="text-xs text-zinc-400">
                The connection link contains encrypted keys that never touch the server. Your session is end-to-end encrypted.
              </p>
            </div>
          </div>
        )}

        {/* Ready state — bundle parsed, awaiting confirmation */}
        {store.status === 'ready' && (
          <div data-testid="pairing-ready" data-pairing-target="ready">
            <div className="bg-zinc-900 border border-zinc-800 rounded-lg p-6 mb-6">
              <div className="space-y-4">
                <div className="flex items-start gap-3">
                  <div className="shrink-0 mt-0.5">
                    <LockIcon className="size-5 text-primary-400" />
                  </div>
                  <div>
                    <h3 className="text-sm font-medium text-zinc-200">End-to-end encrypted</h3>
                    <p className="text-xs text-zinc-500 mt-1">
                      Your private keys are generated locally and never leave this device. Not even Botster's servers can read your messages.
                    </p>
                  </div>
                </div>

                <div className="border-t border-zinc-800 pt-4">
                  <div className="flex items-center justify-between">
                    <span className="text-xs text-zinc-500">Hub identity fingerprint</span>
                    <Badge color="indigo">
                      <code className="text-xs font-mono">{fingerprint}...</code>
                    </Badge>
                  </div>
                </div>
              </div>
            </div>

            <div className="flex gap-3">
              <Button
                color="indigo"
                className="flex-1"
                onClick={handlePair}
                data-action="pairing#pair"
              >
                <ShieldCheckIcon data-slot="icon" className="size-5" />
                Complete Pairing
              </Button>
              <Button outline onClick={handleCopyCode}>
                <ClipboardIcon data-slot="icon" className="size-5" />
                {copyLabel}
              </Button>
            </div>
          </div>
        )}

        {/* Loading/verifying state */}
        {store.status === 'verifying' && (
          <div className="bg-zinc-900 border border-zinc-800 rounded-lg p-8 text-center">
            <div className="inline-flex items-center justify-center w-12 h-12 bg-primary-500/10 rounded-xl mb-4">
              <SpinnerIcon className="w-6 h-6 text-primary-400 animate-spin" />
            </div>
            <Text>Establishing encrypted session...</Text>
          </div>
        )}

        {/* Success state */}
        {store.status === 'success' && (
          <div
            data-testid="pairing-success"
            data-pairing-target="success"
            className="bg-emerald-500/10 border border-emerald-500/20 rounded-lg p-8 text-center"
          >
            <div className="inline-flex items-center justify-center w-12 h-12 bg-emerald-500/10 rounded-xl mb-4">
              <CheckIcon className="w-6 h-6 text-emerald-400" />
            </div>
            <h3 className="text-lg font-medium text-zinc-100 mb-1">Paired successfully</h3>
            <Text>Redirecting to your hub...</Text>
          </div>
        )}

        {/* Error state */}
        {store.status === 'error' && (
          <div>
            <div className="bg-red-500/10 border border-red-500/20 rounded-lg p-6 mb-6">
              <div className="flex items-start gap-3">
                <AlertIcon className="w-5 h-5 text-red-400 shrink-0 mt-0.5" />
                <div>
                  <h3 className="text-sm font-medium text-red-300">Pairing failed</h3>
                  <p className="text-sm text-zinc-400 mt-1">
                    {store.errorMessage || 'Invalid or missing connection code. Scan the QR code from your hub\'s terminal to try again.'}
                  </p>
                </div>
              </div>
            </div>

            <RouterLink to={redirectUrl} className="block text-center text-sm text-zinc-500 hover:text-zinc-300 transition-colors">
              &larr; Back to hub
            </RouterLink>
          </div>
        )}

        {/* Back link (shown with ready and paste states) */}
        {(store.status === 'paste' || store.status === 'ready') && (
          <div className="text-center mt-6">
            <RouterLink to={redirectUrl} className="text-sm text-zinc-500 hover:text-zinc-300 transition-colors">
              &larr; Back to hub
            </RouterLink>
          </div>
        )}

        {/* Idle/initializing — blank, crypto loading */}
        {store.status === 'idle' && (
          <div className="text-center py-8">
            <SpinnerIcon className="w-6 h-6 text-zinc-500 animate-spin mx-auto" />
          </div>
        )}
      </div>
    </div>
  )
}
