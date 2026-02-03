/**
 * WebRTC Transport Worker
 *
 * Regular Worker that handles WebRTC DataChannel connections.
 * NO cryptographic operations - all crypto is handled by the main thread via
 * the crypto SharedWorker.
 *
 * This worker handles:
 * - WebRTC signaling (SDP offer/answer, ICE candidates)
 * - DataChannel creation and message routing
 * - Connection state management
 *
 * Same API as signal.js for 1:1 transport swap.
 */

// =============================================================================
// Connection Pool
// =============================================================================

// Connection pool: hubId -> { pc, dataChannel, state, refCount, closeTimer, config }
const connections = new Map()

// Subscription map: subscriptionId -> { hubId, dataChannel }
const subscriptions = new Map()

// Grace period before closing idle connections
const CONNECTION_CLOSE_DELAY_MS = 2000

// Subscription ID counter
let subscriptionIdCounter = 0
function generateSubscriptionId() {
  return `sub_${++subscriptionIdCounter}_${Date.now()}`
}

// =============================================================================
// Message Handler
// =============================================================================

self.onmessage = async (event) => {
  const { id, action, ...params } = event.data

  // Handle pong (heartbeat response from main thread)
  if (action === "pong") {
    return
  }

  try {
    let result

    switch (action) {
      case "init":
        result = { initialized: true }
        break
      case "connect":
        result = await handleConnect(params.hubId, params.signalingUrl, params.browserIdentity)
        break
      case "disconnect":
        result = await handleDisconnect(params.hubId)
        break
      case "subscribe":
        result = await handleSubscribe(params.hubId, params.channel, params.params)
        break
      case "unsubscribe":
        result = await handleUnsubscribe(params.subscriptionId)
        break
      case "sendRaw":
        result = await handleSendRaw(params.subscriptionId, params.message)
        break
      case "handleAnswer":
        result = await handleAnswer(params.hubId, params.sdp)
        break
      case "handleIce":
        result = await handleIceCandidate(params.hubId, params.candidate)
        break
      default:
        throw new Error(`Unknown action: ${action}`)
    }

    self.postMessage({ id, success: true, result })
  } catch (error) {
    console.error("[WebRTCWorker] Error:", action, error)
    self.postMessage({ id, success: false, error: error.message })
  }
}

// =============================================================================
// Connection Handlers
// =============================================================================

async function handleConnect(hubId, signalingUrl, browserIdentity) {
  let conn = connections.get(hubId)

  if (conn) {
    // Cancel any pending close timer
    if (conn.closeTimer) {
      clearTimeout(conn.closeTimer)
      conn.closeTimer = null
    }
    conn.refCount++
    return { state: conn.state, refCount: conn.refCount }
  }

  // Fetch ICE server configuration
  const iceConfig = await fetchIceConfig(signalingUrl, hubId)

  // Create peer connection
  const pc = new RTCPeerConnection({ iceServers: iceConfig.ice_servers })

  conn = {
    pc,
    dataChannel: null,
    state: "connecting",
    refCount: 1,
    hubId,
    browserIdentity,
    signalingUrl,
    pendingCandidates: [],
    closeTimer: null
  }
  connections.set(hubId, conn)

  // Set up ICE candidate handling
  pc.onicecandidate = async (event) => {
    if (event.candidate) {
      await sendSignal(signalingUrl, hubId, browserIdentity, {
        signal_type: "ice",
        candidate: event.candidate.toJSON()
      })
    }
  }

  // Set up connection state handling
  pc.onconnectionstatechange = () => {
    const state = pc.connectionState
    conn.state = state

    self.postMessage({
      event: "connection:state",
      hubId,
      state
    })

    if (state === "failed" || state === "disconnected" || state === "closed") {
      // Emit disconnection event
      for (const [subId, sub] of subscriptions) {
        if (sub.hubId === hubId) {
          self.postMessage({
            event: "subscription:disconnected",
            subscriptionId: subId
          })
        }
      }
    }
  }

  // Create data channel (browser initiates)
  const dataChannel = pc.createDataChannel("relay", {
    ordered: true  // SCTP provides reliable ordered delivery
  })

  conn.dataChannel = dataChannel
  setupDataChannel(hubId, dataChannel)

  // Create and send offer
  const offer = await pc.createOffer()
  await pc.setLocalDescription(offer)

  await sendSignal(signalingUrl, hubId, browserIdentity, {
    signal_type: "offer",
    sdp: offer.sdp
  })

  // Start polling for answer and ICE candidates
  startSignalPolling(hubId, signalingUrl, browserIdentity)

  return { state: "connecting", refCount: 1 }
}

