/**
 * HubCrypto - thin crypto adapter over SharedWorker bridge.
 *
 * Important: this does NOT own key/session persistence. Existing Olm account
 * and ratchet/session storage remains in workers/olm_crypto.js.
 */

import bridge from "workers/bridge"

export class HubCrypto {
  static hasSession(hubId) {
    return bridge.hasSession(String(hubId))
  }

  static getIdentityKey(hubId) {
    return bridge.getIdentityKey(String(hubId))
  }

  static clearSession(hubId) {
    return bridge.clearSession(String(hubId))
  }

  static clearActiveSession(hubId) {
    return bridge.clearActiveSession(String(hubId))
  }

  static createSession(hubId, bundle) {
    return bridge.createSession(String(hubId), bundle)
  }

  static encryptSignal(hubId, payload) {
    return bridge.encrypt(String(hubId), payload)
  }

  static decryptSignal(hubId, envelope) {
    return bridge.decrypt(String(hubId), envelope)
  }

  static encryptBinary(hubId, bytes) {
    return bridge.encryptBinary(String(hubId), bytes)
  }

  static decryptBinary(hubId, bytes) {
    return bridge.decryptBinary(String(hubId), bytes)
  }
}
