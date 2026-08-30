/**
 * Browser pairing helpers.
 */

import {
  parsePairingBundle,
  parsePairingBundleFromUrl,
  preparePairing,
} from 'transport/browser_pairing'
import bridge from 'workers/bridge'

/**
 * Initialize the WebRTC transport.
 */
export async function ensureCryptoReady() {
  await bridge.init()
}

/**
 * Parse a pairing bundle from the current page's URL fragment.
 * Returns the parsed bundle or null.
 */
export function parseBundleFromFragment() {
  return parsePairingBundle(window.location.hash)
}

/**
 * Parse a pairing bundle from a pasted URL string.
 * Returns the parsed bundle or null.
 */
export function parseBundleFromUrl(urlString) {
  return parsePairingBundleFromUrl(urlString)
}

/**
 * Store the pinned hub key and browser signing credential.
 */
export async function createSession(hubId, bundle) {
  return preparePairing(hubId, bundle)
}
