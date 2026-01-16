import { Controller } from "@hotwired/stimulus"

// Handles the hubs list in the sidebar
// Currently server-rendered, but provides hook for future real-time updates
export default class extends Controller {
  static targets = ["list"]

  connect() {
    // Hub list is server-rendered
    // Future: subscribe to ActionCable for real-time status updates
  }
}
