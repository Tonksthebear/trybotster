import { Controller } from "@hotwired/stimulus";

/**
 * Modal Controller - Generic modal dialog management
 *
 * Features:
 * - Show/hide via actions or custom events
 * - Click backdrop to close
 * - Escape key to close
 * - Focus management
 *
 * Usage:
 *   <div data-controller="modal" class="hidden">
 *     <div data-action="click->modal#closeFromBackdrop" class="fixed inset-0 bg-black/50">
 *       <div data-modal-target="content" class="modal-content">
 *         <button data-action="modal#hide">Close</button>
 *       </div>
 *     </div>
 *   </div>
 *
 * Trigger from another controller:
 *   this.dispatch("show", { target: modalElement })
 *   // or dispatch custom event: modalElement.dispatchEvent(new CustomEvent("modal:show"))
 */
export default class extends Controller {
  static targets = ["content"];

  connect() {
    this.boundHandleKeydown = this.handleKeydown.bind(this);
  }

  disconnect() {
    document.removeEventListener("keydown", this.boundHandleKeydown);
  }

  show() {
    this.element.classList.remove("hidden");
    document.addEventListener("keydown", this.boundHandleKeydown);
    document.body.classList.add("overflow-hidden");
    this.focusFirst();
    this.dispatch("opened");
  }

  hide() {
    this.element.classList.add("hidden");
    document.removeEventListener("keydown", this.boundHandleKeydown);
    document.body.classList.remove("overflow-hidden");
    this.dispatch("closed");
  }

  // Close when clicking the backdrop (not the content)
  closeFromBackdrop(event) {
    // Only close if clicking directly on backdrop, not its children
    if (event.target === event.currentTarget) {
      this.hide();
    }
  }

  handleKeydown(event) {
    if (event.key === "Escape") {
      this.hide();
    }
  }

  focusFirst() {
    requestAnimationFrame(() => {
      const focusable = this.element.querySelector(
        "input:not([type='hidden']), textarea, select, button:not([data-action*='hide'])"
      );
      if (focusable) {
        focusable.focus();
      }
    });
  }
}
