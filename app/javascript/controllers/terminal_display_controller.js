import { Controller } from "@hotwired/stimulus";
import * as xterm from "@xterm/xterm";
import * as xtermFit from "@xterm/addon-fit";

// Handle various ESM export styles
const Terminal = xterm.Terminal || xterm.default?.Terminal || xterm.default;
const FitAddon = xtermFit.FitAddon || xtermFit.default?.FitAddon || xtermFit.default;

/**
 * Terminal Display Controller - xterm.js rendering
 *
 * This controller handles terminal display only:
 * - Initializes and renders xterm.js
 * - Receives messages via connection controller callbacks
 * - Sends keyboard input via connection outlet
 * - Handles terminal resize
 *
 * Uses connection controller's registerListener API for reliable event handling.
 */
export default class extends Controller {
  static targets = ["container"];

  static outlets = ["connection"];

  connect() {
    this.terminal = null;
    this.fitAddon = null;
    this.connection = null; // Set when connection is ready

    // Initialize terminal immediately
    this.initTerminal();

    // Handle window resize
    this.boundHandleResize = this.handleResize.bind(this);
    window.addEventListener("resize", this.boundHandleResize);
  }

  disconnect() {
    window.removeEventListener("resize", this.boundHandleResize);

    if (this.terminal) {
      this.terminal.dispose();
      this.terminal = null;
    }
  }

  // Called by Stimulus when connection outlet becomes available
  connectionOutletConnected(outlet) {
    outlet.registerListener(this, {
      onConnected: (outlet) => this.handleConnected(outlet),
      onDisconnected: () => this.handleDisconnected(),
      onMessage: (message) => this.handleMessage(message),
      onError: (error) => this.handleError(error),
    });
  }

  // Called by Stimulus when connection outlet is removed
  connectionOutletDisconnected(outlet) {
    outlet.unregisterListener(this);
    this.connection = null;
  }

  initTerminal() {
    this.terminal = new Terminal({
      cursorBlink: true,
      fontFamily: "'JetBrains Mono', 'Fira Code', 'Consolas', monospace",
      fontSize: 14,
      theme: {
        background: "#1a1a1a",
        foreground: "#d4d4d4",
        cursor: "#ffffff",
        selectionBackground: "#3a3a3a",
      },
      allowProposedApi: true,
      scrollback: 10000,
    });

    this.fitAddon = new FitAddon();
    this.terminal.loadAddon(this.fitAddon);

    // Open terminal in container
    const container = this.hasContainerTarget ? this.containerTarget : this.element;
    this.terminal.open(container);

    // Fit to container
    requestAnimationFrame(() => {
      this.fitAddon.fit();
    });

    // Handle terminal input
    this.terminal.onData((data) => {
      this.sendInput(data);
    });

    // Click to focus
    container.addEventListener("click", () => {
      this.focus();
    });

    // Show initial message
    this.terminal.writeln("Secure Terminal (Signal Protocol E2E Encryption)");
    this.terminal.writeln("Connecting...");
    this.terminal.writeln("");
  }

  // Handle connection established
  handleConnected(outlet) {
    this.connection = outlet;
    const hubId = outlet.getHubId();
    this.terminal.writeln(`[Connected to hub: ${hubId.substring(0, 8)}...]`);
    this.terminal.writeln("[Signal E2E encryption active]");
    this.terminal.writeln("");
    // Set GUI mode to receive raw agent PTY output (not TUI)
    this.connection.send("set_mode", { mode: "gui" });
    this.sendResize();
    // Focus terminal so it can receive keyboard input
    this.focus();
  }

  // Handle connection lost
  handleDisconnected() {
    this.terminal.writeln("\r\n[Disconnected]");
    this.connection = null;
  }

  // Handle decrypted messages from CLI
  handleMessage(message) {
    switch (message.type) {
      case "output":
        this.writeOutput(message.data);
        break;

      case "clear":
        this.terminal.clear();
        break;

      case "agent_selected":
        this.terminal.clear();
        break;

      case "scrollback":
        // Write scrollback history to terminal
        // These lines will scroll up into history as new output arrives
        this.writeScrollback(message.lines);
        break;
    }
  }

  // Write scrollback history lines to terminal
  writeScrollback(lines) {
    if (!this.terminal || !lines || lines.length === 0) {
      return;
    }

    // Write each line - they'll be in the scrollback buffer
    for (const line of lines) {
      this.terminal.writeln(line);
    }
  }

  // Handle connection errors
  handleError(error) {
    this.terminal.writeln(`\r\n[Error: ${error}]`);
  }

  // Write output to terminal
  writeOutput(data) {
    if (this.terminal && data) {
      this.terminal.write(data);
    }
  }

  // Send input to CLI via connection outlet
  sendInput(data) {
    if (this.connection) {
      this.connection.sendInput(data);
    }
  }

  // Handle window resize
  handleResize() {
    if (this.fitAddon) {
      this.fitAddon.fit();
      this.sendResize();
    }
  }

  // Send resize to CLI
  sendResize() {
    if (this.connection && this.terminal) {
      this.connection.sendResize(this.terminal.cols, this.terminal.rows);
    }
  }

  // Public: Clear terminal
  clear() {
    if (this.terminal) {
      this.terminal.clear();
    }
  }

  // Public: Write line
  writeln(text) {
    if (this.terminal) {
      this.terminal.writeln(text);
    }
  }

  // Public: Focus terminal
  focus() {
    if (this.terminal) {
      this.terminal.focus();
    }
  }

  // Public: Get terminal dimensions
  getDimensions() {
    if (this.terminal) {
      return { cols: this.terminal.cols, rows: this.terminal.rows };
    }
    return { cols: 80, rows: 24 };
  }

  // Mobile touch control actions
  sendCtrlC() {
    this.sendInput("\x03");
  }

  sendEnter() {
    this.sendInput("\r");
  }

  sendEscape() {
    this.sendInput("\x1b");
  }

  sendTab() {
    this.sendInput("\t");
  }

  sendArrowUp() {
    this.sendInput("\x1b[A");
  }

  sendArrowDown() {
    this.sendInput("\x1b[B");
  }

  sendArrowLeft() {
    this.sendInput("\x1b[D");
  }

  sendArrowRight() {
    this.sendInput("\x1b[C");
  }
}
