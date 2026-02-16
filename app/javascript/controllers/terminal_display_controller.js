import { Controller } from "@hotwired/stimulus";
import { Restty } from "restty";
import { ConnectionManager, HubConnection } from "connections";
import { WebRtcPtyTransport } from "transport/webrtc_pty_transport";

/**
 * Terminal Display Controller
 *
 * Renders a terminal using Restty (libghostty-vt WASM + WebGPU/WebGL2).
 * WebRtcPtyTransport bridges our E2E-encrypted WebRTC DataChannel into
 * Restty's native transport layer for SSH-like terminal integration.
 *
 * Init sequence:
 *   1. Acquire HubConnection (establishes WebRTC peer)
 *   2. Create Restty with transport (loads WASM, renders canvas)
 *   3. onBackend fires (WASM ready) → connectPty()
 *   4. connectPty() → transport.connect() → acquires TerminalConnection → data flows
 *
 * Mobile: Tap vs swipe detection prevents virtual keyboard from appearing
 * during scroll. Only deliberate taps (< 10px movement) trigger keyboard.
 *
 * Restty handles: GPU rendering, auto-resize, touch selection, font shaping.
 */
export default class extends Controller {
  static targets = ["container"];

  static values = {
    hubId: String,
    agentIndex: Number,
    ptyIndex: { type: Number, default: 0 },
  };

  #restty = null;
  #transport = null;
  #hubConn = null;
  #imeInput = null;
  #touchStart = null;
  #viewportHandler = null;
  #momentumRafId = null;
  #resizeFixTimer = null;
  #disconnected = false;
  #cursorRow = 0;
  #cellH = 0;

  connect() {
    this.#disconnected = false;
    this.#initTerminal();
    this.#bindViewport();
  }

