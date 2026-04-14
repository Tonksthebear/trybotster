/**
 * Crypto Bridge — connects the Vite React world to the importmap-side
 * crypto worker (workers/bridge.js) and bundle parser (matrix/bundle.js).
 *
 * The importmap entry for workers/bridge exports a singleton WorkerBridge
 * instance. Application.js (importmap side) assigns it to window.__botsterBridge
 * so the Vite side can reach it without import-map resolution.
 *
 * Similarly, matrix/bundle.js exports parseBundleFromUrl / parseBundleFromFragment
 * and ensureMatrixReady, exposed via window.__botsterBundle.
 */

function resolveBridge() {
  const bridge = window.__botsterBridge
  if (!bridge) {
    throw new Error(
      '[crypto-bridge] WorkerBridge not available. ' +
      'Ensure application.js assigns window.__botsterBridge.'
    )
  }
  return bridge
}

function resolveBundle() {
  const bundle = window.__botsterBundle
  if (!bundle) {
    throw new Error(
      '[crypto-bridge] Bundle module not available. ' +
      'Ensure application.js assigns window.__botsterBundle.'
    )
  }
  return bundle
}

/**
 * Initialize the crypto worker (idempotent).
 * Reads WASM URLs from <meta> tags, same as the Stimulus controller.
 */
export async function ensureCryptoReady() {
  const bundle = resolveBundle()
  const cryptoWorkerUrl = document.querySelector('meta[name="crypto-worker-url"]')?.content
  const wasmJsUrl = document.querySelector('meta[name="crypto-wasm-js-url"]')?.content
  const wasmBinaryUrl = document.querySelector('meta[name="crypto-wasm-binary-url"]')?.content
  await bundle.ensureMatrixReady(cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl)
}

/**
 * Parse a DeviceKeyBundle from the current page's URL fragment.
 * Returns the parsed bundle or null.
 */
export function parseBundleFromFragment() {
  return resolveBundle().parseBundleFromFragment()
}

/**
 * Parse a DeviceKeyBundle from a pasted URL string.
 * Returns the parsed bundle or null.
 */
export function parseBundleFromUrl(urlString) {
  return resolveBundle().parseBundleFromUrl(urlString)
}

/**
 * Create an E2E encrypted session with the hub using the parsed bundle.
 */
export async function createSession(hubId, bundle) {
  const bridge = resolveBridge()
  return bridge.createSession(hubId, bundle)
}