async function handleDisconnect(hubId) {
  const conn = connections.get(hubId)
  if (!conn) {
    return { refCount: 0, closed: false }
  }

  conn.refCount--

  if (conn.refCount <= 0) {
    // Schedule close after grace period
    if (conn.closeTimer) {
      clearTimeout(conn.closeTimer)
    }

    conn.closeTimer = setTimeout(() => {
      const currentConn = connections.get(hubId)
      if (currentConn && currentConn.refCount <= 0) {
        console.log(`[WebRTCWorker] Closing idle connection to hub ${hubId}`)
        currentConn.pc.close()
        connections.delete(hubId)

        // Clean up subscriptions
        for (const [subId, sub] of subscriptions) {
          if (sub.hubId === hubId) {
            subscriptions.delete(subId)
          }
        }
      }
    }, CONNECTION_CLOSE_DELAY_MS)

    return { refCount: 0, closing: true }
  }

  return { refCount: conn.refCount, closed: false }
}

// =============================================================================
// Signaling Handlers
// =============================================================================

async function handleAnswer(hubId, sdp) {
  const conn = connections.get(hubId)
  if (!conn) {
    throw new Error(`No connection for hub ${hubId}`)
  }

  const answer = new RTCSessionDescription({ type: "answer", sdp })
  await conn.pc.setRemoteDescription(answer)

  // Apply any pending ICE candidates
  for (const candidate of conn.pendingCandidates) {
    await conn.pc.addIceCandidate(candidate)
  }
  conn.pendingCandidates = []

  return { applied: true }
}

async function handleIceCandidate(hubId, candidateData) {
  const conn = connections.get(hubId)
  if (!conn) {
    throw new Error(`No connection for hub ${hubId}`)
  }

  const candidate = new RTCIceCandidate(candidateData)

  if (conn.pc.remoteDescription) {
    await conn.pc.addIceCandidate(candidate)
  } else {
    // Queue until remote description is set
    conn.pendingCandidates.push(candidate)
  }

  return { queued: !conn.pc.remoteDescription }
}

// =============================================================================
// Subscription Handlers
// =============================================================================

async function handleSubscribe(hubId, channelName, channelParams) {
  const conn = connections.get(hubId)
  if (!conn) {
    throw new Error(`No connection for hub ${hubId}`)
  }

  const subscriptionId = generateSubscriptionId()

  // For WebRTC, the DataChannel IS the subscription
  // We send a subscribe message through the channel to tell CLI what we want
  subscriptions.set(subscriptionId, {
    hubId,
    channelName,
    channelParams,
    dataChannel: conn.dataChannel
  })

  // Wait for data channel to be open
  if (conn.dataChannel.readyState !== "open") {
    await waitForDataChannelOpen(conn.dataChannel)
  }

  // Send subscription request through data channel
  const subscribeMsg = {
    type: "subscribe",
    subscriptionId,
    channel: channelName,
    params: channelParams
  }
  conn.dataChannel.send(JSON.stringify(subscribeMsg))

  self.postMessage({
    event: "subscription:confirmed",
    subscriptionId
  })

  return { subscriptionId }
}

async function handleUnsubscribe(subscriptionId) {
  const sub = subscriptions.get(subscriptionId)
  if (!sub) {
    return { unsubscribed: false, reason: "Subscription not found" }
  }

  // Send unsubscribe message
  if (sub.dataChannel?.readyState === "open") {
    sub.dataChannel.send(JSON.stringify({
      type: "unsubscribe",
      subscriptionId
    }))
  }

  subscriptions.delete(subscriptionId)
  return { unsubscribed: true }
}

async function handleSendRaw(subscriptionId, message) {
  const sub = subscriptions.get(subscriptionId)
  if (!sub) {
    throw new Error(`Subscription ${subscriptionId} not found`)
  }

  if (sub.dataChannel?.readyState !== "open") {
    throw new Error("DataChannel not open")
  }

  // Wrap message with subscription ID for routing
  const wrapped = {
    subscriptionId,
    data: message
  }

  // Send as binary if it's an array (encrypted envelope bytes)
  if (Array.isArray(message) || message instanceof Uint8Array) {
    const bytes = message instanceof Uint8Array ? message : new Uint8Array(message)
    // Prepend subscription ID length + subscription ID + data
    const subIdBytes = new TextEncoder().encode(subscriptionId)
    const header = new Uint8Array(2 + subIdBytes.length)
    new DataView(header.buffer).setUint16(0, subIdBytes.length, true)
    header.set(subIdBytes, 2)
    const combined = new Uint8Array(header.length + bytes.length)
    combined.set(header)
    combined.set(bytes, header.length)
    sub.dataChannel.send(combined)
  } else {
    sub.dataChannel.send(JSON.stringify(wrapped))
  }

  return { sent: true }
}

// =============================================================================
// Data Channel Setup
// =============================================================================

function setupDataChannel(hubId, dataChannel) {
  dataChannel.binaryType = "arraybuffer"

  dataChannel.onopen = () => {
    console.log(`[WebRTCWorker] DataChannel open for hub ${hubId}`)
    const conn = connections.get(hubId)
    if (conn) {
      conn.state = "connected"
    }

    self.postMessage({
      event: "connection:state",
      hubId,
      state: "connected"
    })
  }

  dataChannel.onclose = () => {
    console.log(`[WebRTCWorker] DataChannel closed for hub ${hubId}`)
    self.postMessage({
      event: "connection:state",
      hubId,
      state: "disconnected",
      reason: "closed"
    })
  }

  dataChannel.onerror = (error) => {
    console.error(`[WebRTCWorker] DataChannel error for hub ${hubId}:`, error)
    self.postMessage({
      event: "connection:error",
      hubId,
      error: error.message || "DataChannel error"
    })
  }

  dataChannel.onmessage = (event) => {
    handleDataChannelMessage(hubId, event.data)
  }
}

