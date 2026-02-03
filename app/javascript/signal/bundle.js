/**
 * Signal Protocol Bundle Parsing
 *
 * Parses PreKeyBundle from QR code URL fragments.
 * The bundle is Base32-encoded binary data containing Signal Protocol keys.
 *
 * Used during initial pairing: CLI displays QR code, browser scans it,
 * URL fragment contains the bundle needed to establish Signal session.
 */

import bridge from "workers/bridge"

/**
 * Ensure the worker bridge is initialized.
 * Call this before any Signal operations.
 *
 * @param {string} cryptoWorkerUrl - URL to crypto SharedWorker (signal_crypto.js)
 * @param {string} wasmJsUrl - URL to libsignal_wasm.js
 * @param {string} wasmBinaryUrl - URL to libsignal_wasm_bg.wasm
 */
export async function ensureSignalReady(cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl) {
  await bridge.init({ cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl })
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
 * Parse binary PreKeyBundle format from CLI.
 *
 * Binary format (1813 bytes total):
 * - version: 1 byte
 * - registration_id: 4 bytes (LE)
 * - identity_key: 33 bytes
 * - signed_prekey_id: 4 bytes (LE)
 * - signed_prekey: 33 bytes
 * - signed_prekey_signature: 64 bytes
 * - prekey_id: 4 bytes (LE)
 * - prekey: 33 bytes
 * - kyber_prekey_id: 4 bytes (LE)
 * - kyber_prekey: 1569 bytes
 * - kyber_prekey_signature: 64 bytes
 */
function parseBinaryBundle(bytes) {
  const VERSION_OFFSET = 0
  const REGISTRATION_ID_OFFSET = 1
  const IDENTITY_KEY_OFFSET = 5
  const SIGNED_PREKEY_ID_OFFSET = 38
  const SIGNED_PREKEY_OFFSET = 42
  const SIGNED_PREKEY_SIG_OFFSET = 75
  const PREKEY_ID_OFFSET = 139
  const PREKEY_OFFSET = 143
  const KYBER_PREKEY_ID_OFFSET = 176
  const KYBER_PREKEY_OFFSET = 180
  const KYBER_PREKEY_SIG_OFFSET = 1749
  const TOTAL_SIZE = 1813

  if (bytes.length !== TOTAL_SIZE) {
    throw new Error(
      `Invalid bundle size: ${bytes.length}, expected ${TOTAL_SIZE}`
    )
  }

  const bundle = {
    version: bytes[VERSION_OFFSET],
    registration_id: readU32LE(bytes, REGISTRATION_ID_OFFSET),
    identity_key: bytesToBase64(
      bytes.slice(IDENTITY_KEY_OFFSET, IDENTITY_KEY_OFFSET + 33)
    ),
    signed_prekey_id: readU32LE(bytes, SIGNED_PREKEY_ID_OFFSET),
    signed_prekey: bytesToBase64(
      bytes.slice(SIGNED_PREKEY_OFFSET, SIGNED_PREKEY_OFFSET + 33)
    ),
    signed_prekey_signature: bytesToBase64(
      bytes.slice(SIGNED_PREKEY_SIG_OFFSET, SIGNED_PREKEY_SIG_OFFSET + 64)
    ),
    prekey_id: readU32LE(bytes, PREKEY_ID_OFFSET),
    prekey: bytesToBase64(bytes.slice(PREKEY_OFFSET, PREKEY_OFFSET + 33)),
    kyber_prekey_id: readU32LE(bytes, KYBER_PREKEY_ID_OFFSET),
    kyber_prekey: bytesToBase64(
      bytes.slice(KYBER_PREKEY_OFFSET, KYBER_PREKEY_OFFSET + 1569)
    ),
    kyber_prekey_signature: bytesToBase64(
      bytes.slice(KYBER_PREKEY_SIG_OFFSET, KYBER_PREKEY_SIG_OFFSET + 64)
    ),
  }

  if (bundle.prekey_id === 0) {
    bundle.prekey_id = null
    bundle.prekey = null
  }

  return bundle
}

/**
 * Parse PreKeyBundle from URL fragment.
 * Expected format: #<base32_binary>
 *
 * @returns {Object|null} Parsed bundle with hub_id and device_id, or null if no valid bundle
 */
export function parseBundleFromFragment() {
  const hash = window.location.hash

  if (!hash) {
    return null
  }

  const base32Data = hash.startsWith("#") ? hash.slice(1) : hash
  if (!base32Data || base32Data.length < 100) {
    return null
  }

  try {
    const bytes = base32Decode(base32Data)
    const bundle = parseBinaryBundle(bytes)

    const hubMatch = window.location.pathname.match(/\/hubs\/([^\/]+)/)
    bundle.hub_id = hubMatch ? hubMatch[1] : ""
    bundle.device_id = 1

    return bundle
  } catch (error) {
    console.error("[Signal] Failed to parse bundle from fragment:", error)
    return null
  }
}
