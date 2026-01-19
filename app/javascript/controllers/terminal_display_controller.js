import { Controller } from "@hotwired/stimulus";
import * as xterm from "@xterm/xterm";
import * as xtermFit from "@xterm/addon-fit";

const Terminal = xterm.Terminal || xterm.default?.Terminal || xterm.default;
const FitAddon = xtermFit.FitAddon || xtermFit.default?.FitAddon || xtermFit.default;

/**
 * Terminal Display Controller
 *
 * Handles xterm.js rendering with custom touch scrolling for mobile.
 * xterm.js lacks proper touch support, so we overlay a transparent div
 * to capture touch events. See: https://github.com/xtermjs/xterm.js/issues/5377
 */
export default class extends Controller {
  static targets = ["container"];
  static outlets = ["connection"];

  // Private fields
  #terminal = null;
  #fitAddon = null;
  #connection = null;
  #touchOverlay = null;
  #keyboardHandler = null;
  #boundHandleResize = null;
  #momentumAnimationId = null;

  connect() {
    this.#boundHandleResize = this.#handleResize.bind(this);
    window.addEventListener("resize", this.#boundHandleResize);
    this.#initTerminal();
  }

  disconnect() {
    window.removeEventListener("resize", this.#boundHandleResize);

    this.#touchOverlay?.remove();
    this.#touchOverlay = null;

    if (this.#momentumAnimationId) {
      cancelAnimationFrame(this.#momentumAnimationId);
      this.#momentumAnimationId = null;
    }

    if (this.#keyboardHandler) {
      window.visualViewport?.removeEventListener("resize", this.#keyboardHandler);
      window.visualViewport?.removeEventListener("scroll", this.#keyboardHandler);
      this.#keyboardHandler = null;
    }

    this.#terminal?.dispose();
    this.#terminal = null;
  }

  // Stimulus outlet callbacks
  connectionOutletConnected(outlet) {
    outlet.registerListener(this, {
      onConnected: (o) => this.#handleConnected(o),
      onDisconnected: () => this.#handleDisconnected(),
      onMessage: (msg) => this.#handleMessage(msg),
      onError: (err) => this.#handleError(err),
    });
  }

  connectionOutletDisconnected(outlet) {
    outlet.unregisterListener(this);
    this.#connection = null;
  }

  // Public actions for touch control buttons
  sendCtrlC() { this.#sendInput("\x03"); }
  sendEnter() { this.#sendInput("\r"); }
  sendEscape() { this.#sendInput("\x1b"); }
  sendTab() { this.#sendInput("\t"); }
  sendArrowUp() { this.#sendInput("\x1b[A"); }
  sendArrowDown() { this.#sendInput("\x1b[B"); }
  sendArrowLeft() { this.#sendInput("\x1b[D"); }
  sendArrowRight() { this.#sendInput("\x1b[C"); }

  // Public API
  clear() { this.#terminal?.clear(); }
  writeln(text) { this.#terminal?.writeln(text); }
  focus() { this.#terminal?.focus(); }

  getDimensions() {
    return this.#terminal
      ? { cols: this.#terminal.cols, rows: this.#terminal.rows }
      : { cols: 80, rows: 24 };
  }

  // Terminal initialization
  #initTerminal() {
    this.#terminal = new Terminal({
      cursorBlink: true,
      fontFamily: "'JetBrains Mono', 'Fira Code', 'Consolas', monospace",
      fontSize: 14,
      scrollback: 10000,
      theme: {
        background: "#09090b",
        foreground: "#d4d4d4",
        cursor: "#ffffff",
        selectionBackground: "#3a3a3a",
      },
    });

    this.#fitAddon = new FitAddon();
    this.#terminal.loadAddon(this.#fitAddon);

    const container = this.hasContainerTarget ? this.containerTarget : this.element;
    this.#terminal.open(container);

    requestAnimationFrame(() => this.#fitAddon.fit());

    this.#terminal.onData((data) => this.#sendInput(data));
    container.addEventListener("click", () => this.focus());

    this.#setupTouchScroll();
    this.#setupKeyboardHandler();

    this.#terminal.writeln("Secure Terminal (Signal Protocol E2E Encryption)");
    this.#terminal.writeln("Connecting...");
    this.#terminal.writeln("");

    // If connection was established before terminal initialized, write greeting now
    if (this.#connection) {
      this.#writeConnectionGreeting();
    }
  }

  // Touch scrolling with momentum
  #setupTouchScroll() {
    const xtermEl = this.#terminal?.element;
    if (!xtermEl) return;

    const overlay = document.createElement("div");
    overlay.style.cssText = `
      position: absolute;
      inset: 0;
      touch-action: none;
      z-index: 10;
    `;
    xtermEl.style.position = "relative";
    xtermEl.appendChild(overlay);
    this.#touchOverlay = overlay;

    const SENSITIVITY = 15;
    const FRICTION = 0.92;
    const TAP_THRESHOLD = 10;
    const MAX_VELOCITY_SAMPLES = 5;

    let startY = 0;
    let lastY = 0;
    let lastTime = 0;
    let isScrolling = false;
    const velocityHistory = [];

    overlay.addEventListener("touchstart", (e) => {
      if (e.touches.length !== 1) return;

      cancelAnimationFrame(this.#momentumAnimationId);
      velocityHistory.length = 0;
      startY = lastY = e.touches[0].clientY;
      lastTime = performance.now();
      isScrolling = false;
    });

    overlay.addEventListener("touchmove", (e) => {
      if (e.touches.length !== 1) return;

      const now = performance.now();
      const deltaTime = Math.max(now - lastTime, 1);
      const deltaY = e.touches[0].clientY - lastY;
      const totalDelta = Math.abs(e.touches[0].clientY - startY);

      lastY = e.touches[0].clientY;
      lastTime = now;

      if (totalDelta > TAP_THRESHOLD) {
        isScrolling = true;
      }

      if (isScrolling) {
        const lines = Math.round(-deltaY / SENSITIVITY);
        if (lines !== 0) {
          this.#terminal.scrollLines(lines);
        }

        velocityHistory.push({ v: -deltaY / SENSITIVITY / deltaTime, t: now });
        while (velocityHistory.length > MAX_VELOCITY_SAMPLES) {
          velocityHistory.shift();
        }

        e.preventDefault();
      }
    });

    overlay.addEventListener("touchend", (e) => {
      if (!isScrolling) {
        // Pass tap through to terminal for focus/keyboard
        overlay.style.pointerEvents = "none";
        const touch = e.changedTouches[0];
        const target = document.elementFromPoint(touch.clientX, touch.clientY);
        target?.focus?.();
        target?.click?.();
        requestAnimationFrame(() => overlay.style.pointerEvents = "auto");
        return;
      }

      // Calculate weighted velocity from recent samples
      let velocity = 0;
      if (velocityHistory.length > 0) {
        const now = performance.now();
        let totalWeight = 0;
        let weightedSum = 0;

        for (const { v, t } of velocityHistory) {
          const weight = Math.max(0, 1 - (now - t) / 150);
          weightedSum += v * weight;
          totalWeight += weight;
        }

        velocity = totalWeight > 0 ? weightedSum / totalWeight : 0;
      }

      // Momentum animation
      if (Math.abs(velocity) > 0.01) {
        const animate = () => {
          if (Math.abs(velocity) < 0.005) {
            this.#momentumAnimationId = null;
            return;
          }

          const lines = Math.round(velocity * 16);
          if (lines !== 0) {
            this.#terminal.scrollLines(lines);
          }

          velocity *= FRICTION;
          this.#momentumAnimationId = requestAnimationFrame(animate);
        };
        animate();
      }
    });
  }

  // iOS keyboard viewport adjustment
  #setupKeyboardHandler() {
    if (!window.visualViewport) return;

    const container = this.hasContainerTarget ? this.containerTarget : this.element;
    let isKeyboardOpen = false;

    this.#keyboardHandler = () => {
      const vh = window.visualViewport.height;
      const offset = window.visualViewport.offsetTop;
      const rect = container.getBoundingClientRect();
      const availableHeight = vh - (rect.top + window.scrollY - offset);

      if (availableHeight < rect.height && availableHeight > 100) {
        container.style.maxHeight = `${availableHeight - 10}px`;
        isKeyboardOpen = true;
        requestAnimationFrame(() => {
          this.#fitAddon?.fit();
          this.#terminal?.scrollToBottom();
        });
      } else if (isKeyboardOpen) {
        container.style.maxHeight = "";
        isKeyboardOpen = false;
        requestAnimationFrame(() => this.#fitAddon?.fit());
      }
    };

    window.visualViewport.addEventListener("resize", this.#keyboardHandler);
    window.visualViewport.addEventListener("scroll", this.#keyboardHandler);
  }

  // Connection handlers
  #handleConnected(outlet) {
    this.#connection = outlet;

    // Terminal may not be initialized yet (outlet callbacks can fire before connect())
    if (!this.#terminal) {
      // Store connection, will write greeting when terminal initializes
      return;
    }

    this.#writeConnectionGreeting();
  }

  #writeConnectionGreeting() {
    if (!this.#connection || !this.#terminal) return;

    const hubId = this.#connection.getHubId();
    this.#terminal.writeln(`[Connected to hub: ${hubId.substring(0, 8)}...]`);
    this.#terminal.writeln("[Signal E2E encryption active]");
    this.#terminal.writeln("");
    this.#connection.send("set_mode", { mode: "gui" });
    this.#sendResize();
    this.focus();
  }

  #handleDisconnected() {
    this.#terminal?.writeln("\r\n[Disconnected]");
    this.#connection = null;
  }

  #handleMessage(message) {
    switch (message.type) {
      case "output":
        // Decode base64 output from CLI (terminal output is base64 encoded for JSON transport)
        try {
          const bytes = Uint8Array.from(atob(message.data), (c) =>
            c.charCodeAt(0),
          );
          this.#terminal?.write(bytes);
        } catch (e) {
          // Fallback: write as string if not base64 (backwards compatibility)
          this.#terminal?.write(message.data);
        }
        break;
      case "clear":
      case "agent_selected":
        this.#terminal?.clear();
        break;
      case "scrollback":
        this.#writeScrollback(message.data, message.compressed);
        break;
    }
  }

  #handleError(error) {
    // Format error properly - error may be { reason, message } object from connection
    const message =
      typeof error === "object"
        ? error.message || error.reason || JSON.stringify(error)
        : error;
    this.#terminal?.writeln(`\r\n[Error: ${message}]`);
  }

  // Scrollback decompression
  async #writeScrollback(data, compressed) {
    if (!this.#terminal || !data) return;

    try {
      const text = compressed ? await this.#decompress(data) : data;
      this.#terminal.write(text);
    } catch (error) {
      console.error("Failed to decompress scrollback:", error);
      this.#terminal.write(data);
    }
  }

  async #decompress(base64Data) {
    const binaryString = atob(base64Data);
    const bytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) {
      bytes[i] = binaryString.charCodeAt(i);
    }
    const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"));
    return new Response(stream).text();
  }

  // I/O helpers
  #sendInput(data) {
    this.#connection?.sendInput(data);
  }

  #sendResize() {
    if (this.#connection && this.#terminal) {
      this.#connection.sendResize(this.#terminal.cols, this.#terminal.rows);
    }
  }

  #handleResize() {
    this.#fitAddon?.fit();
    this.#sendResize();
  }
}
