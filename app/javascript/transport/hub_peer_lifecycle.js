export class HubPeerLifecycle {
  #callbacks
  #constants

  constructor({ callbacks, constants }) {
    this.#callbacks = callbacks
    this.#constants = constants
  }

  async connect(hubId, conn) {
    const { TransportState } = this.#constants

    const subscription = this.#callbacks.getSignalingSubscription(hubId)
    if (!subscription) throw new Error(`No signaling subscription for hub ${hubId}`)
    if (!conn.signalingConnected) throw new Error(`Signaling not connected for hub ${hubId}`)

    console.debug(`[WebRTCTransport] Creating peer connection for hub ${hubId}`)
    conn.peerSetupStartedAt = performance.now()
    conn.offerSentAt = 0

    const iceConfig = await this.#callbacks.getIceConfig(hubId, conn)
    console.debug(
      `[WebRTCTransport] ICE config ready for hub ${hubId} in ${Math.round(performance.now() - conn.peerSetupStartedAt)}ms`,
    )

    const pc = new RTCPeerConnection({
      iceServers: iceConfig.ice_servers,
      iceCandidatePoolSize: 1,
    })
    conn.pc = pc
    conn.state = TransportState.CONNECTING

    pc.onicecandidate = async (event) => {
      if (!event.candidate) return

      try {
        const envelope = await this.#callbacks.encryptSignal(hubId, {
          type: "ice",
          candidate: event.candidate.toJSON(),
        })
        subscription.perform("signal", { envelope })
      } catch (error) {
        console.error("[WebRTCTransport] Failed to send ICE candidate:", error)
      }
    }

    pc.oniceconnectionstatechange = () => {
      const { ConnectionMode, ICE_RESTART_MAX_ATTEMPTS } = this.#constants
      const state = pc.iceConnectionState
      console.debug(`[WebRTCTransport] ICE connection state: ${state}`)

      if (state === "connected" || state === "completed") {
        conn.iceRestartAttempts = 0
        if (conn.iceRestartTimer) {
          clearTimeout(conn.iceRestartTimer)
          conn.iceRestartTimer = null
        }
        if (conn.iceDisconnectedTimer) {
          clearTimeout(conn.iceDisconnectedTimer)
          conn.iceDisconnectedTimer = null
        }
        if (conn.iceDisrupted) {
          conn.iceDisrupted = false
          this.detectConnectionMode(hubId, conn).then((mode) => {
            this.#callbacks.emit("connection:state", { hubId, state: "connected", mode })
            this.#callbacks.emit("connection:mode", { hubId, mode })
          })
        }
      } else if (state === "failed") {
        conn.mode = ConnectionMode.UNKNOWN
        conn.iceDisrupted = true
        this.#callbacks.emit("connection:mode", { hubId, mode: ConnectionMode.UNKNOWN })
        this.#scheduleIceRestart(hubId, conn)
      } else if (state === "disconnected") {
        console.debug("[WebRTCTransport] ICE disconnected (transient), waiting for recovery or failure")
        if (!conn.iceDisconnectedTimer) {
          conn.iceDisconnectedTimer = setTimeout(() => {
            conn.iceDisconnectedTimer = null
            if (pc.iceConnectionState === "disconnected") {
              console.debug("[WebRTCTransport] ICE stuck disconnected for 5s, cleaning up peer")
              this.cleanupPeer(hubId, conn)
            }
          }, 5_000)
        }
      }
    }

    pc.onconnectionstatechange = () => {
      const { TransportState, ICE_RESTART_MAX_ATTEMPTS } = this.#constants
      const state = pc.connectionState
      console.debug(`[WebRTCTransport] Connection state: ${state}`)

      if (state === "connected") {
        conn.state = TransportState.CONNECTED
        this.detectConnectionMode(hubId, conn).then((mode) => {
          conn.mode = mode
          this.#callbacks.emit("connection:mode", { hubId, mode })
        }).catch(() => {})
      } else if (state === "closed") {
        this.cleanupPeer(hubId, conn)
      } else if (state === "failed") {
        if (conn.iceRestartAttempts >= ICE_RESTART_MAX_ATTEMPTS) {
          console.debug(
            `[WebRTCTransport] Connection failed after ${conn.iceRestartAttempts} ICE restarts, cleaning up peer`,
          )
          this.cleanupPeer(hubId, conn)
        }
      }
    }

    const dataChannel = pc.createDataChannel("relay", { ordered: true })
    conn.dataChannel = dataChannel
    this.#callbacks.setupDataChannel(hubId, dataChannel)

    const offer = await pc.createOffer()
    await pc.setLocalDescription(offer)

    const envelope = await this.#callbacks.encryptSignal(hubId, {
      type: "offer",
      sdp: offer.sdp,
    })
    subscription.perform("signal", { envelope })
    conn.offerSentAt = performance.now()
    console.debug(
      `[WebRTCTransport] Offer sent for hub ${hubId} in ${Math.round(conn.offerSentAt - conn.peerSetupStartedAt)}ms`,
    )
    this.#startPeerSetupTimer(hubId, conn, pc)

    return { state: TransportState.CONNECTING }
  }

  probeHealth(hubId) {
    const conn = this.#callbacks.getConnection(hubId)
    if (!conn?.pc) return { alive: false, pcState: "none", dcState: "none" }

    const pcState = conn.pc.connectionState
    const dcState = conn.dataChannel?.readyState || "none"

    const terminalDead = pcState === "failed" || pcState === "closed" || pcState === "disconnected"
    const stalePeer = pcState === "connected" && dcState !== "open" && dcState !== "connecting"
    const dead = terminalDead || stalePeer

    if (dead) {
      console.debug(`[WebRTCTransport] Probe: peer dead for hub ${hubId} (pc=${pcState}, dc=${dcState}), cleaning up`)
      conn.iceRestartAttempts = 0
      this.cleanupPeer(hubId, conn)
    }

    return { alive: !dead, pcState, dcState }
  }

  disconnectPeer(hubId) {
    const conn = this.#callbacks.getConnection(hubId)
    if (!conn?.pc) return

    console.debug(`[WebRTCTransport] Disconnecting peer for hub ${hubId} (keeping signaling)`)
    this.teardownPeer(conn)
    this.#callbacks.emit("connection:state", { hubId, state: "disconnected" })
  }

  async handleAnswer(hubId, sdp) {
    const conn = this.#callbacks.getConnection(hubId)
    if (!conn?.pc) return

    if (conn.pc.signalingState === "stable") {
      console.debug("[WebRTCTransport] Ignoring stale answer (already in stable state)")
      return
    }

    const answer = new RTCSessionDescription({ type: "answer", sdp })
    await conn.pc.setRemoteDescription(answer)

    for (const candidate of conn.pendingCandidates) {
      await conn.pc.addIceCandidate(candidate)
    }
    conn.pendingCandidates = []
  }

  async handleIceCandidate(hubId, candidateData) {
    const { MAX_PENDING_REMOTE_ICE } = this.#constants
    const conn = this.#callbacks.getConnection(hubId)
    if (!conn) return

    const candidate = new RTCIceCandidate(candidateData)

    if (!conn.pc || !conn.pc.remoteDescription) {
      conn.pendingCandidates.push(candidate)
      if (conn.pendingCandidates.length > MAX_PENDING_REMOTE_ICE) {
        conn.pendingCandidates.shift()
      }
      return
    }

    await conn.pc.addIceCandidate(candidate)
  }

  teardownPeer(conn) {
    const { TransportState, ConnectionMode } = this.#constants

    this.#clearPeerSetupTimer(conn)
    if (conn.iceRestartTimer) {
      clearTimeout(conn.iceRestartTimer)
      conn.iceRestartTimer = null
    }
    if (conn.iceDisconnectedTimer) {
      clearTimeout(conn.iceDisconnectedTimer)
      conn.iceDisconnectedTimer = null
    }

    if (conn.dataChannel) {
      conn.dataChannel.onopen = null
      conn.dataChannel.onclose = null
      conn.dataChannel.onerror = null
      conn.dataChannel.onmessage = null
      conn.dataChannel = null
    }
    if (conn.pc) {
      conn.pc.oniceconnectionstatechange = null
      conn.pc.onconnectionstatechange = null
      conn.pc.onicecandidate = null
      conn.pc.close()
      conn.pc = null
    }

    conn.state = TransportState.DISCONNECTED
    conn.mode = ConnectionMode.UNKNOWN
    conn.iceDisrupted = false
    conn.iceRestartAttempts = 0
    conn.decryptFailures = 0
    conn.pendingCandidates = []
    conn.activeFileTransferIds.clear()
    conn.nextFileTransferId = 0
    conn.lastStalledAt = 0
    conn.peerSetupStartedAt = 0
    conn.offerSentAt = 0
  }

  cleanupPeer(hubId, conn) {
    this.teardownPeer(conn)
    this.#callbacks.emit("connection:state", { hubId, state: "disconnected" })
  }

  async detectConnectionMode(hubId, conn) {
    const { ConnectionMode } = this.#constants
    const { pc } = conn
    if (!pc) return ConnectionMode.UNKNOWN

    try {
      const stats = await pc.getStats()
      let selectedPairId = null
      let localCandidateId = null

      stats.forEach((report) => {
        if (report.type === "transport" && report.selectedCandidatePairId) {
          selectedPairId = report.selectedCandidatePairId
        }
      })

      if (selectedPairId) {
        const pair = stats.get(selectedPairId)
        if (pair) {
          localCandidateId = pair.localCandidateId
        }
      }

      if (localCandidateId) {
        const localCandidate = stats.get(localCandidateId)
        if (localCandidate) {
          const candidateType = localCandidate.candidateType
          console.debug(`[WebRTCTransport] Selected candidate type: ${candidateType}`)
          const mode = candidateType === "relay" ? ConnectionMode.RELAYED : ConnectionMode.DIRECT
          conn.mode = mode
          return mode
        }
      }

      stats.forEach((report) => {
        if (report.type === "candidate-pair" && report.nominated && report.state === "succeeded") {
          const localCandidate = stats.get(report.localCandidateId)
          if (localCandidate) {
            const candidateType = localCandidate.candidateType
            console.debug(`[WebRTCTransport] Nominated candidate type: ${candidateType}`)
            conn.mode = candidateType === "relay" ? ConnectionMode.RELAYED : ConnectionMode.DIRECT
          }
        }
      })

      return conn.mode
    } catch (error) {
      console.error("[WebRTCTransport] Failed to detect connection mode:", error)
      return ConnectionMode.UNKNOWN
    }
  }

  #scheduleIceRestart(hubId, conn) {
    const {
      ICE_RESTART_DELAY_MS,
      ICE_RESTART_MAX_ATTEMPTS,
      ICE_RESTART_BACKOFF_MULTIPLIER,
    } = this.#constants

    if (conn.iceRestartTimer) return
    if (conn.iceRestartAttempts >= ICE_RESTART_MAX_ATTEMPTS) {
      console.debug(`[WebRTCTransport] ICE restart max attempts (${ICE_RESTART_MAX_ATTEMPTS}) reached for hub ${hubId}`)
      return
    }

    const delay = ICE_RESTART_DELAY_MS * Math.pow(ICE_RESTART_BACKOFF_MULTIPLIER, conn.iceRestartAttempts)
    console.debug(
      `[WebRTCTransport] Scheduling ICE restart for hub ${hubId} in ${delay}ms (attempt ${conn.iceRestartAttempts + 1}/${ICE_RESTART_MAX_ATTEMPTS})`,
    )

    conn.iceRestartTimer = setTimeout(() => {
      conn.iceRestartTimer = null
      this.#performIceRestart(hubId, conn)
    }, delay)
  }

  async #performIceRestart(hubId, conn) {
    const { pc } = conn
    if (!pc || pc.connectionState === "closed") return

    conn.iceRestartAttempts++
    console.debug(`[WebRTCTransport] Performing ICE restart for hub ${hubId} (attempt ${conn.iceRestartAttempts})`)

    try {
      pc.restartIce()

      const offer = await pc.createOffer({ iceRestart: true })
      await pc.setLocalDescription(offer)

      const subscription = this.#callbacks.getSignalingSubscription(hubId)
      if (!subscription) {
        console.error("[WebRTCTransport] No signaling subscription for ICE restart")
        return
      }

      const envelope = await this.#callbacks.encryptSignal(hubId, {
        type: "offer",
        sdp: offer.sdp,
      })
      subscription.perform("signal", { envelope })

      console.debug(`[WebRTCTransport] ICE restart offer sent for hub ${hubId}`)
    } catch (error) {
      console.error(`[WebRTCTransport] ICE restart failed for hub ${hubId}:`, error)
    }
  }

  #clearPeerSetupTimer(conn) {
    if (conn?.peerSetupTimer) {
      clearTimeout(conn.peerSetupTimer)
      conn.peerSetupTimer = null
    }
  }

  #startPeerSetupTimer(hubId, conn, pc) {
    const { PEER_SETUP_TIMEOUT_MS } = this.#constants

    this.#clearPeerSetupTimer(conn)
    conn.peerSetupTimer = setTimeout(() => {
      conn.peerSetupTimer = null

      const current = this.#callbacks.getConnection(hubId)
      if (!current || current !== conn || current.pc !== pc) return
      if (current.dataChannel?.readyState === "open") return

      const elapsed = current.peerSetupStartedAt
        ? Math.round(performance.now() - current.peerSetupStartedAt)
        : PEER_SETUP_TIMEOUT_MS

      console.warn(
        `[WebRTCTransport] Peer setup timed out for hub ${hubId} after ${elapsed}ms; cleaning up for retry`,
      )
      this.cleanupPeer(hubId, current)
    }, PEER_SETUP_TIMEOUT_MS)
  }
}
