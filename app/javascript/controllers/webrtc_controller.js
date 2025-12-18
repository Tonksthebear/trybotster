import { Controller } from "@hotwired/stimulus"
import { Terminal } from "@xterm/xterm"
import { FitAddon } from "@xterm/addon-fit"

// WebRTC P2P connection to local CLI for viewing running agents
// Handles signaling via Rails server, then establishes direct P2P connection
export default class extends Controller {
  static targets = ["terminal", "status", "agentList", "connectButton"]
  static values = {
    csrfToken: String,
    pollInterval: { type: Number, default: 1000 }
  }

  connect() {
    this.peerConnection = null
    this.dataChannel = null
    this.terminal = null
    this.fitAddon = null
    this.sessionId = null
    this.pollTimer = null
    this.subscribedAgentId = null

    this.updateStatus("disconnected", "Not connected")
  }

  disconnect() {
    this.cleanup()
  }

  // Called when user clicks "Connect to Hub"
  async startConnection() {
    if (this.peerConnection) {
      this.cleanup()
      return
    }

    this.updateStatus("connecting", "Creating connection...")
    this.connectButtonTarget.disabled = true
    this.connectButtonTarget.textContent = "Connecting..."

    try {
      await this.createPeerConnection()
      await this.createOffer()
      this.startPollingForAnswer()
    } catch (error) {
      console.error("Connection failed:", error)
      this.updateStatus("error", `Connection failed: ${error.message}`)
      this.cleanup()
    }
  }

  async createPeerConnection() {
    const config = {
      iceServers: [
        { urls: "stun:stun.l.google.com:19302" },
        { urls: "stun:stun1.l.google.com:19302" }
      ]
    }

    this.peerConnection = new RTCPeerConnection(config)

    // Create data channel for communication with CLI
    this.dataChannel = this.peerConnection.createDataChannel("agents", {
      ordered: true
    })

    this.dataChannel.onopen = () => {
      console.log("Data channel opened")
      this.updateStatus("connected", "Connected to Hub")
      this.connectButtonTarget.textContent = "Disconnect"
      this.connectButtonTarget.disabled = false
      // Request agent list
      this.sendMessage({ type: "get_agents" })
    }

    this.dataChannel.onclose = () => {
      console.log("Data channel closed")
      this.updateStatus("disconnected", "Connection closed")
      this.cleanup()
    }

    this.dataChannel.onmessage = (event) => {
      this.handleMessage(JSON.parse(event.data))
    }

    this.dataChannel.onerror = (error) => {
      console.error("Data channel error:", error)
      this.updateStatus("error", "Data channel error")
    }

    // ICE connection state changes
    this.peerConnection.oniceconnectionstatechange = () => {
      const state = this.peerConnection.iceConnectionState
      console.log("ICE connection state:", state)

      if (state === "failed" || state === "disconnected") {
        this.updateStatus("error", "Connection failed - your network may block P2P")
        this.cleanup()
      }
    }

    // Wait for ICE gathering to complete
    await new Promise((resolve) => {
      if (this.peerConnection.iceGatheringState === "complete") {
        resolve()
      } else {
        this.peerConnection.onicegatheringstatechange = () => {
          if (this.peerConnection.iceGatheringState === "complete") {
            resolve()
          }
        }
        // Timeout after 10 seconds
        setTimeout(resolve, 10000)
      }
    })
  }

