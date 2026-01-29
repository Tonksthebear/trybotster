import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static values = {
    message: String,
  };
  initialize() {
    console.log("Test controller initialized", this.isOnNewPage());
    this.messageValue = "Hello, world!";
  }
  connect() {
    console.log("Test controller connected", this.isOnNewPage());
    this.messageValue = "Connected";
  }

  disconnect() {
    console.log("Test controller disconnected", this.isOnNewPage());
    this.messageValue = "Disconnected";
  }

  messageValueChanged() {
    console.log("Message changed:", this.messageValue);
  }

  afterLoad() {
    console.log("Test controller afterLoad", this.isOnNewPage());
  }

  afterRender() {
    console.log("Test controller afterRender", this.isOnNewPage());
  }

  isOnNewPage() {
    const stillPresent = document.querySelector(
      `[data-controller~="${this.identifier}"][data-turbo-permanent]`,
    );
    return !!stillPresent;
  }
}