function handleDataChannelMessage(hubId, data) {
  try {
    if (data instanceof ArrayBuffer) {
      // Binary message - extract subscription ID from header
      const bytes = new Uint8Array(data)
      const subIdLen = new DataView(data).getUint16(0, true)
      const subIdBytes = bytes.slice(2, 2 + subIdLen)
      const subscriptionId = new TextDecoder().decode(subIdBytes)
      const payload = bytes.slice(2 + subIdLen)

      self.postMessage({
        event: "subscription:message",
        subscriptionId,
        message: { data: Array.from(payload) }
      })
    } else {
      // JSON message
      const msg = JSON.parse(data)

      if (msg.subscriptionId) {
        self.postMessage({
          event: "subscription:message",
          subscriptionId: msg.subscriptionId,
          message: msg.data || msg
        })
      } else if (msg.type === "health") {
        // Broadcast health to all subscriptions for this hub
        for (const [subId, sub] of subscriptions) {
          if (sub.hubId === hubId) {
            self.postMessage({
              event: "subscription:message",
              subscriptionId: subId,
              message: msg
            })
          }
        }
      }
    }
  } catch (error) {
    console.error("[WebRTCWorker] Failed to parse message:", error)
  }
}

function waitForDataChannelOpen(dataChannel) {
  return new Promise((resolve, reject) => {
    if (dataChannel.readyState === "open") {
      resolve()
      return
    }

    const timeout = setTimeout(() => {
      reject(new Error("DataChannel open timeout"))
    }, 10000)

    const onOpen = () => {
      clearTimeout(timeout)
      dataChannel.removeEventListener("open", onOpen)
      dataChannel.removeEventListener("error", onError)
      resolve()
    }

    const onError = (e) => {
      clearTimeout(timeout)
      dataChannel.removeEventListener("open", onOpen)
      dataChannel.removeEventListener("error", onError)
      reject(e)
    }

    dataChannel.addEventListener("open", onOpen)
    dataChannel.addEventListener("error", onError)
  })
}

// =============================================================================
// HTTP Helpers
// =============================================================================

async function fetchIceConfig(signalingUrl, hubId) {
  const response = await fetch(`${signalingUrl}/hubs/${hubId}/webrtc`, {
    credentials: "include"
  })

  if (!response.ok) {
    throw new Error(`Failed to fetch ICE config: ${response.status}`)
  }

  return response.json()
}

async function sendSignal(signalingUrl, hubId, browserIdentity, signal) {
  const response = await fetch(`${signalingUrl}/hubs/${hubId}/webrtc_signals`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json"
    },
    credentials: "include",
    body: JSON.stringify({
      ...signal,
      browser_identity: browserIdentity
    })
  })

  if (!response.ok) {
    throw new Error(`Failed to send signal: ${response.status}`)
  }

  return response.json()
}

// Polling state: hubId -> { timer, browserIdentity }
const pollingState = new Map()

function startSignalPolling(hubId, signalingUrl, browserIdentity) {
  // Don't start multiple polling loops for same hub
  if (pollingState.has(hubId)) {
    return
  }

  const poll = async () => {
    const conn = connections.get(hubId)
    if (!conn || conn.state === "connected" || conn.state === "closed") {
      // Stop polling once connected or closed
      stopSignalPolling(hubId)
      return
    }

    try {
      const response = await fetch(
        `${signalingUrl}/hubs/${hubId}/webrtc_signals?browser_identity=${encodeURIComponent(browserIdentity)}`,
        { credentials: "include" }
      )

      if (response.ok) {
        const { signals } = await response.json()

        for (const signal of signals) {
          if (signal.type === "answer") {
            console.log("[WebRTCWorker] Received answer via polling")
            await handleAnswer(hubId, signal.sdp)
          } else if (signal.type === "ice") {
            console.log("[WebRTCWorker] Received ICE candidate via polling")
            await handleIceCandidate(hubId, signal.candidate)
          }
        }
      }
    } catch (error) {
      console.warn("[WebRTCWorker] Poll error:", error)
    }

    // Continue polling if still connecting
    const state = pollingState.get(hubId)
    if (state) {
      state.timer = setTimeout(poll, 1000) // Poll every second
    }
  }

  pollingState.set(hubId, { timer: null, browserIdentity })
  poll() // Start immediately
}

function stopSignalPolling(hubId) {
  const state = pollingState.get(hubId)
  if (state?.timer) {
    clearTimeout(state.timer)
  }
  pollingState.delete(hubId)
}
