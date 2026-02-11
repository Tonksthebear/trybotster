import { Controller } from "@hotwired/stimulus"

export default class extends Controller {
  static targets = ["source", "button", "icon", "label"]

  copy() {
    const text = this.sourceTarget.value || this.sourceTarget.textContent
    navigator.clipboard.writeText(text).then(() => {
      this.showSuccess()
    }).catch(() => {
      // Fallback for older browsers
      this.sourceTarget.select()
      document.execCommand("copy")
      this.showSuccess()
    })
  }

  showSuccess() {
    const originalLabel = this.labelTarget.textContent
    const originalIcon = this.iconTarget.innerHTML

    this.labelTarget.textContent = "Copied!"
    this.iconTarget.innerHTML = `<path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 13l4 4L19 7" />`

    setTimeout(() => {
      this.labelTarget.textContent = originalLabel
      this.iconTarget.innerHTML = originalIcon
    }, 2000)
  }
}
