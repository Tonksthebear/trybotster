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
  #disconnected = false;

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

    this.#hubConn?.release();
    this.#hubConn = null;

    this.#restty?.destroy();
    this.#restty = null;

    this.#transport?.destroy();
    this.#transport = null;
  }

  /**
   * Shift the terminal view when the virtual keyboard opens.
   * On iOS Safari, dvh doesn't shrink when the keyboard appears — it overlays
   * the page. Instead of resizing (which triggers Restty to re-layout the grid),
   * we translate the container up so the bottom (buttons + input area) stays
   * above the keyboard. The top of the terminal clips off-screen, which is fine
   * since the user is interacting at the bottom.
   */
  #bindViewport() {
    if (!window.visualViewport) return;
    this.#viewportHandler = () => {
      const vv = window.visualViewport;
      // How much the keyboard covers: full layout height minus visible viewport
      const keyboardHeight = window.innerHeight - vv.height;
      // Shift up by keyboard height + account for iOS scroll offset
      const offset = keyboardHeight > 0 ? -(keyboardHeight - vv.offsetTop) : 0;
      this.element.style.transform = offset ? `translateY(${offset}px)` : "";
    };
    window.visualViewport.addEventListener("resize", this.#viewportHandler);
    window.visualViewport.addEventListener("scroll", this.#viewportHandler);
  }

  #unbindViewport() {
    if (!this.#viewportHandler || !window.visualViewport) return;
    window.visualViewport.removeEventListener("resize", this.#viewportHandler);
    window.visualViewport.removeEventListener("scroll", this.#viewportHandler);
    this.element.style.transform = "";
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
        if (isMobile) {
          this.#imeInput = pane.imeInput;
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
        touchSelectionMode: "long-press",
        callbacks: {
          onBackend: () => {
            this.#restty?.connectPty();
            if (!isMobile) this.#restty?.focus();
          },
          onTermSize: (cols, rows) => {
            this.#hubConn?.sendResize(cols, rows);
          },
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
    const FRICTION = 0.92;
    const MIN_VELOCITY = 0.5;
    const ime = this.#imeInput;
    if (!ime) return;

    // Gate imeInput.focus() — Restty's calls become no-ops during touch gestures
    const nativeFocus = ime.focus.bind(ime);
    let focusGated = false;

    ime.focus = (opts) => {
      if (!focusGated) nativeFocus(opts);
    };

    // Velocity tracking for momentum
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
      // Cancel any in-progress momentum
      if (this.#momentumRafId) {
        cancelAnimationFrame(this.#momentumRafId);
        this.#momentumRafId = null;
      }
    }, true);

    canvas.addEventListener("pointermove", (e) => {
      if (e.pointerType !== "touch" || !this.#touchStart) return;
      const dt = e.timeStamp - lastTime;
      if (dt > 0) {
        // Weighted average: blend new velocity with previous for smoothing
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
        // Deliberate tap — show keyboard
        nativeFocus({ preventScroll: true });
      } else if (Math.abs(velocity) > MIN_VELOCITY) {
        // Scroll gesture with momentum — start inertia animation
        this.#startMomentum(canvas, velocity);
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

  /**
   * Momentum scroll: dispatches synthetic WheelEvents with decaying velocity.
   * Restty's onWheel handler picks these up and calls scrollViewportByLines().
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
        deltaY: vel,
        deltaMode: 0,  // DOM_DELTA_PIXEL
        bubbles: true,
        cancelable: true,
      }));

      this.#momentumRafId = requestAnimationFrame(tick);
    };

    this.#momentumRafId = requestAnimationFrame(tick);
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
