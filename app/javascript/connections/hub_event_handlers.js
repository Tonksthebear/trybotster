/**
 * Hub Event Handlers - Bridge event listener setup for Connection.
 *
 * Sets up hub-level listeners for WebRTC peer state, signaling state,
 * health events, and session lifecycle. Each handler is a named function
 * for readability and testability.
 *
 * Returns an array of unsubscribe functions for cleanup.
 */

import { BrowserStatus, ConnectionState } from "connections/constants"

/**
 * Set up all hub-level bridge event listeners.
 *
 * @param {Object} bridge - WorkerBridge instance
 * @param {string} hubId - Hub identifier
 * @param {Object} cb - Callbacks into Connection (narrow interface)
 * @returns {Function[]} Array of unsubscribe functions
 */
export function setupHubEventListeners(bridge, hubId, cb) {
  const unsubs = []

  // WebRTC peer connection state changes
  unsubs.push(bridge.on("connection:state", (event) => {
    if (event.hubId !== hubId) return
    if (event.state === "disconnected") {
      handlePeerDisconnected(cb)
    } else if (event.state === "connected") {
      handlePeerConnected(event, cb)
    }
  }))

  // Connection mode changes (after ICE restart, path may change)
  unsubs.push(bridge.on("connection:mode", (event) => {
    if (event.hubId !== hubId) return
    cb.setConnectionMode(event.mode)
  }))

  // ActionCable signaling state (connected/disconnected)
  unsubs.push(bridge.on("signaling:state", async (event) => {
    if (event.hubId !== hubId) return
    await handleSignalingState(event, cb)
  }))

  // Health events from ActionCable signaling channel
  unsubs.push(bridge.on("health", (event) => {
    if (event.hubId !== hubId) return
    cb.handleHealthMessage(event)
  }))

  // Session invalid (Olm session desync detected by CLI)
  unsubs.push(bridge.on("session:invalid", (event) => {
    if (event.hubId !== hubId) return
    handleSessionInvalid(event, hubId, bridge, cb)
  }))

  // Session refreshed (ratchet restart succeeded)
  unsubs.push(bridge.on("session:refreshed", async (event) => {
    if (event.hubId !== hubId) return
    await handleSessionRefreshed(event, cb)
  }))

  return unsubs
}

// ========== Individual Handlers ==========

function handlePeerDisconnected(cb) {
  // Preserve session_invalid error state — user must re-pair
  if (cb.isSessionInvalid()) return

  // Clear stale subscription so reconnect triggers a fresh subscribe.
  // Without this, #ensureConnected() sees subscriptionId still set and
  // skips subscribe() — leaving input dead and no snapshot delivered.
  cb.clearStaleSubscription()

  cb.emit("disconnected")

  // Retry with backoff if hub is still online
  if (cb.isCliReachable()) {
    cb.schedulePeerReconnect()
  }
}

function handlePeerConnected(event, cb) {
  cb.cancelPeerReconnect()
  if (event.mode) cb.setConnectionMode(event.mode)

  // Peer is ready — subscribe if we have a pending subscription.
  // connectPeer() returns before the DataChannel opens, so the
  // subscribe() call in #ensureConnected() often fails with
  // "peer not ready" and gets silently deferred. This picks it up.
  if (!cb.hasSubscription()) {
    cb.ensureConnected()
  }
}

async function handleSignalingState(event, cb) {
  if (event.state === "disconnected") {
    // ActionCable went down. Do NOT tear down an existing healthy WebRTC
    // DataChannel — data still flows peer-to-peer without signaling.
    // But mark that signaling is unavailable so we don't attempt new
    // peer connections (they need AC for offer/answer exchange).
    // Only clear the subscription if the DC is also dead.
    try {
      const { dcState } = await cb.probePeerHealth()
      if (dcState !== "open") {
        // DC is also dead — clean up fully
        cb.clearStaleSubscription()
      }
      // If DC is open, leave subscription intact — data still flows
    } catch {
      // Probe failed — assume DC is also dead
      cb.clearStaleSubscription()
    }
    cb.setBrowserStatus(BrowserStatus.DISCONNECTED)
  } else if (event.state === "connected" && cb.getBrowserStatus() === BrowserStatus.DISCONNECTED) {
    // ActionCable reconnected. Check if the existing WebRTC DC is still
    // healthy before deciding what to do.
    cb.setBrowserStatus(BrowserStatus.SUBSCRIBED)

    try {
      const { dcState } = await cb.probePeerHealth()
      if (dcState === "open" && cb.hasSubscription()) {
        // DC is open and subscription exists — existing connection is healthy.
        // Nothing to do; data was flowing the whole time AC was down.
        cb.log("AC reconnected, DC still open — no action needed")
      } else {
        // DC is closed/degraded — use the restored AC to re-establish peer.
        // probePeerHealth already cleaned up the dead peer if needed.
        cb.log(`AC reconnected, DC=${dcState} — re-establishing peer`)
        cb.ensureConnected()
      }
    } catch {
      // Probe failed — try to reconnect
      cb.ensureConnected()
    }
  }
}

function handleSessionInvalid(event, hubId, bridge, cb) {
  if (cb.getErrorCode() === "session_invalid") return
  cb.log(`Session invalid: ${event.message}`)
  cb.disconnectPeer()
  bridge.clearSession(hubId).catch(() => {})
  cb.clearIdentity()
  cb.setError("session_invalid", event.message)
}

async function handleSessionRefreshed(event, cb) {
  cb.log("Session refreshed via ratchet restart")
  cb.clearSessionError()
  await cb.disconnectPeer()
  await cb.ensureConnectedAsync()
}
