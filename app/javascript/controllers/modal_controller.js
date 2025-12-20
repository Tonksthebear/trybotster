import { Controller } from "@hotwired/stimulus";

// Generic modal controller for show/hide behavior
// Parent controllers dispatch "modal:show" or "modal:hide" events on the modal element
// Business logic should be handled by parent controllers via custom events
export default class extends Controller {
  connect() {
    // Listen for show/hide events from parent controllers
    this.boundShow = () => this.show();
    this.boundHide = () => this.hide();
    this.element.addEventListener("modal:show", this.boundShow);
    this.element.addEventListener("modal:hide", this.boundHide);
  }

  disconnect() {
    this.element.removeEventListener("modal:show", this.boundShow);
    this.element.removeEventListener("modal:hide", this.boundHide);
  }

  show() {
    this.element.classList.remove("hidden");
    this.dispatch("opened");
  }

  hide() {
    this.element.classList.add("hidden");
    this.dispatch("closed");
  }
}