  disconnect() {
    this.#disconnected = true;
    this.#unbindViewport();

    if (this.#momentumRafId) {
      cancelAnimationFrame(this.#momentumRafId);
      this.#momentumRafId = null;
    }
    clearTimeout(this.#resizeFixTimer);

    this.#hubConn?.release();
    this.#hubConn = null;

    this.#restty?.destroy();
    this.#restty = null;

    this.#transport?.destroy();
    this.#transport = null;
  }

  /**
   * Track the virtual keyboard on iOS Safari.
   * Sets --kb-shift CSS variable and data-mobile-keyboard attribute on body.
   * --kb-shift is the pixel amount needed to keep the cursor row and mobile
   * buttons visible above the keyboard. Consumers use the mobile-keyboard:
   * Tailwind variant and --kb-shift variable for styling.
   */
  #bindViewport() {
    if (!window.visualViewport) return;
    this.#viewportHandler = () => {
      const vv = window.visualViewport;
      const keyboardHeight = window.innerHeight - vv.height;
      if (keyboardHeight <= 0) {
        document.body.style.removeProperty("--kb-shift");
        delete document.body.dataset.mobileKeyboard;
        return;
      }
      // Find the cursor's absolute Y position on screen.
      // The container target holds the terminal canvas; its top is where
      // row 0 starts. Cursor is at (cursorRow + 1) * cellH below that
      // (the +1 accounts for the row itself needing to be fully visible).
      const container = this.hasContainerTarget ? this.containerTarget : this.element;
      const termTop = container.getBoundingClientRect().top;
      const dpr = window.devicePixelRatio || 1;
      const cellH = this.#cellH / dpr;
      const cursorBottom = termTop + (this.#cursorRow + 1) * cellH;
      // How far below the visible viewport is the cursor?
      const cursorOverflow = cursorBottom - vv.height + vv.offsetTop;
      // Always shift enough to keep the mobile buttons above the keyboard.
      // The buttons are at the bottom of the element, so they need the full
      // keyboard shift to stay visible.
      const buttonsShift = keyboardHeight - vv.offsetTop;
      const shift = Math.max(0, cursorOverflow, buttonsShift);
      // Set CSS variable and data attribute on body for consumers
      document.body.style.setProperty("--kb-shift", `${shift}px`);
      if (shift > 0) {
        document.body.dataset.mobileKeyboard = "";
      } else {
        delete document.body.dataset.mobileKeyboard;
      }
    };
    window.visualViewport.addEventListener("resize", this.#viewportHandler);
    window.visualViewport.addEventListener("scroll", this.#viewportHandler);
  }

  #unbindViewport() {
    if (!this.#viewportHandler || !window.visualViewport) return;
    window.visualViewport.removeEventListener("resize", this.#viewportHandler);
    window.visualViewport.removeEventListener("scroll", this.#viewportHandler);
    document.body.style.removeProperty("--kb-shift");
    delete document.body.dataset.mobileKeyboard;
    this.#viewportHandler = null;
  }

  async #initTerminal() {
    if (!this.hubIdValue) return;

    const isMobile = "ontouchstart" in window;
    const container = this.hasContainerTarget
      ? this.containerTarget
      : this.element;

    // 1. Acquire hub connection (establishes WebRTC peer)
    this.#hubConn = await ConnectionManager.acquire(
      HubConnection,
      this.hubIdValue,
      { hubId: this.hubIdValue },
    );

    // Guard: if disconnected during async acquire, release and bail
    if (this.#disconnected) {
      this.#hubConn.release();
      this.#hubConn = null;
      return;
    }

    // 2. Create transport (stores params, no connection yet)
    this.#transport = new WebRtcPtyTransport({
      hubId: this.hubIdValue,
      agentIndex: this.agentIndexValue,
      ptyIndex: this.ptyIndexValue,
    });

    // 3. Create Restty — loads WASM, renders canvas
    //    onBackend fires after WASM init → connectPty() subscribes terminal channel
    this.#restty = new Restty({
      root: container,
      onPaneCreated: (pane) => {
        container.addEventListener("contextmenu", (e) => e.preventDefault());
        if (isMobile) {
          this.#imeInput = pane.imeInput;
          // Suppress iOS autocorrect/QuickType bar — not useful for terminal input
          Object.assign(pane.imeInput, {
            autocorrect: "off",
            autocomplete: "off",
            autocapitalize: "off",
            spellcheck: false,
          });
          this.#bindTapDetection(pane.canvas);
          this.#bindMobileEnter(pane.imeInput);
        }
      },
      appOptions: {
        ptyTransport: this.#transport,
        autoResize: true,
        fontSize: 14,
        fontPreset: "none",
        fontSources: [
          // Primary font — JetBrains Mono with Nerd Font glyphs (~9.5MB for all 4 variants)
          // Tries local install first, falls back to CDN
          { type: "local", matchers: ["jetbrainsmono nerd font", "jetbrains mono nerd font", "jetbrains mono"], label: "JetBrains Mono (Local)" },
          { type: "url", url: "https://cdn.jsdelivr.net/gh/ryanoasis/nerd-fonts@v3.4.0/patched-fonts/JetBrainsMono/NoLigatures/Regular/JetBrainsMonoNLNerdFontMono-Regular.ttf", label: "JetBrains Mono Nerd Font Regular" },
          { type: "url", url: "https://cdn.jsdelivr.net/gh/ryanoasis/nerd-fonts@v3.4.0/patched-fonts/JetBrainsMono/NoLigatures/Bold/JetBrainsMonoNLNerdFontMono-Bold.ttf", label: "JetBrains Mono Nerd Font Bold" },
          { type: "url", url: "https://cdn.jsdelivr.net/gh/ryanoasis/nerd-fonts@v3.4.0/patched-fonts/JetBrainsMono/NoLigatures/Italic/JetBrainsMonoNLNerdFontMono-Italic.ttf", label: "JetBrains Mono Nerd Font Italic" },
          { type: "url", url: "https://cdn.jsdelivr.net/gh/ryanoasis/nerd-fonts@v3.4.0/patched-fonts/JetBrainsMono/NoLigatures/BoldItalic/JetBrainsMonoNLNerdFontMono-BoldItalic.ttf", label: "JetBrains Mono Nerd Font Bold Italic" },
          // Nerd Font symbols — powerline, devicons, etc. (~2.5MB)
          { type: "local", matchers: ["symbols nerd font mono", "symbols nerd font"], label: "Symbols Nerd Font (Local)" },
          { type: "url", url: "https://cdn.jsdelivr.net/gh/ryanoasis/nerd-fonts@v3.4.0/patched-fonts/NerdFontsSymbolsOnly/SymbolsNerdFontMono-Regular.ttf" },
          // Symbol fallbacks (~3MB total) — misc Unicode symbols, arrows, box drawing, etc.
          { type: "local", matchers: ["apple symbols", "applesymbols"], label: "Apple Symbols (Local)" },
          { type: "url", url: "https://cdn.jsdelivr.net/gh/notofonts/noto-fonts@main/unhinted/ttf/NotoSansSymbols2/NotoSansSymbols2-Regular.ttf" },
          { type: "url", url: "https://cdn.jsdelivr.net/gh/ChiefMikeK/ttf-symbola@master/Symbola.ttf" },
        ],
        maxScrollback: 10_000_000, // 10MB — ~7,000 lines at 175 cols
        touchSelectionMode: "long-press",
        callbacks: {
          onLog: (line) => {
            if (line.includes("StringAllocOutOfMemory")) {
              console.warn("[terminal] WASM OOM detected (should not happen with patched ghostty-vt)");
            }
          },
          onBackend: () => {
            this.#restty?.connectPty();
            if (!isMobile) this.#restty?.focus();
          },
          onTermSize: (cols, rows) => {
            if (this.#hubConn?.handshakeComplete) {
              this.#hubConn.sendResize(cols, rows);
            } else if (this.#hubConn) {
              // Queue initial resize — fires before DataChannel is open.
              const handler = () => {
                this.#hubConn?.off("connected", handler);
                this.#hubConn?.sendResize(cols, rows);
              };
              this.#hubConn.on("connected", handler);
            }
            // Force re-layout after resize settles to fix garbled rendering.
            // Restty's internal grid can desync from the canvas after resize.
            clearTimeout(this.#resizeFixTimer);
            this.#resizeFixTimer = setTimeout(() => {
              this.#restty?.updateSize(true);
            }, 100);
          },
          onCursor: (_col, row) => { this.#cursorRow = row; },
          onCellSize: (_cellW, cellH) => { this.#cellH = cellH; },
        },
      },
    });
  }

