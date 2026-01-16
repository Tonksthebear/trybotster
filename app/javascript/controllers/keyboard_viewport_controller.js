import { Controller } from "@hotwired/stimulus"

/**
 * Keeps an element pinned above the virtual keyboard on iOS Safari.
 *
 * Uses the Visual Viewport API with the iOS-specific pattern:
 * - Position via `top` + `transform: translateY(-100%)`
 * - Listen to both `resize` and `scroll` events (Safari 15 quirk)
 * - Use requestAnimationFrame for smooth updates
 *
 * Usage:
 *   <div data-controller="keyboard-viewport"
 *        data-keyboard-viewport-threshold-value="100">
 *   </div>
 */
export default class extends Controller {
  static values = {
    threshold: { type: Number, default: 100 } // Min px to consider keyboard "open"
  }

  connect() {
    if (!window.visualViewport) return

    this.pendingUpdate = null
    this.keyboardVisible = false

    this.scheduleUpdate = this.scheduleUpdate.bind(this)

    // Safari 15 doesn't fire resize reliably - need both events
    window.visualViewport.addEventListener("resize", this.scheduleUpdate)
    window.visualViewport.addEventListener("scroll", this.scheduleUpdate)
  }

  disconnect() {
    if (!window.visualViewport) return

    window.visualViewport.removeEventListener("resize", this.scheduleUpdate)
    window.visualViewport.removeEventListener("scroll", this.scheduleUpdate)

    if (this.pendingUpdate) {
      cancelAnimationFrame(this.pendingUpdate)
    }

    this.resetPosition()
  }

  scheduleUpdate() {
    // Debounce with rAF for smooth 60fps updates
    if (this.pendingUpdate) return

    this.pendingUpdate = requestAnimationFrame(() => {
      this.pendingUpdate = null
      this.updatePosition()
    })
  }

  updatePosition() {
    const viewport = window.visualViewport
    const keyboardHeight = window.innerHeight - viewport.height

    if (keyboardHeight > this.thresholdValue) {
      // Keyboard is open - position element above it
      // Using top + translateY pattern that works on iOS Safari
      const topPosition = viewport.offsetTop + viewport.height

      this.element.style.position = "fixed"
      this.element.style.left = "0"
      this.element.style.right = "0"
      this.element.style.bottom = "auto"
      this.element.style.top = `${topPosition}px`
      this.element.style.transform = "translateY(-100%)"
      this.keyboardVisible = true
    } else if (this.keyboardVisible) {
      // Keyboard closed - reset to normal flow
      this.resetPosition()
    }
  }

  resetPosition() {
    this.element.style.position = ""
    this.element.style.left = ""
    this.element.style.right = ""
    this.element.style.bottom = ""
    this.element.style.top = ""
    this.element.style.transform = ""
    this.keyboardVisible = false
  }
}
