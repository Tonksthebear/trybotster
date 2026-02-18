/**
 * HandshakeManager - E2E handshake protocol for Connection.
 *
 * Manages the browser ↔ CLI handshake that confirms E2E encryption is
 * working after a WebRTC subscription is established. The protocol:
 *
 *   Browser "last":  CLI sends "connected" → browser sends "ack" → complete
 *   CLI "last":      Browser sends "connected" → CLI sends "ack" → complete
 *
 * Owns: sent/complete flags, timeout timer, device name detection.
 * Does not know about Connection — communicates via callbacks.
 */

const HANDSHAKE_TIMEOUT_MS = 8000

export class HandshakeManager {
  #sent = false
  #complete = false
  #timer = null
  #callbacks

  /**
   * @param {Object} callbacks
   * @param {Function} callbacks.sendEncrypted - Send encrypted message via DataChannel
   * @param {Function} callbacks.emit - Emit event on Connection
   * @param {Function} callbacks.onComplete - Called when handshake finishes
   * @param {Function} callbacks.log - Debug logger (receives string)
   */
  constructor(callbacks) {
    this.#callbacks = callbacks
  }

  get sent() { return this.#sent }
  get complete() { return this.#complete }

  /**
   * Send handshake to CLI indicating browser is ready.
   * Called when browser detects CLI is connected (browser is "last").
   */
  send() {
    if (this.#sent || this.#complete) return

    this.#sent = true
    this.#callbacks.log("Sending handshake")

    this.#callbacks.sendEncrypted({
      type: "connected",
      device_name: this.#getDeviceName(),
      timestamp: Date.now(),
    }).catch(err => {
      this.#callbacks.log(`Handshake send failed: ${err.message}`)
      this.#sent = false
    })

    this.#timer = setTimeout(() => {
      if (!this.#complete) {
        this.#callbacks.log("Handshake timeout")
        this.#callbacks.emit("error", {
          reason: "handshake_timeout",
          message: "CLI did not respond to handshake",
        })
      }
    }, HANDSHAKE_TIMEOUT_MS)
  }

  /**
   * Handle incoming handshake from CLI.
   * CLI was "last" to connect — respond with ack.
   */
  handleIncoming(message) {
    this.#callbacks.sendEncrypted({ type: "ack", timestamp: Date.now() })
      .catch(err => this.#callbacks.log(`Ack send failed: ${err.message}`))
    this.#markComplete()
  }

  /**
   * Handle handshake acknowledgment from CLI.
   * CLI confirmed our handshake.
   */
  handleAck(message) {
    if (this.#timer) {
      clearTimeout(this.#timer)
      this.#timer = null
    }
    this.#markComplete()
  }

  /**
   * Reset all handshake state. Called on peer disconnect,
   * force-resubscribe, or CLI disconnection.
   */
  reset() {
    this.#complete = false
    this.#sent = false
    if (this.#timer) {
      clearTimeout(this.#timer)
      this.#timer = null
    }
  }

  #markComplete() {
    if (this.#complete) return
    this.#complete = true
    this.#callbacks.log("Handshake complete")
    this.#callbacks.onComplete()
  }

  #getDeviceName() {
    const ua = navigator.userAgent
    if (ua.includes("iPhone")) return "iPhone"
    if (ua.includes("iPad")) return "iPad"
    if (ua.includes("Android")) return "Android"
    if (ua.includes("Mac")) return "Mac Browser"
    if (ua.includes("Windows")) return "Windows Browser"
    if (ua.includes("Linux")) return "Linux Browser"
    return "Browser"
  }
}
