import { Controller } from "@hotwired/stimulus"

// Handles the hubs list in the sidebar with live Turbo Stream updates
export default class extends Controller {
  static targets = ["list"]
  static values = { currentHub: Number }

  connect() {
    this.applyActiveState()
    // Reapply active state after Turbo Stream updates
    document.addEventListener("turbo:before-stream-render", this.handleStreamRender)
  }

  disconnect() {
    document.removeEventListener("turbo:before-stream-render", this.handleStreamRender)
  }

  handleStreamRender = (event) => {
    const stream = event.target
    if (stream.target === "sidebar_hubs_list") {
      // Schedule active state application after the DOM update
      requestAnimationFrame(() => this.applyActiveState())
    }
  }

  applyActiveState() {
    if (!this.hasListTarget || !this.currentHubValue) return

    const links = this.listTarget.querySelectorAll(".sidebar-hub-link")
    links.forEach(link => {
      const hubId = parseInt(link.dataset.hubId, 10)
      const isActive = hubId === this.currentHubValue

      link.classList.toggle("bg-zinc-800", isActive)
      link.classList.toggle("text-zinc-100", isActive)
      link.classList.toggle("text-zinc-400", !isActive)
      link.classList.toggle("hover:bg-zinc-800/50", !isActive)
      link.classList.toggle("hover:text-zinc-200", !isActive)
    })
  }
}
