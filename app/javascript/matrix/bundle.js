/**
 * Device Key Bundle Parsing
 *
 * Parses DeviceKeyBundle from QR code URL fragments.
 * The bundle is Base32-encoded binary data containing Olm device keys.
 *
 * Used during initial pairing: CLI displays QR code, browser scans it,
 * URL fragment contains the bundle needed to establish Olm session.
 *
 * Binary format (161 bytes, fixed size):
 *   [1]  version (0x06)
 *   [32] identity_key (Curve25519)
 *   [32] signing_key (Ed25519)
 *   [32] one_time_key (Curve25519)
 *   [64] signature (Ed25519)
 */

import bridge from "workers/bridge"

/**
 * Ensure the worker bridge is initialized for crypto.
 * Call this before any crypto operations.
 *
 * @param {string} cryptoWorkerUrl - URL to crypto SharedWorker
 * @param {string} wasmJsUrl - URL to vodozemac-wasm JS
 */
export async function ensureMatrixReady(cryptoWorkerUrl, wasmJsUrl) {
  await bridge.init({ cryptoWorkerUrl, wasmJsUrl })
}

/**
 * Decode Base32 (RFC 4648) to Uint8Array.
 * Used for QR code URLs which use Base32 for alphanumeric mode efficiency.
 */
function base32Decode(base32) {
  base32 = base32
    .toUpperCase()
    .replace(/=+$/, "")
    .replace(/[^A-Z2-7]/g, "")

  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
  let bits = ""
  for (const c of base32) {
    const i = alphabet.indexOf(c)
    if (i < 0) throw new Error(`Invalid Base32 character: ${c}`)
    bits += i.toString(2).padStart(5, "0")
  }

  const byteCount = Math.floor(bits.length / 8)
  const bytes = new Uint8Array(byteCount)
  for (let i = 0; i < byteCount; i++) {
    bytes[i] = parseInt(bits.slice(i * 8, i * 8 + 8), 2)
  }
  return bytes
}

/**
 * Convert Uint8Array to unpadded Base64 string.
 */
function bytesToBase64(bytes) {
  return btoa(String.fromCharCode(...bytes)).replace(/=+$/, "")
}

/**
 * Parse binary DeviceKeyBundle v6 format from CLI.
 *
 * Fixed 161 bytes:
 *   [1]  version (0x06)
 *   [32] identity_key (Curve25519)
 *   [32] signing_key (Ed25519)
 *   [32] one_time_key (Curve25519)
 *   [64] signature (Ed25519)
 */
export function parseBinaryBundle(bytes) {
  const BUNDLE_VERSION = 0x06
  const BUNDLE_SIZE = 161

  if (bytes.length < BUNDLE_SIZE) {
    throw new Error(
      `Invalid bundle size: ${bytes.length}, expected ${BUNDLE_SIZE}`
    )
  }

  let offset = 0

  const version = bytes[offset]
  offset += 1

  if (version !== BUNDLE_VERSION) {
    throw new Error(
      `Invalid bundle version: ${version}, expected ${BUNDLE_VERSION}`
    )
  }

  const identityKey = bytes.slice(offset, offset + 32)
  offset += 32

  const signingKey = bytes.slice(offset, offset + 32)
  offset += 32

  const oneTimeKey = bytes.slice(offset, offset + 32)
  offset += 32

  const signature = bytes.slice(offset, offset + 64)
  offset += 64

  return {
    version,
    identityKey: bytesToBase64(identityKey),
    signingKey: bytesToBase64(signingKey),
    oneTimeKey: bytesToBase64(oneTimeKey),
    signature: bytesToBase64(signature),
  }
}

/**
 * Parse DeviceKeyBundle from URL fragment.
 * Expected format: #<base32_binary>
 *
 * @returns {Object|null} Parsed bundle with hubId, or null if no valid bundle
 */
export function parseBundleFromFragment() {
  const hash = window.location.hash

  if (!hash) {
    return null
  }

  const base32Data = hash.startsWith("#") ? hash.slice(1) : hash
  // v6 bundles are 161 bytes = ~258 base32 chars
  if (!base32Data || base32Data.length < 50) {
    return null
  }

  try {
    const bytes = base32Decode(base32Data)
    const bundle = parseBinaryBundle(bytes)

    // Extract hub ID from URL path (e.g., /hubs/abc123)
    const hubMatch = window.location.pathname.match(/\/hubs\/([^\/]+)/)
    bundle.hubId = hubMatch ? hubMatch[1] : ""

    return bundle
  } catch (error) {
    console.error("[Bundle] Failed to parse bundle from fragment:", error)
    return null
  }
}
