import { Controller } from "@hotwired/stimulus"

// Lazy load xterm to avoid blocking if import fails
let Terminal, FitAddon

// WebRTC P2P connection to local CLI for viewing the full hub TUI
// Handles signaling via Rails server, then establishes direct P2P connection
// The browser becomes a remote terminal for the hub - see the full TUI and send keyboard input
export default class extends Controller {
  static targets = ["terminal", "status", "connectButton"]
  static values = {
    csrfToken: String,
    pollInterval: { type: Number, default: 1000 }
  }

  connect() {
    console.log("WebRTC controller connected")
    this.peerConnection = null
    this.dataChannel = null
    this.terminal = null
    this.fitAddon = null
    this.sessionId = null
    this.pollTimer = null

    this.updateStatus("disconnected", "Not connected")

    // Lazy load xterm
    this.loadXterm()
  }

  async loadXterm() {
    try {
      const xtermModule = await import("@xterm/xterm")
      const fitModule = await import("@xterm/addon-fit")
      // xterm exports as default.Terminal, fit exports as FitAddon directly
      Terminal = xtermModule.Terminal || xtermModule.default?.Terminal
      FitAddon = fitModule.FitAddon || fitModule.default?.FitAddon
      console.log("xterm loaded successfully, Terminal:", Terminal, "FitAddon:", FitAddon)
    } catch (error) {
      console.error("Failed to load xterm:", error)
    }
  }

  disconnect() {
    this.cleanup()
  }

  // Called when user clicks "Connect to Hub"
  async startConnection() {
    console.log("startConnection called")

    if (this.peerConnection) {
      console.log("Existing connection, cleaning up")
      this.cleanup()
      return
    }

    this.updateStatus("connecting", "Creating connection...")
    this.connectButtonTarget.disabled = true
    this.connectButtonTarget.textContent = "Connecting..."

    try {
      console.log("Creating peer connection...")
      await this.createPeerConnection()
      console.log("Creating offer...")
      await this.createOffer()
      console.log("Starting polling for answer...")
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
    this.dataChannel = this.peerConnection.createDataChannel("hub", {
      ordered: true
    })

    this.dataChannel.onopen = () => {
      console.log("Data channel opened")
      this.updateStatus("connected", "Connected to Hub - streaming TUI")
      this.connectButtonTarget.textContent = "Disconnect"
      this.connectButtonTarget.disabled = false

      // Initialize terminal immediately for full TUI streaming
      this.initializeTerminal()
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

    console.log("Peer connection created successfully")
  }

  async createOffer() {
    console.log("Creating offer...")
    const offer = await this.peerConnection.createOffer()
    console.log("Offer created, setting local description...")
    await this.peerConnection.setLocalDescription(offer)
    console.log("Local description set, waiting for ICE gathering...")

    // Wait for ICE candidates to be gathered
    await new Promise((resolve) => {
      if (this.peerConnection.iceGatheringState === "complete") {
        console.log("ICE gathering already complete")
        resolve()
      } else {
        const checkState = () => {
          console.log("ICE gathering state:", this.peerConnection.iceGatheringState)
          if (this.peerConnection.iceGatheringState === "complete") {
            resolve()
          }
        }
        this.peerConnection.onicegatheringstatechange = checkState
        setTimeout(() => {
          console.log("ICE gathering timeout, proceeding anyway")
          resolve()
        }, 5000) // Timeout
      }
    })

    // Get the complete offer with ICE candidates
    const completeOffer = this.peerConnection.localDescription
    console.log("Complete offer ready, SDP length:", completeOffer.sdp.length)

    this.updateStatus("connecting", "Sending offer to server...")

    // POST offer to Rails signaling endpoint
    console.log("POSTing offer to server...")
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

    console.log("Server response status:", response.status)
    if (!response.ok) {
      const errorText = await response.text()
      console.error("Server error response:", errorText)
      throw new Error(`Server error: ${response.status}`)
    }

    const data = await response.json()
    console.log("Session created:", data)
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
    switch (message.type) {
      case "screen":
        // Full TUI screen update - decode base64 and write to terminal
        if (this.terminal) {
          // Properly decode base64 → binary → UTF-8
          // atob() returns a binary string, we need to convert to proper UTF-8
          const binaryString = atob(message.data)
          const bytes = Uint8Array.from(binaryString, c => c.charCodeAt(0))
          const data = new TextDecoder().decode(bytes)
          this.terminal.write(data)
        }
        break
      case "error":
        console.error("CLI error:", message.message)
        break
      default:
        console.log("Unknown message type:", message.type, message)
    }
  }

  initializeTerminal() {
    if (!Terminal) {
      console.error("Terminal not loaded yet, retrying in 100ms...")
      setTimeout(() => this.initializeTerminal(), 100)
      return
    }

    if (this.terminal) {
      return // Already initialized
    }

    console.log("Initializing terminal for full TUI streaming")
    this.terminal = new Terminal({
      cursorBlink: true,
      disableStdin: false, // Enable keyboard input
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

    // Let xterm fit to container, then tell CLI to render at that size
    requestAnimationFrame(() => {
      this.fitAddon.fit()
      console.log(`Terminal fitted to: ${this.terminal.cols}x${this.terminal.rows}`)
      this.sendResize()
    })

    // Capture keyboard input and send to CLI
    this.terminal.onKey(({ key, domEvent }) => {
      this.sendKeyPress(domEvent)
    })

    // Also capture special keys that onKey might miss
    this.terminalTarget.addEventListener("keydown", (e) => {
      if (this.shouldCaptureKey(e)) {
        e.preventDefault()
        this.sendKeyPress(e)
      }
    })

    // Resize on window resize - re-fit and notify CLI
    this.resizeHandler = () => {
      if (this.fitAddon && this.terminal) {
        this.fitAddon.fit()
        console.log(`Window resized, terminal now: ${this.terminal.cols}x${this.terminal.rows}`)
        this.sendResize()
      }
    }
    window.addEventListener("resize", this.resizeHandler)

    // Focus terminal for keyboard input
    this.terminal.focus()
  }

  sendResize() {
    if (!this.terminal || !this.dataChannel || this.dataChannel.readyState !== "open") return

    const message = {
      type: "resize",
      rows: this.terminal.rows,
      cols: this.terminal.cols
    }

    console.log(`Sending resize: ${message.cols}x${message.rows}`)
    this.dataChannel.send(JSON.stringify(message))
  }

  shouldCaptureKey(e) {
    // Capture control key combinations and special keys
    if (e.ctrlKey || e.altKey || e.metaKey) return true
    if (["Escape", "Tab", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
         "Home", "End", "PageUp", "PageDown", "Insert", "Delete",
         "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"
        ].includes(e.key)) return true
    return false
  }

  sendKeyPress(domEvent) {
    // Don't send if no connection
    if (!this.dataChannel || this.dataChannel.readyState !== "open") return

    const message = {
      type: "key_press",
      key: domEvent.key,
      ctrl: domEvent.ctrlKey,
      alt: domEvent.altKey,
      shift: domEvent.shiftKey
    }

    this.dataChannel.send(JSON.stringify(message))
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

    if (this.resizeHandler) {
      window.removeEventListener("resize", this.resizeHandler)
      this.resizeHandler = null
    }

    if (this.terminal) {
      this.terminal.dispose()
      this.terminal = null
      this.fitAddon = null
    }

    this.sessionId = null

    if (this.hasConnectButtonTarget) {
      this.connectButtonTarget.disabled = false
      this.connectButtonTarget.textContent = "Connect to Hub"
    }

    this.updateStatus("disconnected", "Not connected")
  }
}
