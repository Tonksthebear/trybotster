import { Controller } from "@hotwired/stimulus"
import bridge from "workers/bridge"
import { ensureMatrixReady, parseBundleFromFragment, parseBundleFromUrl } from "matrix/bundle"

/**
 * Handles the dedicated pairing ceremony at /hubs/:id/pairing#<bundle>.
 *
 * Two entry paths:
 * 1. QR scan: URL contains bundle in hash fragment → show confirmation → pair
 * 2. Paste link: No fragment → show paste input → parse pasted URL → pair
 */
export default class extends Controller {
  static values = { hubId: String, redirectUrl: String }
  static targets = [
    "ready", "loading", "success", "error", "errorMessage",
    "fingerprint", "pairButton", "backLink",
    "pasteLink", "pasteLinkInput", "pasteLinkError"
  ]

  connect() {
    this.bundle = parseBundleFromFragment()

    if (this.bundle) {
      history.replaceState(null, "", location.pathname + location.search)
    }

    this.#initialize()
  }

  async #initialize() {
    try {
      const cryptoWorkerUrl = document.querySelector('meta[name="crypto-worker-url"]')?.content
      const wasmJsUrl = document.querySelector('meta[name="crypto-wasm-js-url"]')?.content
      const wasmBinaryUrl = document.querySelector('meta[name="crypto-wasm-binary-url"]')?.content
      await ensureMatrixReady(cryptoWorkerUrl, wasmJsUrl, wasmBinaryUrl)
    } catch (error) {
      console.error("[Pairing] Failed to initialize crypto:", error)
      this.#showError("Failed to initialize encryption. Please refresh and try again.")
      return
    }

    if (this.bundle) {
      this.#showReadyWithBundle()
    } else {
      this.#showPasteLink()
    }
  }

  handlePaste(event) {
    // Short delay to let the paste value populate the input
    setTimeout(() => this.#processPastedUrl(), 0)
  }

  #processPastedUrl() {
    const input = this.pasteLinkInputTarget.value.trim()
    if (!input) return

    this.#hidePasteLinkError()

    try {
      const bundle = parseBundleFromUrl(input)
      if (!bundle) {
        this.#showPasteLinkError("Invalid connection link. Make sure you copied the full URL including the # part.")
        return
      }

      this.bundle = bundle
      this.bundle.hubId = this.hubIdValue
      this.#showReadyWithBundle()
    } catch (error) {
      console.error("[Pairing] Failed to parse pasted URL:", error)
      this.#showPasteLinkError("Could not read connection code from that link. Please try copying again.")
    }
  }

  #showReadyWithBundle() {
    if (this.hasFingerprintTarget) {
      const fp = this.bundle.identityKey.slice(0, 8)
      this.fingerprintTarget.textContent = fp + "..."
    }
    this.#showReady()
  }

  async pair() {
    this.#showLoading()

    try {
      await bridge.createSession(this.hubIdValue, this.bundle)
      this.#showSuccess()

      setTimeout(() => {
        window.location.href = this.redirectUrlValue
      }, 800)
    } catch (error) {
      console.error("[Pairing] Session creation failed:", error)
      this.#showError(`Pairing failed: ${error.message || "Unknown error"}. Scan the QR code again to get a fresh code.`)
    }
  }

  #showPasteLink() {
    this.pasteLinkTarget.classList.remove("hidden")
    this.backLinkTarget.classList.remove("hidden")
    this.readyTarget.classList.add("hidden")
    this.loadingTarget.classList.add("hidden")
    this.successTarget.classList.add("hidden")
    this.errorTarget.classList.add("hidden")
  }

  #showReady() {
    this.readyTarget.classList.remove("hidden")
    this.backLinkTarget.classList.remove("hidden")
    this.pasteLinkTarget.classList.add("hidden")
    this.loadingTarget.classList.add("hidden")
    this.successTarget.classList.add("hidden")
    this.errorTarget.classList.add("hidden")
  }

  #showLoading() {
    this.readyTarget.classList.add("hidden")
    this.backLinkTarget.classList.add("hidden")
    this.pasteLinkTarget.classList.add("hidden")
    this.loadingTarget.classList.remove("hidden")
    this.successTarget.classList.add("hidden")
    this.errorTarget.classList.add("hidden")
  }

  #showSuccess() {
    this.readyTarget.classList.add("hidden")
    this.backLinkTarget.classList.add("hidden")
    this.pasteLinkTarget.classList.add("hidden")
    this.loadingTarget.classList.add("hidden")
    this.successTarget.classList.remove("hidden")
    this.errorTarget.classList.add("hidden")
  }

  #showError(message) {
    this.readyTarget.classList.add("hidden")
    this.backLinkTarget.classList.add("hidden")
    this.pasteLinkTarget.classList.add("hidden")
    this.loadingTarget.classList.add("hidden")
    this.successTarget.classList.add("hidden")
    this.errorTarget.classList.remove("hidden")
    if (this.hasErrorMessageTarget) {
      this.errorMessageTarget.textContent = message
    }
  }

  #showPasteLinkError(message) {
    this.pasteLinkErrorTarget.textContent = message
    this.pasteLinkErrorTarget.classList.remove("hidden")
  }

  #hidePasteLinkError() {
    this.pasteLinkErrorTarget.classList.add("hidden")
  }
}
