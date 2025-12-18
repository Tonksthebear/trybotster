import { Controller } from "@hotwired/stimulus"

// Lazy load xterm to avoid blocking if import fails
let Terminal, FitAddon

// WebRTC P2P connection to local CLI for agent management
// Supports two modes:
// - TUI mode: streams the full hub terminal interface
// - GUI mode: shows agent list with individual terminal view (mobile-friendly)
export default class extends Controller {
  static targets = [
    "terminal",       // Terminal container
    "status",         // Connection status indicator
    "connectButton",  // Connect/disconnect button
    "agentList",      // Agent list container (GUI mode)
    "modeToggle",     // Toggle between TUI and GUI modes
    "tuiContainer",   // Terminal panel (expands in TUI mode)
    "guiContainer",   // Agent list panel (GUI mode only)
    "terminalTitle"   // Terminal title text
  ]

  static values = {
    csrfToken: String,
    pollInterval: { type: Number, default: 1000 },
    mode: { type: String, default: "gui" } // "tui" or "gui"
  }

  connect() {
    console.log("WebRTC controller connected")
    this.peerConnection = null
    this.dataChannel = null
    this.terminal = null
    this.fitAddon = null
    this.sessionId = null
    this.pollTimer = null
    this.agents = []
    this.selectedAgentId = null

    this.updateStatus("disconnected", "Not connected")

    // Lazy load xterm
    this.loadXterm()
  }

  async loadXterm() {
    try {
      const xtermModule = await import("@xterm/xterm")
      const fitModule = await import("@xterm/addon-fit")
      Terminal = xtermModule.Terminal || xtermModule.default?.Terminal
      FitAddon = fitModule.FitAddon || fitModule.default?.FitAddon
      console.log("xterm loaded successfully")
    } catch (error) {
      console.error("Failed to load xterm:", error)
    }
  }

  disconnect() {
    this.cleanup()
  }

  // Toggle between TUI and GUI modes
  toggleMode() {
    this.modeValue = this.modeValue === "tui" ? "gui" : "tui"
    this.updateModeDisplay()

    // Request agent list when switching to GUI mode
    if (this.modeValue === "gui" && this.dataChannel?.readyState === "open") {
      this.requestAgentList()
    }
  }

  updateModeDisplay() {
    if (this.hasGuiContainerTarget && this.hasTuiContainerTarget) {
      if (this.modeValue === "tui") {
        // TUI mode: hide agent list, terminal takes full width
        this.guiContainerTarget.classList.add("hidden")
        this.tuiContainerTarget.classList.remove("lg:col-span-3")
        this.tuiContainerTarget.classList.add("lg:col-span-4")
      } else {
        // GUI mode: show agent list, terminal takes 3/4 width
        this.guiContainerTarget.classList.remove("hidden")
        this.tuiContainerTarget.classList.remove("lg:col-span-4")
        this.tuiContainerTarget.classList.add("lg:col-span-3")
      }
    }

    if (this.hasModeToggleTarget) {
      this.modeToggleTarget.textContent = this.modeValue === "tui" ? "Switch to GUI" : "Switch to TUI"
    }

    if (this.hasTerminalTitleTarget) {
      this.terminalTitleTarget.textContent = this.modeValue === "tui" ? "Hub Terminal (TUI)" : "Agent Terminal"
    }

    // Clear and resize terminal when switching modes
    if (this.terminal) {
      this.terminal.clear()
      requestAnimationFrame(() => {
        if (this.fitAddon) {
          this.fitAddon.fit()
          this.sendResize()
        }
      })
    }
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

    this.dataChannel = this.peerConnection.createDataChannel("hub", {
      ordered: true
    })

    this.dataChannel.onopen = () => {
      console.log("Data channel opened")
      this.updateStatus("connected", "Connected to Hub")
      this.connectButtonTarget.textContent = "Disconnect"
      this.connectButtonTarget.disabled = false

      // Initialize terminal
      this.initializeTerminal()

      // Request agent list for GUI mode
      if (this.modeValue === "gui") {
        this.requestAgentList()
      }
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
    const offer = await this.peerConnection.createOffer()
    await this.peerConnection.setLocalDescription(offer)

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

    const completeOffer = this.peerConnection.localDescription
    this.updateStatus("connecting", "Sending offer to server...")

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
          headers: { "X-CSRF-Token": this.csrfTokenValue }
        })

