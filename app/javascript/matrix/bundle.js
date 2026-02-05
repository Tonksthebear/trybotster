/**
 * Matrix Device Key Bundle Parsing
 *
 * Parses DeviceKeyBundle from QR code URL fragments.
 * The bundle is Base32-encoded binary data containing Matrix device keys.
 *
 * Used during initial pairing: CLI displays QR code, browser scans it,
 * URL fragment contains the bundle needed to establish Matrix session.
 *
 * Binary format (~165 bytes vs 1813 bytes for Signal):
 * - version: 1 byte (0x05 for Matrix)
 * - identity_key: 32 bytes (Curve25519)
 * - signing_key: 32 bytes (Ed25519)
 * - one_time_key: 32 bytes (Curve25519)
 * - key_id_length: 4 bytes (LE u32)
 * - key_id: N bytes (UTF-8, ~10 bytes typical)
 * - signature: 64 bytes (Ed25519)
 */

import bridge from "workers/bridge"

/**
 * Ensure the worker bridge is initialized for Matrix crypto.
 * Call this before any Matrix operations.
 *
 * @param {string} cryptoWorkerUrl - URL to crypto SharedWorker (matrix_crypto.js)
 * @param {string} wasmJsUrl - URL to matrix-sdk-crypto-wasm JS
 */
export async function ensureMatrixReady(cryptoWorkerUrl, wasmJsUrl) {
  // Matrix SDK crypto WASM handles binary loading internally, no separate binary URL needed
  await bridge.init({ cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl: null })
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
 * Convert Uint8Array to Base64 string.
 */
function bytesToBase64(bytes) {
  let binary = ""
  for (let i = 0; i < bytes.length; i++) {
    binary += String.fromCharCode(bytes[i])
  }
  return btoa(binary)
}

/**
 * Read little-endian u32 from byte array.
 */
function readU32LE(bytes, offset) {
  return (
    (bytes[offset] |
      (bytes[offset + 1] << 8) |
      (bytes[offset + 2] << 16) |
      (bytes[offset + 3] << 24)) >>>
    0
  )
}

/**
 * Parse binary DeviceKeyBundle format from CLI.
 *
 * Binary format (~165 bytes):
 * - version: 1 byte (0x05 for Matrix)
 * - identity_key: 32 bytes (Curve25519)
 * - signing_key: 32 bytes (Ed25519)
 * - one_time_key: 32 bytes (Curve25519)
 * - key_id_length: 4 bytes (LE u32)
 * - key_id: N bytes (UTF-8)
 * - signature: 64 bytes (Ed25519)
 */
function parseBinaryBundle(bytes) {
  const MATRIX_VERSION = 0x05
  const MIN_SIZE = 1 + 32 + 32 + 32 + 4 + 64 // 165 bytes minimum (key_id can be 0)

  if (bytes.length < MIN_SIZE) {
    throw new Error(
      `Invalid bundle size: ${bytes.length}, expected at least ${MIN_SIZE}`
    )
  }

  let offset = 0

  // Version (1 byte)
  const version = bytes[offset]
  offset += 1

  if (version !== MATRIX_VERSION) {
    throw new Error(
      `Invalid bundle version: ${version}, expected ${MATRIX_VERSION}`
    )
  }

  // Identity key - Curve25519 (32 bytes)
  const identityKey = bytes.slice(offset, offset + 32)
  offset += 32

  // Signing key - Ed25519 (32 bytes)
  const signingKey = bytes.slice(offset, offset + 32)
  offset += 32

  // One-time key - Curve25519 (32 bytes)
  const oneTimeKey = bytes.slice(offset, offset + 32)
  offset += 32

  // Key ID length (4 bytes LE u32)
  const keyIdLength = readU32LE(bytes, offset)
  offset += 4

  // Validate key ID length is reasonable
  if (keyIdLength > 256) {
    throw new Error(`Invalid key ID length: ${keyIdLength}`)
  }

  // Validate total size
  const expectedSize = MIN_SIZE + keyIdLength
  if (bytes.length !== expectedSize) {
    throw new Error(
      `Invalid bundle size: ${bytes.length}, expected ${expectedSize}`
    )
  }

  // Key ID (N bytes UTF-8)
  const keyIdBytes = bytes.slice(offset, offset + keyIdLength)
  const oneTimeKeyId = new TextDecoder().decode(keyIdBytes)
  offset += keyIdLength

  // Signature - Ed25519 (64 bytes)
  const signature = bytes.slice(offset, offset + 64)

  return {
    version,
    identityKey: bytesToBase64(identityKey),
    signingKey: bytesToBase64(signingKey),
    oneTimeKey: bytesToBase64(oneTimeKey),
    oneTimeKeyId,
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
  // Matrix bundles are ~165 bytes, which is ~264 base32 chars
  // Use a lower threshold since Matrix bundles are much smaller than Signal
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
    console.error("[Matrix] Failed to parse bundle from fragment:", error)
    return null
  }
}
