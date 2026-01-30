import { Controller } from "@hotwired/stimulus";
import * as xterm from "@xterm/xterm";
import * as xtermFit from "@xterm/addon-fit";
import { ConnectionManager, TerminalConnection } from "connections";

const Terminal = xterm.Terminal || xterm.default?.Terminal || xterm.default;
const FitAddon =
  xtermFit.FitAddon || xtermFit.default?.FitAddon || xtermFit.default;

/**
 * Terminal Display Controller
 *
 * Handles xterm.js rendering with custom touch scrolling for mobile.
 * xterm.js lacks proper touch support, so we overlay a transparent div
 * to capture touch events. See: https://github.com/xtermjs/xterm.js/issues/5377
 *
 * Uses ConnectionManager to acquire connections directly.
 */
export default class extends Controller {
  static targets = ["container"];

  static values = {
    hubId: String,
    agentIndex: Number,
    ptyIndex: { type: Number, default: 0 },
  };

  // Private fields
  #terminal = null;
  #fitAddon = null;
  #terminalConn = null;
  #unsubscribers = [];
  #touchOverlay = null;
  #keyboardHandler = null;
  #boundHandleResize = null;
  #momentumAnimationId = null;
  #isComposing = false;
  #sentDuringComposition = "";
  #isHandlingAutocorrect = false;

  connect() {
    this.#boundHandleResize = this.#handleResize.bind(this);
    window.addEventListener("resize", this.#boundHandleResize);
    this.#initTerminal();
    this.#initConnections();
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
      window.visualViewport?.removeEventListener(
        "resize",
        this.#keyboardHandler,
      );
      window.visualViewport?.removeEventListener(
        "scroll",
        this.#keyboardHandler,
      );
      this.#keyboardHandler = null;
    }

    this.#unsubscribers.forEach((unsub) => unsub());
    this.#unsubscribers = [];

    this.#terminalConn?.release();
    this.#terminalConn = null;

    this.#terminal?.dispose();
    this.#terminal = null;
  }

  async #initConnections() {
    if (!this.hubIdValue) return;

    const termKey = TerminalConnection.key(
      this.hubIdValue,
      this.agentIndexValue,
      this.ptyIndexValue,
    );

