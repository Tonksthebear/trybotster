import { Controller } from "@hotwired/stimulus"

// Bridges Vite-bundled React components into the Stimulus/Turbo lifecycle.
// Usage: <div data-controller="react-mount" data-component="ProofOfLife" data-props='{}'>
//
// The Vite entrypoint registers window.__viteReact with mount/unmount.
// This controller calls them on connect/disconnect, so Turbo navigations
// properly mount and tear down React trees.
export default class extends Controller {
  connect() {
    window.__viteReact?.mount(this.element)
  }

  disconnect() {
    window.__viteReact?.unmount(this.element)
  }
}