  async createOffer() {
    const offer = await this.peerConnection.createOffer()
    await this.peerConnection.setLocalDescription(offer)

    // Wait for ICE candidates to be gathered
    await new Promise((resolve) => {
      if (this.peerConnection.iceGatheringState === "complete") {
        resolve()
      } else {
        const checkState = () => {
          if (this.peerConnection.iceGatheringState === "complete") {
            resolve()
          }
        }
        this.peerConnection.onicegatheringstatechange = checkState
        setTimeout(resolve, 5000) // Timeout
      }
    })

    // Get the complete offer with ICE candidates
    const completeOffer = this.peerConnection.localDescription

    this.updateStatus("connecting", "Sending offer to server...")

    // POST offer to Rails signaling endpoint
    const response = await fetch("/api/webrtc/sessions", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-CSRF-Token": this.csrfTokenValue
      },
      body: JSON.stringify({
        offer: {
          type: completeOffer.type,
          sdp: completeOffer.sdp
        }
      })
    })

    if (!response.ok) {
      throw new Error(`Server error: ${response.status}`)
    }

    const data = await response.json()
    this.sessionId = data.session_id
    this.updateStatus("connecting", "Waiting for CLI to respond...")
  }

  startPollingForAnswer() {
    this.pollTimer = setInterval(async () => {
      try {
        const response = await fetch(`/api/webrtc/sessions/${this.sessionId}`, {
          headers: {
            "X-CSRF-Token": this.csrfTokenValue
          }
        })

        if (!response.ok) {
          if (response.status === 410) {
            // Session expired
            this.updateStatus("error", "Session expired - CLI did not respond")
            this.cleanup()
          }
          return
        }

        const data = await response.json()

        if (data.status === "answered" && data.answer) {
          clearInterval(this.pollTimer)
          this.pollTimer = null

          this.updateStatus("connecting", "Establishing P2P connection...")

          const answer = new RTCSessionDescription({
            type: data.answer.type,
            sdp: data.answer.sdp
          })
          await this.peerConnection.setRemoteDescription(answer)
        }
      } catch (error) {
        console.error("Polling error:", error)
      }
    }, this.pollIntervalValue)

    // Stop polling after 30 seconds if no answer
    setTimeout(() => {
      if (this.pollTimer) {
        this.updateStatus("error", "Timeout - CLI did not respond. Is botster-hub running?")
        this.cleanup()
      }
    }, 30000)
  }

  handleMessage(message) {
    console.log("Received message:", message)

    switch (message.type) {
      case "agents":
        this.renderAgentList(message.agents)
        break
      case "output":
        if (message.agent_id === this.subscribedAgentId && this.terminal) {
          // Decode base64 terminal output
          const data = atob(message.data)
          this.terminal.write(data)
        }
        break
      case "status":
        this.updateAgentStatus(message.agent_id, message.status)
        break
      case "error":
        console.error("CLI error:", message.message)
        break
    }
  }

  renderAgentList(agents) {
    if (!this.hasAgentListTarget) return

    if (agents.length === 0) {
      this.agentListTarget.innerHTML = `
        <p class="text-gray-500 text-sm">No running agents</p>
      `
      return
    }

    this.agentListTarget.innerHTML = agents.map(agent => `
      <button
        type="button"
        class="w-full text-left px-4 py-3 hover:bg-gray-50 border-b border-gray-200 last:border-b-0 ${this.subscribedAgentId === agent.id ? 'bg-green-50' : ''}"
        data-action="click->webrtc#selectAgent"
        data-agent-id="${agent.id}"
        data-agent-repo="${agent.repo}"
        data-agent-issue="${agent.issue}"
      >
        <div class="font-medium text-gray-900">${agent.repo}#${agent.issue}</div>
        <div class="text-sm text-gray-500">${agent.status}</div>
      </button>
    `).join("")
  }

  selectAgent(event) {
    const agentId = event.currentTarget.dataset.agentId

    // Unsubscribe from previous agent
    if (this.subscribedAgentId) {
      this.sendMessage({ type: "unsubscribe", agent_id: this.subscribedAgentId })
    }

    // Subscribe to new agent
    this.subscribedAgentId = agentId
    this.sendMessage({ type: "subscribe", agent_id: agentId })

    // Initialize terminal if needed
    if (!this.terminal && this.hasTerminalTarget) {
      this.initializeTerminal()
    } else if (this.terminal) {
      this.terminal.clear()
    }

    // Re-render to show selection
    this.sendMessage({ type: "get_agents" })
  }

  initializeTerminal() {
    this.terminal = new Terminal({
      cursorBlink: false,
      disableStdin: true, // Read-only for now
      fontSize: 14,
      fontFamily: "Menlo, Monaco, 'Courier New', monospace",
      theme: {
        background: "#1e1e1e",
        foreground: "#d4d4d4"
      }
    })

    this.fitAddon = new FitAddon()
    this.terminal.loadAddon(this.fitAddon)
    this.terminal.open(this.terminalTarget)
    this.fitAddon.fit()

    // Resize on window resize
    window.addEventListener("resize", () => {
      if (this.fitAddon) this.fitAddon.fit()
    })
  }

  updateAgentStatus(agentId, status) {
    // Refresh agent list to show updated status
    this.sendMessage({ type: "get_agents" })
  }

  sendMessage(message) {
    if (this.dataChannel && this.dataChannel.readyState === "open") {
      this.dataChannel.send(JSON.stringify(message))
    }
  }

  updateStatus(state, message) {
    if (!this.hasStatusTarget) return

    const colors = {
      disconnected: "text-gray-500",
      connecting: "text-yellow-600",
      connected: "text-green-600",
      error: "text-red-600"
    }

    const icons = {
      disconnected: "○",
      connecting: "◐",
      connected: "●",
      error: "✕"
    }

    this.statusTarget.className = `text-sm ${colors[state]}`
    this.statusTarget.textContent = `${icons[state]} ${message}`
  }

  cleanup() {
    if (this.pollTimer) {
      clearInterval(this.pollTimer)
      this.pollTimer = null
    }

    if (this.dataChannel) {
      this.dataChannel.close()
      this.dataChannel = null
    }

    if (this.peerConnection) {
      this.peerConnection.close()
      this.peerConnection = null
    }

    if (this.terminal) {
      this.terminal.dispose()
      this.terminal = null
      this.fitAddon = null
    }

    this.sessionId = null
    this.subscribedAgentId = null

    if (this.hasConnectButtonTarget) {
      this.connectButtonTarget.disabled = false
      this.connectButtonTarget.textContent = "Connect to Hub"
    }

    this.updateStatus("disconnected", "Not connected")

    if (this.hasAgentListTarget) {
      this.agentListTarget.innerHTML = ""
    }
  }
}
