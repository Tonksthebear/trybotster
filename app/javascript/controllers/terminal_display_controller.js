import { Controller } from "@hotwired/stimulus";
import { init, Terminal, FitAddon } from "ghostty-web";
import { ConnectionManager, TerminalConnection, HubConnection } from "connections";

/**
 * Terminal Display Controller
 *
 * Renders a terminal using ghostty-web (Ghostty's VT100 parser compiled to WASM).
 * Uses ConnectionManager to acquire connections directly.
 */
export default class extends Controller {
  static targets = ["container"];

  static values = {
    hubId: String,
    agentIndex: Number,
    ptyIndex: { type: Number, default: 0 },
  };

  #terminal = null;
  #fitAddon = null;
  #terminalConn = null;
  #hubConn = null;
  #unsubscribers = [];
  #boundHandleResize = null;
  #resizeDebounceTimer = null;
  #momentumAnimationId = null;
  #keyboardHandler = null;
  #textarea = null;        // ghostty's hidden textarea — we control its focusability
  #isMobile = false;

  connect() {
    this.#boundHandleResize = this.#handleResize.bind(this);
    window.addEventListener("resize", this.#boundHandleResize);
    this.#initTerminal().then(() => this.#initConnections());
  }

  disconnect() {
    window.removeEventListener("resize", this.#boundHandleResize);

    if (this.#resizeDebounceTimer) {
      clearTimeout(this.#resizeDebounceTimer);
      this.#resizeDebounceTimer = null;
    }

    if (this.#momentumAnimationId) {
      cancelAnimationFrame(this.#momentumAnimationId);
      this.#momentumAnimationId = null;
    }

    if (this.#keyboardHandler) {
      window.visualViewport?.removeEventListener("resize", this.#keyboardHandler);
      window.visualViewport?.removeEventListener("scroll", this.#keyboardHandler);
      this.#keyboardHandler = null;
      this.element.style.height = "";
      this.element.style.overflow = "";
    }

    this.#unsubscribers.forEach((unsub) => unsub());
    this.#unsubscribers = [];

    this.#terminalConn?.release();
    this.#terminalConn = null;

    this.#hubConn?.release();
    this.#hubConn = null;

    this.#terminal?.dispose();
    this.#terminal = null;
  }

  async #initTerminal() {
    await init();

    this.#isMobile = "ontouchstart" in window;

    this.#terminal = new Terminal({
      cursorBlink: true,
      fontFamily: "'JetBrainsMono NF', monospace",
      fontSize: 14,
      scrollback: 10000,
      theme: {
        background: "#09090b",
      },
    });

    this.#fitAddon = new FitAddon();
    this.#terminal.loadAddon(this.#fitAddon);

    const container = this.hasContainerTarget
      ? this.containerTarget
      : this.element;
    this.#terminal.open(container);

    // Neutralize ghostty's focusable elements on mobile.
    // ghostty-web creates a hidden <textarea tabindex="0"> and sets
    // contenteditable="true" on the container. iOS Safari focuses these on
    // ANY touch regardless of preventDefault. We make them unfocusable by
    // touch (tabindex=-1) and only focus via script on a deliberate tap.
    if (this.#isMobile) {
      this.#textarea = container.querySelector("textarea");
      if (this.#textarea) {
        this.#textarea.setAttribute("tabindex", "-1");
        // Hide iOS text caret — the terminal canvas cursor is sufficient
        this.#textarea.style.caretColor = "transparent";
        this.#textarea.addEventListener("blur", () => {
          // Re-lock when keyboard dismisses
          this.#textarea?.setAttribute("tabindex", "-1");
        });
      }
      // Remove contenteditable + tabindex from the container
      container.removeAttribute("contenteditable");
      container.setAttribute("tabindex", "-1");

      // Hide the canvas-rendered scrollbar — we handle scroll via touch.
      // showScrollbar lives on the internal renderer, not the Terminal.
      // Walk the object tree to find and patch it.
      this.#disableScrollbar();
    }

    await new Promise((resolve) => {
      requestAnimationFrame(() => {
        this.#fitAddon.fit();
        resolve();
      });
    });

    this.#terminal.onData((data) => {
      // Cancel any momentum scrolling when user starts typing
      if (this.#momentumAnimationId) {
        cancelAnimationFrame(this.#momentumAnimationId);
        this.#momentumAnimationId = null;
      }
      if (this.#isMobile && data === "\r") {
        // iOS keyboard Enter → newline (Shift+Enter equivalent).
        // The touch "Enter" button sends \r directly via sendEnter().
        this.#sendInput("\n");
      } else {
        this.#sendInput(data);
      }
    });

    if (this.#isMobile) {
      this.#setupTouchScroll();
    }
    this.#setupKeyboardHandler();
  }

