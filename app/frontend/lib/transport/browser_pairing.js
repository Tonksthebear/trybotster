const DB_NAME = "botster-browser-pairing"
const STORE_NAME = "credentials"
const PROTOCOL_VERSION = 7

function bytesToBase64Url(bytes) {
  let binary = ""
  for (const byte of bytes) binary += String.fromCharCode(byte)
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "")
}

function base64UrlToBytes(value) {
  const padded = value.replaceAll("-", "+").replaceAll("_", "/").padEnd(Math.ceil(value.length / 4) * 4, "=")
  return Uint8Array.from(atob(padded), character => character.charCodeAt(0))
}

function decodeBase32(value) {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
  let bits = 0
  let buffer = 0
  const output = []
  for (const character of value.toUpperCase().replace(/=+$/, "")) {
    const index = alphabet.indexOf(character)
    if (index < 0) throw new Error("Invalid pairing fragment")
    buffer = (buffer << 5) | index
    bits += 5
    if (bits >= 8) {
      bits -= 8
      output.push((buffer >> bits) & 0xff)
    }
  }
  return new Uint8Array(output)
}

function readUint64(bytes, offset) {
  let value = 0n
  for (let index = 0; index < 8; index += 1) {
    value = (value << 8n) | BigInt(bytes[offset + index])
  }
  const number = Number(value)
  if (!Number.isSafeInteger(number)) throw new Error("Invalid pairing expiry")
  return number
}

export function parsePairingBundle(fragment) {
  if (!fragment) return null
  const encoded = fragment.startsWith("#") ? fragment.slice(1) : fragment
  const bytes = decodeBase32(encoded)
  if (bytes.length < 75) throw new Error("Pairing bundle is too short")
  const version = bytes[0]
  if (version !== PROTOCOL_VERSION) throw new Error(`Unsupported pairing protocol version ${version}`)
  const expiresAt = readUint64(bytes, 1)
  const routeLength = (bytes[9] << 8) | bytes[10]
  const expectedLength = 11 + routeLength + 64
  if (bytes.length !== expectedLength) throw new Error("Invalid pairing bundle size")
  const route = new TextDecoder("utf-8", { fatal: true }).decode(bytes.slice(11, 11 + routeLength))
  if (!route.startsWith("/hubs/")) throw new Error("Invalid pairing route")
  const keyOffset = 11 + routeLength
  return {
    version,
    expiresAt,
    route,
    hubPublicKey: bytes.slice(keyOffset, keyOffset + 32),
    pairingSecret: bytes.slice(keyOffset + 32),
  }
}

export function parsePairingBundleFromUrl(urlString) {
  const url = new URL(urlString, window.location.origin)
  return parsePairingBundle(url.hash)
}

function openDatabase() {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, 1)
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(STORE_NAME)) {
        request.result.createObjectStore(STORE_NAME, { keyPath: "hubId" })
      }
    }
    request.onsuccess = () => resolve(request.result)
    request.onerror = () => reject(request.error)
  })
}

async function withStore(mode, action) {
  const database = await openDatabase()
  try {
    return await new Promise((resolve, reject) => {
      const transaction = database.transaction(STORE_NAME, mode)
      const request = action(transaction.objectStore(STORE_NAME))
      request.onsuccess = () => resolve(request.result)
      request.onerror = () => reject(request.error)
      transaction.onerror = () => reject(transaction.error)
    })
  } finally {
    database.close()
  }
}

function loadCredential(hubId) {
  return withStore("readonly", store => store.get(String(hubId)))
}

function saveCredential(record) {
  return withStore("readwrite", store => store.put(record))
}

async function credentialId(publicKeyBytes) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", publicKeyBytes))
  return [...digest.slice(0, 16)].map(byte => byte.toString(16).padStart(2, "0")).join("")
}

export async function generateBrowserKey() {
  const keyPair = await crypto.subtle.generateKey("Ed25519", false, ["sign", "verify"])
  const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", keyPair.publicKey))
  const privateKey = keyPair.privateKey
  return { publicKey, privateKey, credentialId: await credentialId(publicKey) }
}

export async function preparePairing(hubId, bundle) {
  const expectedRoute = `/hubs/${hubId}`
  if (bundle.route !== expectedRoute) throw new Error("Pairing route does not match this hub")
  if (bundle.expiresAt < Math.floor(Date.now() / 1000)) throw new Error("Pairing secret expired")
  const existing = await loadCredential(hubId)
  const pinnedKey = bytesToBase64Url(bundle.hubPublicKey)
  if (existing?.hubPublicKey && existing.hubPublicKey !== pinnedKey) {
    throw new Error("Hub public key changed")
  }
  const browserKey = existing || await generateBrowserKey()
  const record = {
    ...browserKey,
    hubId: String(hubId),
    route: bundle.route,
    hubPublicKey: pinnedKey,
    pairingSecret: bytesToBase64Url(bundle.pairingSecret),
    expiresAt: bundle.expiresAt,
  }
  await saveCredential(record)
  return { credentialId: record.credentialId, hubPublicKey: record.hubPublicKey }
}

