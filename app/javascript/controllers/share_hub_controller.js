import { Controller } from "@hotwired/stimulus"
import { ConnectionManager, HubConnection } from "connections"

export default class extends Controller {
  static values = { hubId: String }

  connect() {
    this.modal = document.getElementById("share-hub-modal")

    // Listen for events from modal buttons
    this.handleRetry = () => this.open()
    this.handleCopy = () => this.copyUrl()
    this.element.addEventListener("share-hub:retry", this.handleRetry)
    this.element.addEventListener("share-hub:copy", this.handleCopy)
  }

  disconnect() {
    if (this.unsubscribe) {
      this.unsubscribe()
      this.unsubscribe = null
    }
    this.element.removeEventListener("share-hub:retry", this.handleRetry)
    this.element.removeEventListener("share-hub:copy", this.handleCopy)
  }

  // Helper to find elements in the modal
  find(selector) {
    return this.modal?.querySelector(selector)
  }

  // Called when button is clicked - opens modal and requests code
  async open() {
    this.showLoading()

    try {
      const hub = await ConnectionManager.acquire(HubConnection, this.hubIdValue, {
        hubId: this.hubIdValue
      })

      // Listen for connection code response
      this.unsubscribe = hub.on("connectionCode", (message) => {
        this.handleConnectionCode(message)
      })

      // Send request - button is only enabled when connected
      const sent = await hub.requestConnectionCode()
      if (!sent) {
        this.showError("Failed to send request - not connected")
      }
    } catch (error) {
      console.error("[ShareHub] Failed to request code:", error)
      this.showError(error.message || "Connection failed")
    }
  }

  handleConnectionCode(message) {
    const { url, qr_ascii } = message

    if (!url || !qr_ascii) {
      this.showError("Invalid response from hub")
      return
    }

    // Set QR code ASCII art
    const qrCode = this.find("[data-share-qr]")
    if (qrCode) {
      // Join array of lines into single string
      qrCode.textContent = Array.isArray(qr_ascii) ? qr_ascii.join("\n") : qr_ascii
    }

    // Set URL
    const urlInput = this.find("[data-share-url]")
    if (urlInput) urlInput.value = url

    this.showContent()
  }

  async copyUrl() {
    const urlInput = this.find("[data-share-url]")
    const copyStatus = this.find("[data-share-status]")

    try {
      await navigator.clipboard.writeText(urlInput?.value || "")
      if (copyStatus) {
        copyStatus.textContent = "Copied!"
        copyStatus.classList.remove("text-zinc-600")
        copyStatus.classList.add("text-emerald-400")

        setTimeout(() => {
          copyStatus.textContent = ""
          copyStatus.classList.remove("text-emerald-400")
          copyStatus.classList.add("text-zinc-600")
        }, 2000)
      }
    } catch (error) {
      if (copyStatus) {
        copyStatus.textContent = "Failed to copy"
        copyStatus.classList.add("text-red-400")
      }
    }
  }

  retry() {
    this.open()
  }

  showLoading() {
    this.find("[data-share-loading]")?.classList.remove("hidden")
    this.find("[data-share-error]")?.classList.add("hidden")
    this.find("[data-share-content]")?.classList.add("hidden")
  }

  showError(message) {
    this.find("[data-share-loading]")?.classList.add("hidden")
    this.find("[data-share-error]")?.classList.remove("hidden")
    this.find("[data-share-content]")?.classList.add("hidden")
    const errorMsg = this.find("[data-share-error-message]")
    if (errorMsg) errorMsg.textContent = message
  }

  showContent() {
    this.find("[data-share-loading]")?.classList.add("hidden")
    this.find("[data-share-error]")?.classList.add("hidden")
    this.find("[data-share-content]")?.classList.remove("hidden")
  }
}