  // Touch scrolling with tap-to-focus.
  //
  // ghostty-web sets contenteditable + tabindex on its element, so the browser
  // will focus (and open the keyboard) on any touch. We prevent that on
  // touchstart and only call focus() ourselves on a clean tap (short + small).
  #setupTouchScroll() {
    const el = this.#terminal?.element;
    if (!el) return;

    const SENSITIVITY = 15;
    const FRICTION = 0.92;
    const TAP_DISTANCE = 15;   // px — generous for iOS finger jitter
    const TAP_DURATION = 250;  // ms — must be a quick tap, not a hold/scroll
    const MAX_SAMPLES = 5;

    let startY = 0;
    let lastY = 0;
    let lastTime = 0;
    let touchStartTime = 0;
    let isScrolling = false;
    const velocityHistory = [];

    // Use capture phase + stopImmediatePropagation to fully intercept touches
    // before ghostty's internal handlers can focus the textarea.
    el.addEventListener("touchstart", (e) => {
      if (e.touches.length !== 1) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      if (this.#momentumAnimationId) {
        cancelAnimationFrame(this.#momentumAnimationId);
        this.#momentumAnimationId = null;
      }
      velocityHistory.length = 0;
      startY = lastY = e.touches[0].clientY;
      touchStartTime = lastTime = performance.now();
      isScrolling = false;
    }, { passive: false, capture: true });

    el.addEventListener("touchmove", (e) => {
      if (e.touches.length !== 1) return;
      e.stopImmediatePropagation();

      const now = performance.now();
      const dt = Math.max(now - lastTime, 1);
      const dy = e.touches[0].clientY - lastY;
      const totalDelta = Math.abs(e.touches[0].clientY - startY);

      lastY = e.touches[0].clientY;
      lastTime = now;

      if (totalDelta > TAP_DISTANCE) isScrolling = true;

      if (isScrolling) {
        const lines = Math.round(-dy / SENSITIVITY);
        if (lines !== 0) this.#terminal.scrollLines(lines);

        velocityHistory.push({ v: -dy / SENSITIVITY / dt, t: now });
        while (velocityHistory.length > MAX_SAMPLES) velocityHistory.shift();

        e.preventDefault();
      }
    }, { passive: false, capture: true });

    el.addEventListener("touchend", (e) => {
      e.stopImmediatePropagation();
      const duration = performance.now() - touchStartTime;

      if (!isScrolling && duration < TAP_DURATION) {
        // Clean tap — make textarea focusable and focus it to open keyboard.
        // tabindex="0" allows focus + keyboard; blur handler resets to -1.
        if (this.#textarea) {
          this.#textarea.setAttribute("tabindex", "0");
          this.#textarea.focus();
        } else {
          this.#terminal?.focus();
        }
        return;
      }

      // Scroll gesture or long press — apply momentum, don't focus
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

      if (Math.abs(velocity) > 0.01) {
        const animate = () => {
          if (Math.abs(velocity) < 0.005) {
            this.#momentumAnimationId = null;
            return;
          }
          const lines = Math.round(velocity * 16);
          if (lines !== 0) this.#terminal.scrollLines(lines);
          velocity *= FRICTION;
          this.#momentumAnimationId = requestAnimationFrame(animate);
        };
        animate();
      }
    }, { passive: true, capture: true });
  }

  // iOS keyboard viewport adjustment.
  //
  // Resize the entire flex layout (this.element) to match the visual viewport
  // so the terminal naturally shrinks and the touch controls stay visible.
  // This avoids fighting iOS scroll behavior — the page never extends behind
  // the keyboard because the layout simply fits within the visible area.
  #setupKeyboardHandler() {
    if (!window.visualViewport) return;

