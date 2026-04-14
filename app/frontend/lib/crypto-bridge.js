/**
 * Crypto Bridge — connects to the crypto worker (workers/bridge.js)
 * and bundle parser (matrix/bundle.js) via direct imports.
 */

import bridge from 'workers/bridge'
import * as bundleModule from 'matrix/bundle'

/**
 * Initialize the crypto worker (idempotent).
 * Reads WASM URLs from <meta> tags.
 */
export async function ensureCryptoReady() {
  const cryptoWorkerUrl = document.querySelector('meta[name="crypto-worker-url"]')?.content
  const wasmJsUrl = document.querySelector('meta[name="crypto-wasm-js-url"]')?.content
  const wasmBinaryUrl = document.querySelector('meta[name="crypto-wasm-binary-url"]')?.content
  await bundleModule.ensureMatrixReady(cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl)
}

/**
 * Parse a DeviceKeyBundle from the current page's URL fragment.
 * Returns the parsed bundle or null.
 */
export function parseBundleFromFragment() {
  return bundleModule.parseBundleFromFragment()
}

/**
 * Parse a DeviceKeyBundle from a pasted URL string.
 * Returns the parsed bundle or null.
 */
export function parseBundleFromUrl(urlString) {
  return bundleModule.parseBundleFromUrl(urlString)
}

/**
 * Create an E2E encrypted session with the hub using the parsed bundle.
 */
export async function createSession(hubId, bundle) {
  return bridge.createSession(hubId, bundle)
}