  /**
   * Detect tap vs swipe on the canvas. Handles three concerns:
   *
   * 1. Keyboard gating: Restty calls imeInput.focus() on pointerdown, opening
   *    the keyboard on every touch. We wrap focus() so it only fires after we
   *    confirm a tap (< 10px movement) on pointerup.
   *
   * 2. Momentum scrolling: Track touch velocity during the gesture. On release,
   *    dispatch synthetic WheelEvents in a rAF loop with deceleration. Restty's
   *    wheel handler converts pixel deltaY to scroll lines.
   */
  #bindTapDetection(canvas) {
    const TAP_THRESHOLD = 10;
    const ime = this.#imeInput;
    if (!ime) return;

    // Gate imeInput.focus() — Restty calls focus on pointerdown which opens
    // the keyboard on every touch. We only want keyboard on deliberate taps.
    const nativeFocus = ime.focus.bind(ime);
    let focusGated = false;

    ime.focus = (opts) => {
      if (!focusGated) nativeFocus(opts);
    };

    // Restty handles live drag scrolling natively in long-press mode — its
    // onPointerMove calls scrollViewportByLines() on pan gestures.
    // We add: focus gating, velocity tracking, and momentum on release.

    let lastY = 0;
    let lastTime = 0;
    let velocity = 0;

