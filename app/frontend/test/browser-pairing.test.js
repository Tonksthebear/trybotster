import { describe, expect, it } from "vitest"
import {
  acceptedTranscript,
  authenticationTranscript,
  extractDtlsFingerprint,
  generateBrowserKey,
  pairingTranscript,
  parsePairingBundle,
  verifyHubAcceptance,
} from "../lib/transport/browser_pairing"
import transcriptFixture from "../../../test/fixtures/browser_pairing_transcripts.json"

function base64Url(bytes) {
  let binary = ""
  for (const byte of bytes) binary += String.fromCharCode(byte)
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "")
}

function encodeBase32(bytes) {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
  let bits = 0
  let buffer = 0
  let output = ""
  for (const byte of bytes) {
    buffer = (buffer << 8) | byte
    bits += 8
    while (bits >= 5) {
      bits -= 5
      output += alphabet[(buffer >> bits) & 31]
    }
  }
  if (bits > 0) output += alphabet[(buffer << (5 - bits)) & 31]
  return output
}

function pairingFragment({ version = 7, route = "/hubs/42", expiresAt = 2_000_000_000 } = {}) {
  const routeBytes = new TextEncoder().encode(route)
  const bytes = new Uint8Array(11 + routeBytes.length + 64)
  bytes[0] = version
  let expiry = BigInt(expiresAt)
  for (let index = 8; index >= 1; index -= 1) {
    bytes[index] = Number(expiry & 0xffn)
    expiry >>= 8n
  }
  bytes[9] = routeBytes.length >> 8
  bytes[10] = routeBytes.length & 0xff
  bytes.set(routeBytes, 11)
  bytes.set(new Uint8Array(32).fill(5), 11 + routeBytes.length)
  bytes.set(new Uint8Array(32).fill(9), 11 + routeBytes.length + 32)
  return `#${encodeBase32(bytes)}`
}

describe("browser pairing bundle", () => {
  it("parses the pinned hub key, one-time secret, route, and expiry", () => {
    const bundle = parsePairingBundle(pairingFragment())
    expect(bundle).toMatchObject({
      version: 7,
      route: "/hubs/42",
      expiresAt: 2_000_000_000,
    })
    expect(bundle.hubPublicKey).toEqual(new Uint8Array(32).fill(5))
    expect(bundle.pairingSecret).toEqual(new Uint8Array(32).fill(9))
  })

  it("rejects a changed protocol version", () => {
    expect(() => parsePairingBundle(pairingFragment({ version: 6 })))
      .toThrow("Unsupported pairing protocol version 6")
  })

  it("rejects a route outside the hub namespace", () => {
    expect(() => parsePairingBundle(pairingFragment({ route: "/devices/42" })))
      .toThrow("Invalid pairing route")
  })

  it("builds stable transcripts for Rust verification", () => {
    const record = {
      route: "/hubs/42",
      hubPublicKey: "hub-key",
      publicKey: new Uint8Array([1, 2]),
      credentialId: "credential",
      expiresAt: 123,
    }
    const challenge = {
      challenge: "challenge",
      browser_fingerprint: "sha-256 browser",
      hub_fingerprint: "sha-256 hub",
    }
    expect(pairingTranscript(record, challenge)).toBe(
      "botster-pairing-v1\n/hubs/42\nhub-key\nchallenge\nAQI\n123\nsha-256 browser\nsha-256 hub",
    )
    expect(authenticationTranscript(record, challenge)).toBe(
      "botster-auth-v1\n/hubs/42\nhub-key\nchallenge\nAQI\nsha-256 browser\nsha-256 hub",
    )
    expect(acceptedTranscript(record, {
      challenge: "challenge",
      credential_id: "credential",
      paired: true,
    }, challenge)).toBe(
      "botster-auth-accepted-v1\n/hubs/42\nchallenge\nAQI\ncredential\npair\nsha-256 browser\nsha-256 hub",
    )
  })

  it("matches the shared Rust transcript fixture", () => {
    const record = {
      route: transcriptFixture.route,
      hubPublicKey: transcriptFixture.hub_public_key,
      publicKey: Uint8Array.from(atob(transcriptFixture.browser_public_key), character => character.charCodeAt(0)),
      credentialId: transcriptFixture.credential_id,
      expiresAt: transcriptFixture.expires_at,
    }
    const challenge = {
      challenge: transcriptFixture.challenge,
      browser_fingerprint: transcriptFixture.browser_fingerprint,
      hub_fingerprint: transcriptFixture.hub_fingerprint,
    }
    const response = {
      challenge: transcriptFixture.challenge,
      paired: transcriptFixture.paired,
    }

    expect(pairingTranscript(record, challenge)).toBe(transcriptFixture.pairing_transcript)
    expect(authenticationTranscript(record, challenge)).toBe(transcriptFixture.authentication_transcript)
    expect(acceptedTranscript(record, response, challenge)).toBe(transcriptFixture.accepted_transcript)
  })

  it("normalizes the DTLS fingerprint from SDP", () => {
    expect(extractDtlsFingerprint("v=0\r\na=fingerprint:SHA-256 AA:BB\r\n"))
      .toBe("sha-256 aa:bb")
  })

  it("keeps the browser private key non-extractable", async () => {
    const key = await generateBrowserKey()
    expect(key.privateKey.extractable).toBe(false)
    await expect(crypto.subtle.exportKey("pkcs8", key.privateKey)).rejects.toThrow()
  })

  it("rejects a bad hub signature", async () => {
    const hubKeys = await crypto.subtle.generateKey("Ed25519", true, ["sign", "verify"])
    const hubPublicKey = base64Url(new Uint8Array(await crypto.subtle.exportKey("raw", hubKeys.publicKey)))
    const record = {
      route: "/hubs/42",
      hubPublicKey,
      publicKey: new Uint8Array([1, 2]),
      credentialId: "credential",
    }
    const challenge = {
      challenge: "challenge",
      browser_fingerprint: "sha-256 browser",
      hub_fingerprint: "sha-256 hub",
    }
    const response = {
      protocol: 7,
      challenge: "challenge",
      credential_id: "credential",
      browser_public_key: "AQI",
      hub_public_key: hubPublicKey,
      paired: false,
      signature: base64Url(new Uint8Array(64)),
    }

    await expect(verifyHubAcceptance(record, response, challenge))
      .rejects.toThrow("Hub signature is invalid")
  })
})