export async function hasBrowserCredential(hubId) {
  return Boolean(await loadCredential(hubId))
}

export async function getBrowserCredentialId(hubId) {
  return (await loadCredential(hubId))?.credentialId || null
}

export function extractDtlsFingerprint(sdp) {
  const line = String(sdp || "").split(/\r?\n/).find(candidate => candidate.trim().toLowerCase().startsWith("a=fingerprint:"))
  if (!line) return null
  const value = line.trim().slice("a=fingerprint:".length).trim().toLowerCase()
  return value || null
}

function bindingSuffix(challenge) {
  return `${challenge.browser_fingerprint}\n${challenge.hub_fingerprint}`
}

export function pairingTranscript(record, challenge) {
  return `botster-pairing-v1\n${record.route}\n${record.hubPublicKey}\n${challenge.challenge}\n${bytesToBase64Url(record.publicKey)}\n${record.expiresAt}\n${bindingSuffix(challenge)}`
}

export function authenticationTranscript(record, challenge) {
  return `botster-auth-v1\n${record.route}\n${record.hubPublicKey}\n${challenge.challenge}\n${bytesToBase64Url(record.publicKey)}\n${bindingSuffix(challenge)}`
}

export function acceptedTranscript(record, response, challenge) {
  return `botster-auth-accepted-v1\n${record.route}\n${response.challenge}\n${bytesToBase64Url(record.publicKey)}\n${record.credentialId}\n${response.paired ? "pair" : "auth"}\n${bindingSuffix(challenge)}`
}

export async function buildAuthenticationRequest(hubId, challenge, dtlsBinding) {
  const record = await loadCredential(hubId)
  if (!record) throw new Error("Browser credential is missing")
  if (challenge.protocol !== PROTOCOL_VERSION) throw new Error("Authentication protocol changed")
  if (challenge.route !== record.route) throw new Error("Authentication route changed")
  if (challenge.hub_public_key !== record.hubPublicKey) throw new Error("Hub public key changed")
  if (!dtlsBinding?.browserFingerprint || !dtlsBinding?.hubFingerprint) throw new Error("DTLS fingerprint is missing")
  if (challenge.browser_fingerprint !== dtlsBinding?.browserFingerprint) throw new Error("Browser DTLS fingerprint changed")
  if (challenge.hub_fingerprint !== dtlsBinding?.hubFingerprint) throw new Error("Hub DTLS fingerprint changed")
  const browserPublicKey = bytesToBase64Url(record.publicKey)
  if (record.pairingSecret) {
    if (record.expiresAt < Math.floor(Date.now() / 1000)) throw new Error("Pairing secret expired")
    const key = await crypto.subtle.importKey(
      "raw",
      base64UrlToBytes(record.pairingSecret),
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["sign"],
    )
    const proof = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(pairingTranscript(record, challenge)))
    return {
      type: "pair_request",
      protocol: PROTOCOL_VERSION,
      challenge: challenge.challenge,
      browser_public_key: browserPublicKey,
      expires_at: record.expiresAt,
      proof: bytesToBase64Url(new Uint8Array(proof)),
    }
  }
  const signature = await crypto.subtle.sign(
    "Ed25519",
    record.privateKey,
    new TextEncoder().encode(authenticationTranscript(record, challenge)),
  )
  return {
    type: "auth_response",
    protocol: PROTOCOL_VERSION,
    challenge: challenge.challenge,
    browser_public_key: browserPublicKey,
    signature: bytesToBase64Url(new Uint8Array(signature)),
  }
}

export async function verifyHubAcceptance(record, response, challenge) {
  if (response.protocol !== PROTOCOL_VERSION) throw new Error("Authentication protocol changed")
  if (response.challenge !== challenge.challenge) throw new Error("Authentication challenge changed")
  if (response.hub_public_key !== record.hubPublicKey) throw new Error("Hub public key changed")
  if (response.credential_id !== record.credentialId) throw new Error("Browser credential changed")
  if (response.browser_public_key !== bytesToBase64Url(record.publicKey)) throw new Error("Browser public key changed")
  const publicKey = await crypto.subtle.importKey("raw", base64UrlToBytes(record.hubPublicKey), "Ed25519", false, ["verify"])
  const valid = await crypto.subtle.verify(
    "Ed25519",
    publicKey,
    base64UrlToBytes(response.signature),
    new TextEncoder().encode(acceptedTranscript(record, response, challenge)),
  )
  if (!valid) throw new Error("Hub signature is invalid")
}

export async function verifyAuthenticationAccepted(hubId, response, challenge) {
  const record = await loadCredential(hubId)
  if (!record) throw new Error("Browser credential is missing")
  await verifyHubAcceptance(record, response, challenge)
  delete record.pairingSecret
  delete record.expiresAt
  await saveCredential(record)
  return { credentialId: record.credentialId, paired: response.paired }
}

export { PROTOCOL_VERSION }
