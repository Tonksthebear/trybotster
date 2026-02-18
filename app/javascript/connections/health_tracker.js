/**
 * HealthTracker - CLI status tracking and health event processing.
 *
 * Manages CLI status transitions (UNKNOWN → OFFLINE → ONLINE → CONNECTED etc.)
 * and triggers side effects: reconnect on inactive→active, handshake on CLI
 * connected, peer teardown on CLI offline.
 *
 * Owns: cliStatus state, transition logic, inactive/active detection.
 * Does not know about Connection — communicates via callbacks.
 */

import { CliStatus, ConnectionState } from "connections/constants"

const CLI_STATUS_MAP = {
  offline: CliStatus.OFFLINE,
  online: CliStatus.ONLINE,
  notified: CliStatus.NOTIFIED,
  connecting: CliStatus.CONNECTING,
  connected: CliStatus.CONNECTED,
  disconnected: CliStatus.DISCONNECTED,
}

const INACTIVE = new Set([CliStatus.UNKNOWN, CliStatus.OFFLINE, CliStatus.DISCONNECTED])
const ACTIVE = new Set([CliStatus.ONLINE, CliStatus.NOTIFIED, CliStatus.CONNECTING, CliStatus.CONNECTED])

export class HealthTracker {
  #cliStatus = CliStatus.UNKNOWN
  #callbacks

  /**
   * @param {Object} callbacks
   * @param {Function} callbacks.emit - Emit event on Connection
   * @param {Function} callbacks.ensureConnected - Trigger peer + subscribe
   * @param {Function} callbacks.disconnectPeer - Tear down WebRTC peer
   * @param {Function} callbacks.setState - Set ConnectionState
   * @param {Function} callbacks.sendHandshake - Send E2E handshake
   * @param {Function} callbacks.resetHandshake - Reset handshake state
   * @param {Function} callbacks.resetPeerReconnect - Reset backoff counter
   * @param {Function} callbacks.getSubscriptionId - Get current subscription ID
   * @param {Function} callbacks.getErrorCode - Get current error code
   * @param {Function} callbacks.getBrowserStatus - Get current browser status
   * @param {Function} callbacks.notifyManager - Notify ConnectionManager subscribers
   * @param {Function} callbacks.log - Debug logger
   */
  constructor(callbacks) {
    this.#callbacks = callbacks
  }

  get cliStatus() { return this.#cliStatus }
  set cliStatus(value) { this.#cliStatus = value }

  /** Whether CLI is reachable (any active status). */
  isCliReachable() {
    return ACTIVE.has(this.#cliStatus)
  }

  /**
   * Handle health message from Rails — updates CLI status and triggers side effects.
   * Hub-wide: { cli: "online" | "offline" } — CLI connected to Rails
   * Per-browser: { cli: "connected" | "disconnected" } — CLI on E2E channel
   */
  handleHealthMessage(message) {
    if (this.#callbacks.getErrorCode() === "unpaired" ||
        this.#callbacks.getErrorCode() === "session_invalid") return

    const newStatus = CLI_STATUS_MAP[message.cli] || this.#cliStatus
    if (newStatus === this.#cliStatus) return

    const prevStatus = this.#cliStatus
    this.#cliStatus = newStatus
    this.#callbacks.log(`CLI status: ${prevStatus} → ${newStatus}`)

    this.#callbacks.emit("cliStatusChange", { status: newStatus, prevStatus })
    this.emitHealthChange()

    // CLI became reachable — ensure peer + subscribe
    if (ACTIVE.has(newStatus) && INACTIVE.has(prevStatus)) {
      this.#callbacks.resetPeerReconnect()
      this.#callbacks.ensureConnected()
    }

    // CLI connected to E2E channel while already subscribed — initiate handshake
    if (newStatus === CliStatus.CONNECTED && prevStatus !== CliStatus.CONNECTED) {
      this.#callbacks.emit("cliConnected")
      if (this.#callbacks.getSubscriptionId()) {
        this.#callbacks.sendHandshake()
      }
    }

    // Hub went offline — tear down WebRTC, keep signaling for health
    if ((newStatus === CliStatus.DISCONNECTED || newStatus === CliStatus.OFFLINE) &&
        !INACTIVE.has(prevStatus)) {
      this.#callbacks.disconnectPeer()
      this.#callbacks.emit("cliDisconnected")
      this.#callbacks.setState(ConnectionState.CLI_DISCONNECTED)
    }
  }

  /**
   * Handle explicit CLI disconnection (server notifies us when CLI unsubscribes).
   */
  handleCliDisconnected() {
    this.#callbacks.resetHandshake()
    const prevStatus = this.#cliStatus
    this.#cliStatus = CliStatus.DISCONNECTED
    this.#callbacks.setState(ConnectionState.CLI_DISCONNECTED)
    this.#callbacks.emit("cliStatusChange", { status: CliStatus.DISCONNECTED, prevStatus })
    this.emitHealthChange()
    this.#callbacks.emit("cliDisconnected")
  }

  /**
   * Emit combined health change event with both browser and CLI status.
   */
  emitHealthChange() {
    this.#callbacks.emit("healthChange", {
      browser: this.#callbacks.getBrowserStatus(),
      cli: this.#cliStatus,
    })
    this.#callbacks.notifyManager({
      type: "health",
      browser: this.#callbacks.getBrowserStatus(),
      cli: this.#cliStatus,
    })
  }
}