        if (!response.ok) {
          if (response.status === 410) {
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
        // Full TUI screen update (TUI mode)
        if (this.modeValue === "tui" && this.terminal) {
          const binaryString = atob(message.data)
          const bytes = Uint8Array.from(binaryString, c => c.charCodeAt(0))
          const data = new TextDecoder().decode(bytes)
          this.terminal.write(data)
        }
        break

      case "agents":
        // Agent list update (GUI mode)
        console.log("Received agent list:", message.agents)
        this.agents = message.agents
        this.renderAgentList()
        break

      case "agent_output":
        // Individual agent terminal output (GUI mode)
        if (this.modeValue === "gui" && this.terminal && message.id === this.selectedAgentId) {
          const binaryString = atob(message.data)
          const bytes = Uint8Array.from(binaryString, c => c.charCodeAt(0))
          const data = new TextDecoder().decode(bytes)
          this.terminal.write(data)
        }
        break

      case "agent_selected":
        console.log("Agent selected:", message.id)
        this.selectedAgentId = message.id
        this.renderAgentList()
        break

      case "agent_created":
        console.log("Agent created:", message.id)
        this.requestAgentList()
        break

      case "agent_deleted":
        console.log("Agent deleted:", message.id)
        if (this.selectedAgentId === message.id) {
          this.selectedAgentId = null
          if (this.terminal) {
            this.terminal.clear()
          }
        }
        this.requestAgentList()
        break

      case "error":
        console.error("CLI error:", message.message)
        this.showError(message.message)
        break

      default:
        console.log("Unknown message type:", message.type, message)
    }
  }

  // Render the agent list in GUI mode
  renderAgentList() {
    if (!this.hasAgentListTarget) return

    if (this.agents.length === 0) {
      this.agentListTarget.innerHTML = `
        <div class="text-gray-500 text-center py-8">
          <p>No agents running</p>
          <p class="text-sm mt-2">Use the TUI to create agents</p>
        </div>
      `
      return
    }

    const html = this.agents.map(agent => {
      const isSelected = agent.id === this.selectedAgentId
      const issueLabel = agent.issue_number ? `#${agent.issue_number}` : agent.branch_name
      const statusColor = agent.status === "Running" ? "text-green-600" : "text-gray-500"

      return `
        <button
          type="button"
          data-action="click->webrtc#selectAgent"
          data-agent-id="${agent.id}"
          class="w-full text-left px-4 py-3 border-b border-gray-200 hover:bg-gray-50 transition-colors ${isSelected ? 'bg-blue-50 border-l-4 border-l-blue-500' : ''}"
        >
          <div class="flex items-center justify-between">
            <div>
              <span class="font-medium text-gray-900">${agent.repo}</span>
              <span class="text-gray-600 ml-2">${issueLabel}</span>
            </div>
            <span class="${statusColor} text-sm">${agent.status}</span>
          </div>
        </button>
      `
    }).join("")

    this.agentListTarget.innerHTML = html
  }

  // Select an agent to view its terminal
  selectAgent(event) {
    const agentId = event.currentTarget.dataset.agentId
    console.log("Selecting agent:", agentId)

    if (this.terminal) {
      this.terminal.clear()
    }

    this.sendMessage({ type: "select_agent", id: agentId })
  }

  // Request the agent list from CLI
  requestAgentList() {
    this.sendMessage({ type: "list_agents" })
  }

  // Create a new agent
  createAgent(repo, issueNumber) {
    this.sendMessage({
      type: "create_agent",
      repo: repo,
      issue_number: issueNumber
    })
  }