    canvas.addEventListener("pointerdown", (e) => {
      if (e.pointerType !== "touch") return;
      this.#touchStart = { x: e.clientX, y: e.clientY };
      focusGated = true;
      lastY = e.clientY;
      lastTime = e.timeStamp;
      velocity = 0;
      if (this.#momentumRafId) {
        cancelAnimationFrame(this.#momentumRafId);
        this.#momentumRafId = null;
      }
    }, true);

    // Track velocity during drag for momentum on release.
    // Restty's onPointerMove handles the actual live scrolling.
    canvas.addEventListener("pointermove", (e) => {
      if (e.pointerType !== "touch" || !this.#touchStart) return;
      const dt = e.timeStamp - lastTime;
      if (dt > 0) {
        const instantVelocity = (lastY - e.clientY) / dt;
        velocity = velocity * 0.4 + instantVelocity * 0.6;
      }
      lastY = e.clientY;
      lastTime = e.timeStamp;
    });

    canvas.addEventListener("pointerup", (e) => {
      if (e.pointerType !== "touch" || !this.#touchStart) {
        focusGated = false;
        return;
      }

      const dx = Math.abs(e.clientX - this.#touchStart.x);
      const dy = Math.abs(e.clientY - this.#touchStart.y);
      this.#touchStart = null;
      focusGated = false;

      if (dx < TAP_THRESHOLD && dy < TAP_THRESHOLD) {
        nativeFocus({ preventScroll: true });
      } else {
        // Scroll gesture — start momentum if fast enough
        if (Math.abs(velocity) > 0.5) {
          this.#startMomentum(canvas, velocity);
        }
        if (document.body.hasAttribute("data-mobile-keyboard")) {
          // iOS dismisses keyboard during scroll — re-focus to bring it back
          nativeFocus({ preventScroll: true });
        }
      }
      velocity = 0;
    });

    canvas.addEventListener("pointercancel", () => {
      this.#touchStart = null;
      focusGated = false;
      velocity = 0;
    });
  }

  /**
   * Momentum scroll after touch release. Dispatches synthetic WheelEvents
   * with shiftKey=true to bypass Restty's mouse mode routing (apps like
   * Claude TUI capture mouse events, but shift+wheel falls through to
   * viewport scrolling). The 0.5x speed from shiftKey is compensated
   * by the 6x deltaY multiplier (net 3x vs native wheel).
   */
  #startMomentum(canvas, initialVelocity) {
    const FRICTION = 0.96;
    const MIN_VELOCITY = 0.2;
    let vel = initialVelocity * 32; // Convert px/ms → px/frame

    const tick = () => {
      vel *= FRICTION;
      if (Math.abs(vel) < MIN_VELOCITY) {
        this.#momentumRafId = null;
        return;
      }

      canvas.dispatchEvent(new WheelEvent("wheel", {
        deltaY: vel * 6, // Compensate for shiftKey's 0.5x speed
        deltaMode: 0,
        shiftKey: true,  // Bypass mouse mode → viewport scroll
        bubbles: true,
        cancelable: true,
      }));

      this.#momentumRafId = requestAnimationFrame(tick);
    };

    this.#momentumRafId = requestAnimationFrame(tick);
  }

  /**
   * Mobile Enter → Shift+Enter. On mobile there's no Shift key, so Enter
   * always submits (e.g. in Claude). Intercept at both keydown AND
   * beforeinput levels — mobile IME processes Enter through the Input
   * Events API, not keydown alone.
   *
   * Sends \n (0x0A / LF) which the CLI maps to "shift+enter" (see
   * raw_input.rs:236). This works regardless of kitty keyboard protocol
   * state. The touch button `sendEnter` still sends bare \r for submitting.
   */
  #bindMobileEnter(imeInput) {
    let enterHandled = false;

    imeInput.addEventListener("keydown", (e) => {
      if (e.key !== "Enter" || e.shiftKey) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      enterHandled = true;
      this.#restty?.sendKeyInput("\n");
    }, true);

    imeInput.addEventListener("beforeinput", (e) => {
      if (e.inputType === "insertLineBreak" || e.inputType === "insertParagraph") {
        e.preventDefault();
        if (!enterHandled) {
          this.#restty?.sendKeyInput("\n");
        }
        enterHandled = false;
      }
    }, true);
  }

  // Public actions for touch control buttons
  // sendKeyInput routes directly to ptyTransport.sendInput() (same path as keyboard events).
  // sendInput() writes to WASM as "program" input which doesn't produce PTY output for control chars.
  sendCtrlC() { this.#restty?.sendKeyInput("\x03"); }
  sendEnter() { this.#restty?.sendKeyInput("\r"); }
  sendEscape() { this.#restty?.sendKeyInput("\x1b"); }
  sendTab() { this.#restty?.sendKeyInput("\t"); }
  sendArrowUp() { this.#restty?.sendKeyInput("\x1b[A"); }
  sendArrowDown() { this.#restty?.sendKeyInput("\x1b[B"); }
  sendArrowLeft() { this.#restty?.sendKeyInput("\x1b[D"); }
  sendArrowRight() { this.#restty?.sendKeyInput("\x1b[C"); }

  // Public API
  clear() { this.#restty?.clearScreen(); }
  focus() { this.#restty?.focus(); }
}
