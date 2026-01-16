import { Controller } from "@hotwired/stimulus"
import { createConsumer } from "@rails/actioncable"

// Handles the agents list in the sidebar
// Subscribes to HubChannel to receive real-time agent updates
export default class extends Controller {
  static targets = ["list", "status"]
  static values = { hubId: Number }

  connect() {
    this.agents = []
    this.selectedAgentId = null
    this.consumer = createConsumer()
    this.subscribe()
  }

  disconnect() {
    this.unsubscribe()
  }

  subscribe() {
    if (!this.hubIdValue) return

    this.subscription = this.consumer.subscriptions.create(
      { channel: "HubChannel", hub_id: this.hubIdValue },
      {
        connected: () => this.handleConnected(),
        disconnected: () => this.handleDisconnected(),
        received: (data) => this.handleReceived(data)
      }
    )
  }

  unsubscribe() {
    if (this.subscription) {
      this.subscription.unsubscribe()
      this.subscription = null
    }
    if (this.consumer) {
      this.consumer.disconnect()
      this.consumer = null
    }
  }

  handleConnected() {
    this.updateStatus("Connected")
    // Request agent list
    this.subscription.perform("request_agents")
  }

  handleDisconnected() {
    this.updateStatus("Disconnected")
    this.agents = []
    this.render()
  }

  handleReceived(data) {
    if (data.type === "agents" || data.type === "agent_list") {
      this.agents = data.agents || []
      this.render()
    } else if (data.type === "agent_selected") {
      this.selectedAgentId = data.id
      this.render()
    }
  }

  updateStatus(text) {
    if (this.hasStatusTarget) {
      this.statusTarget.textContent = text
    }
  }

  render() {
    if (!this.hasListTarget) return

    if (this.agents.length === 0) {
      this.listTarget.innerHTML = `
        <p class="px-2 py-4 text-center text-xs text-zinc-600">No agents running</p>
      `
      return
    }

    this.listTarget.innerHTML = this.agents.map(agent => {
      const isSelected = agent.id === this.selectedAgentId
      const statusColor = this.getStatusColor(agent.status)

      return `
        <button type="button"
                class="w-full flex items-center gap-2 px-2 py-1.5 rounded text-sm transition-colors ${isSelected ? 'bg-zinc-800 text-zinc-100' : 'text-zinc-400 hover:bg-zinc-800/50 hover:text-zinc-200'}"
                data-action="sidebar-agents#selectAgent"
                data-agent-id="${this.escapeAttr(agent.id)}">
          <span class="shrink-0 size-2 rounded-full ${statusColor}"></span>
          <span class="truncate font-mono text-xs">${this.escapeHtml(agent.name || agent.id)}</span>
        </button>
      `
    }).join("")
  }

  selectAgent(event) {
    const agentId = event.currentTarget.dataset.agentId
    if (!agentId || !this.subscription) return

    this.subscription.perform("select_agent", { agent_id: agentId })
    this.selectedAgentId = agentId
    this.render()
  }

  getStatusColor(status) {
    switch (status) {
      case "running":
      case "active":
        return "bg-success-500"
      case "idle":
      case "waiting":
        return "bg-warning-500"
      case "error":
      case "failed":
        return "bg-danger-500"
      default:
        return "bg-zinc-600"
    }
  }

  escapeHtml(text) {
    const div = document.createElement("div")
    div.textContent = text || ""
    return div.innerHTML
  }

  escapeAttr(text) {
    return (text || "").replace(/"/g, "&quot;")
  }
}