  // Delete an agent
  deleteAgent(agentId, deleteWorktree = false) {
    this.sendMessage({
      type: "delete_agent",
      id: agentId,
      delete_worktree: deleteWorktree
    })
  }

  // Send raw input to selected agent
  sendInput(data) {
    this.sendMessage({
      type: "send_input",
      data: data
    })
  }

  showError(message) {
    // Could show a toast notification here
    console.error("Error from CLI:", message)
  }

  // Touch-friendly control methods for mobile devices
  sendCtrlC() {
    this.sendMessage({ type: "key_press", key: "c", ctrl: true, alt: false, shift: false })
  }

  sendEnter() {
    this.sendMessage({ type: "key_press", key: "Enter", ctrl: false, alt: false, shift: false })
  }

  sendEscape() {
    this.sendMessage({ type: "key_press", key: "Escape", ctrl: false, alt: false, shift: false })
  }

  sendArrowUp() {
    this.sendMessage({ type: "key_press", key: "ArrowUp", ctrl: false, alt: false, shift: false })
  }

  sendArrowDown() {
    this.sendMessage({ type: "key_press", key: "ArrowDown", ctrl: false, alt: false, shift: false })
  }

  sendArrowLeft() {
    this.sendMessage({ type: "key_press", key: "ArrowLeft", ctrl: false, alt: false, shift: false })
  }

  sendArrowRight() {
    this.sendMessage({ type: "key_press", key: "ArrowRight", ctrl: false, alt: false, shift: false })
  }

  initializeTerminal() {
    if (!Terminal) {
      console.error("Terminal not loaded yet, retrying in 100ms...")
      setTimeout(() => this.initializeTerminal(), 100)
      return
    }

    if (this.terminal) {
      return
    }

    console.log("Initializing terminal")
    this.terminal = new Terminal({
      cursorBlink: true,
      disableStdin: false,
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

    requestAnimationFrame(() => {
      this.fitAddon.fit()
      console.log(`Terminal fitted to: ${this.terminal.cols}x${this.terminal.rows}`)
      this.sendResize()
    })

    // Capture keyboard input and send to CLI
    this.terminal.onKey(({ key, domEvent }) => {
      this.sendKeyPress(domEvent)
    })

    // Capture special keys
    this.terminalTarget.addEventListener("keydown", (e) => {
      if (this.shouldCaptureKey(e)) {
        e.preventDefault()
        this.sendKeyPress(e)
      }
    })

    // Resize on window resize
    this.resizeHandler = () => {
      if (this.fitAddon && this.terminal) {
        this.fitAddon.fit()
        this.sendResize()
      }
    }
    window.addEventListener("resize", this.resizeHandler)

    this.terminal.focus()
  }

  sendResize() {
    if (!this.terminal || !this.dataChannel || this.dataChannel.readyState !== "open") return

    this.sendMessage({
      type: "resize",
      rows: this.terminal.rows,
      cols: this.terminal.cols
    })
  }

  shouldCaptureKey(e) {
    if (e.ctrlKey || e.altKey || e.metaKey) return true
    if (["Escape", "Tab", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
         "Home", "End", "PageUp", "PageDown", "Insert", "Delete",
         "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"
        ].includes(e.key)) return true
    return false
  }

  sendKeyPress(domEvent) {
    if (!this.dataChannel || this.dataChannel.readyState !== "open") return

    this.sendMessage({
      type: "key_press",
      key: domEvent.key,
      ctrl: domEvent.ctrlKey,
      alt: domEvent.altKey,
      shift: domEvent.shiftKey
    })
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
    this.agents = []
    this.selectedAgentId = null

    if (this.hasConnectButtonTarget) {
      this.connectButtonTarget.disabled = false
      this.connectButtonTarget.textContent = "Connect to Hub"
    }

    if (this.hasAgentListTarget) {
      this.agentListTarget.innerHTML = ""
    }

    this.updateStatus("disconnected", "Not connected")
  }
}