    this.#terminalConn = await ConnectionManager.acquire(
      TerminalConnection,
      termKey,
      {
        hubId: this.hubIdValue,
        agentIndex: this.agentIndexValue,
        ptyIndex: this.ptyIndexValue,
      },
    );

    // Subscribe to terminal events
    this.#unsubscribers.push(
      this.#terminalConn.onOutput((data) => {
        this.#handleMessage({ type: "output", data });
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.on("connected", () => {
        this.#handleConnected();
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onDisconnected(() => {
        this.#handleDisconnected();
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onError((err) => {
        this.#handleError(err);
      }),
    );

    // Always force subscribe to get fresh CLI handshake/scrollback
    try {
      await this.#terminalConn.subscribe({ force: true });
    } catch (e) {
      this.#handleError({ message: e.message });
    }
  }

  // Public actions for touch control buttons
  sendCtrlC() {
    this.#sendInput("\x03");
  }
  sendEnter() {
    this.#sendInput("\r");
  }
  sendEscape() {
    this.#sendInput("\x1b");
  }
  sendTab() {
    this.#sendInput("\t");
  }
  sendArrowUp() {
    this.#sendInput("\x1b[A");
  }
  sendArrowDown() {
    this.#sendInput("\x1b[B");
  }
  sendArrowLeft() {
    this.#sendInput("\x1b[D");
  }
  sendArrowRight() {
    this.#sendInput("\x1b[C");
  }

  // Public API
  clear() {
    this.#terminal?.clear();
  }
  writeln(text) {
    this.#terminal?.writeln(text);
  }
  focus() {
    this.#terminal?.focus();
  }

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

    const container = this.hasContainerTarget
      ? this.containerTarget
      : this.element;
    this.#terminal.open(container);

    requestAnimationFrame(() => this.#fitAddon.fit());

    this.#terminal.onData((data) => {
      // Cancel any momentum scrolling when user starts typing
      if (this.#momentumAnimationId) {
        cancelAnimationFrame(this.#momentumAnimationId);
        this.#momentumAnimationId = null;
      }
      // Skip if we're handling autocorrect directly (prevents double-send)
      if (this.#isHandlingAutocorrect) return;
      // Track what xterm sends during composition for autocorrect handling
      if (this.#isComposing) {
        this.#sentDuringComposition += data;
      }
      this.#sendInput(data);
    });
    container.addEventListener("click", () => this.focus());

    this.#setupTouchScroll();
    this.#setupKeyboardHandler();
    this.#setupMobileAutocorrect();
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
        // Only call focus() - calling click() can cause keyboard to close immediately
        const xtermTextarea = this.#terminal?.element?.querySelector(
          ".xterm-helper-textarea",
        );
        if (xtermTextarea) {
          xtermTextarea.focus();
        }
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

    const container = this.hasContainerTarget
      ? this.containerTarget
      : this.element;
    let isKeyboardOpen = false;
    let debounceTimer = null;

    const handleViewportChange = () => {
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

    // Debounce to avoid rapid fire during keyboard animations
    this.#keyboardHandler = () => {
      if (debounceTimer) clearTimeout(debounceTimer);
      debounceTimer = setTimeout(handleViewportChange, 50);
    };

    window.visualViewport.addEventListener("resize", this.#keyboardHandler);
    window.visualViewport.addEventListener("scroll", this.#keyboardHandler);
  }

  // Connection handlers
  #handleConnected() {
    if (!this.#terminal) return;

    this.#terminal.clear();

    // Send initial dimensions so CLI knows browser size
    // CLI will send scrollback as part of the subscription handshake
    requestAnimationFrame(() => {
      this.#fitAddon?.fit();
      this.#sendResize();
    });
    this.focus();
  }

  #handleDisconnected() {
    this.#terminal?.writeln("\r\n[Disconnected]");
  }

  #handleMessage(message) {
    switch (message.type) {
      case "output":
        this.#terminal?.write(message.data);
        break;
      case "clear":
        this.#terminal?.clear();
        break;
      case "agent_selected":
      case "agent_channel_switched":
      case "pty_channel_switched":
        // Terminal channel is now ready - send resize
        this.#terminal?.clear();
        requestAnimationFrame(() => {
          this.#fitAddon?.fit();
          this.#sendResize();
        });
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

  // I/O helpers
  async #sendInput(data) {
    if (!this.#terminalConn) return;
    await this.#terminalConn.sendInput(data);
  }

  async #sendResize() {
    if (this.#terminalConn && this.#terminal) {
      await this.#terminalConn.sendResize(this.#terminal.cols, this.#terminal.rows);
    }
  }

  #handleResize() {
    this.#fitAddon?.fit();
    this.#sendResize();
  }

  // Mobile Autocorrect/Autocomplete Support (iOS and Android)
  //
  // xterm.js doesn't handle mobile autocorrect. iOS fires `insertReplacementText`
  // but getTargetRanges() returns empty. We use two strategies:
  //
  // 1. Composition events (Android IME, some autocorrect): Track what xterm sends
  //    during composition, delete that amount on compositionend
  //
  // 2. Word-based fallback (iOS): Autocorrect replaces the last word being typed,
  //    so we find the last word in the textarea and delete that many chars
  //
  #setupMobileAutocorrect() {
    // Only enable on mobile/touch devices
    const isMobile =
      /Android|iPhone|iPad|iPod/i.test(navigator.userAgent) ||
      ("ontouchstart" in window && navigator.maxTouchPoints > 0);
    if (!isMobile) return;

    const textarea = this.#terminal?.element?.querySelector(
      ".xterm-helper-textarea",
    );
    if (!textarea) return;

    // Composition events (Android IME)
    textarea.addEventListener("compositionstart", () => {
      this.#isComposing = true;
      this.#sentDuringComposition = "";
    });

    textarea.addEventListener("compositionend", (e) => {
      this.#isComposing = false;
      const deleteCount = this.#sentDuringComposition.length;
      if (deleteCount > 0 && e.data) {
        this.#sendInput("\x7f".repeat(deleteCount) + e.data);
      }
      this.#sentDuringComposition = "";
    });

    // iOS won't send delete events when textarea is empty
    // Keep dummy content in the textarea so iOS always thinks there's something to delete
    const DUMMY_CONTENT = "     "; // 5 spaces

    // Initialize textarea with dummy content on focus
    textarea.addEventListener("focus", () => {
      if (textarea.value.length < 3) {
        textarea.value = DUMMY_CONTENT;
      }
    });

    // Unified beforeinput handler for iOS
    textarea.addEventListener(
      "beforeinput",
      (e) => {
        // Handle delete key
        if (
          e.inputType === "deleteContentBackward" ||
          e.inputType === "deleteContentForward"
        ) {
          e.preventDefault();
          this.#sendInput("\x7f");
          setTimeout(() => {
            if (textarea.value.length < 3) {
              textarea.value = DUMMY_CONTENT;
            }
          }, 0);
          return;
        }

        // Word-delete mode - iOS may use different inputTypes
        if (
          e.inputType === "deleteWordBackward" ||
          e.inputType === "deleteWordForward" ||
          e.inputType === "deleteSoftLineBackward" ||
          e.inputType === "deleteHardLineBackward"
        ) {
          e.preventDefault();
          this.#sendInput("\x17");
          setTimeout(() => {
            if (textarea.value.length < 3) {
              textarea.value = DUMMY_CONTENT;
            }
          }, 0);
          return;
        }

        // Autocorrect/replacement handler
        if (this.#isComposing) return;
        if (e.inputType !== "insertReplacementText") return;

        // Block xterm's onData from double-sending
        this.#isHandlingAutocorrect = true;

        const text = textarea.value;
        const replacement = e.data || "";

        // Detect punctuation replacement (double-space-to-period, etc.)
        // These replace just the trailing space, not a whole word
        const isPunctuationReplacement = /^[.!?,;:]+\s*$/.test(replacement);

        let deleteCount;
        let textToSend;

        if (isPunctuationReplacement) {
          // Double-space-to-period: iOS sends space first, then replacement
          // Terminal has TWO spaces, delete both then add ". "
          e.preventDefault();
          this.#sendInput("\x7f\x7f");
          setTimeout(() => {
            this.#sendInput(". ");
            this.#isHandlingAutocorrect = false;
          }, 50);
          return;
        }

        // Autocomplete: delete the word being replaced
        const words = text.trim().split(/\s+/);
        const wordToReplace = words[words.length - 1] || "";
        deleteCount = wordToReplace.length;
        textToSend = replacement.trimStart();

        if (deleteCount > 0) {
          this.#sendInput("\x7f".repeat(deleteCount));
        }
        // Send replacement with trailing space for next word
        if (textToSend) {
          setTimeout(() => this.#sendInput(textToSend + " "), 50);
        }

        // Clear flag after iOS finishes updating
        setTimeout(() => {
          this.#isHandlingAutocorrect = false;
          this.#terminal?.scrollToBottom();
        }, 100);
      },
      { capture: true },
    );
  }
}