    let isKeyboardOpen = false;
    let debounceTimer = null;

    const handleViewportChange = () => {
      const vv = window.visualViewport;
      const keyboardHeight = window.innerHeight - vv.height;

      if (keyboardHeight > 100) {
        // Keyboard is open — constrain the entire layout to the visual viewport
        this.element.style.height = `${vv.height}px`;
        // Prevent iOS from scrolling the page behind the keyboard
        this.element.style.overflow = "hidden";
        window.scrollTo(0, 0);
        isKeyboardOpen = true;

        requestAnimationFrame(() => {
          this.#fitAddon?.fit();
          this.#terminal?.scrollToBottom();
          this.#sendResize();
        });
      } else if (isKeyboardOpen) {
        // Keyboard closed — restore natural layout
        this.element.style.height = "";
        this.element.style.overflow = "";
        isKeyboardOpen = false;

        requestAnimationFrame(() => {
          this.#fitAddon?.fit();
          this.#sendResize();
        });
      }
    };

    this.#keyboardHandler = () => {
      if (debounceTimer) clearTimeout(debounceTimer);
      debounceTimer = setTimeout(handleViewportChange, 50);
    };

    window.visualViewport.addEventListener("resize", this.#keyboardHandler);
    window.visualViewport.addEventListener("scroll", this.#keyboardHandler);
  }

  // Disable the canvas-rendered scrollbar by patching the renderer.
  // showScrollbar() lives on the internal CanvasRenderer, not the Terminal.
  #disableScrollbar() {
    const findRenderer = (obj, depth = 0) => {
      if (depth > 3 || !obj) return null;
      if (typeof obj.showScrollbar === "function" && typeof obj.hideScrollbar === "function") {
        return obj;
      }
      for (const key of Object.keys(obj)) {
        const val = obj[key];
        if (val && typeof val === "object" && !(val instanceof HTMLElement)) {
          const found = findRenderer(val, depth + 1);
          if (found) return found;
        }
      }
      return null;
    };

    const renderer = findRenderer(this.#terminal);
    if (renderer) {
      renderer.scrollbarVisible = false;
      renderer.scrollbarOpacity = 0;
      renderer.showScrollbar = () => {};
      renderer.hideScrollbar = () => {};
    }
  }

  async #initConnections() {
    if (!this.hubIdValue) return;

    this.#hubConn = await ConnectionManager.acquire(
      HubConnection,
      this.hubIdValue,
      { hubId: this.hubIdValue },
    );

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
        rows: this.#terminal?.rows || 24,
        cols: this.#terminal?.cols || 80,
      },
    );

    this.#unsubscribers.push(
      this.#terminalConn.onOutput((data) => {
        this.#handleMessage({ type: "output", data });
      }),
    );

    this.#unsubscribers.push(
      this.#terminalConn.onConnected(() => {
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
    if (this.#textarea) this.#textarea.setAttribute("tabindex", "0");
    this.#terminal?.focus();
  }

  getDimensions() {
    return this.#terminal
      ? { cols: this.#terminal.cols, rows: this.#terminal.rows }
      : { cols: 80, rows: 24 };
  }

  // Connection handlers
  #handleConnected() {
    if (!this.#terminal) return;
    this.#sendResize();
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
        this.#terminal?.clear();
        requestAnimationFrame(() => {
          this.#fitAddon?.fit();
          this.#sendResize();
        });
        break;
    }
  }

  #handleError(error) {
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
    if (!this.#terminal) return;

    const cols = this.#terminal.cols;
    const rows = this.#terminal.rows;

    this.#terminalConn?.sendResize(cols, rows);
    this.#hubConn?.sendResize(cols, rows);
  }

  #handleResize() {
    this.#fitAddon?.fit();

    if (this.#resizeDebounceTimer) {
      clearTimeout(this.#resizeDebounceTimer);
    }
    this.#resizeDebounceTimer = setTimeout(() => {
      this.#resizeDebounceTimer = null;
      this.#sendResize();
    }, 150);
  }
}
