import { create } from 'zustand'

/**
 * Pairing ceremony state machine.
 *
 * States:
 *   idle       — initial, determining entry path
 *   paste      — no bundle in URL, show paste input
 *   ready      — bundle parsed, awaiting user confirmation
 *   verifying  — createSession in progress
 *   success    — paired, redirecting
 *   error      — something broke
 */
export const usePairingStore = create((set, get) => ({
  // --- State ---
  status: 'idle',      // idle | paste | ready | verifying | success | error
  bundle: null,        // parsed DeviceKeyBundle (identityKey, signingKey, oneTimeKey, etc.)
  rawCode: null,       // raw base32 fragment for copy-to-clipboard
  errorMessage: null,
  pasteLinkError: null,

  // --- Actions ---

  showPaste() {
    set({ status: 'paste', pasteLinkError: null })
  },

  showReady(bundle, rawCode) {
    set({ status: 'ready', bundle, rawCode, pasteLinkError: null, errorMessage: null })
  },

  startVerifying() {
    set({ status: 'verifying' })
  },

  succeed() {
    set({ status: 'success' })
  },

  fail(message) {
    set({ status: 'error', errorMessage: message })
  },

  setPasteLinkError(message) {
    set({ pasteLinkError: message })
  },

  clearPasteLinkError() {
    set({ pasteLinkError: null })
  },

  reset() {
    set({
      status: 'idle',
      bundle: null,
      rawCode: null,
      errorMessage: null,
      pasteLinkError: null,
    })
  },
}))
