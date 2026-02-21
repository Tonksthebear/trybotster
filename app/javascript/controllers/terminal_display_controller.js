import { Controller } from "@hotwired/stimulus";
import { Restty } from "restty";
import { ConnectionManager, HubConnection } from "connections";
import { WebRtcPtyTransport } from "transport/webrtc_pty_transport";
import { usePresence } from "lib/use_presence";

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
  static targets = ["container", "dropZone", "toast"];

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
  #overlay = null;
  #focused = false;
  #present = true;
  #connected = false;
  #teardownPresence = null;

  connect() {
    this.#disconnected = false;
    this.#initTerminal();
    this.#bindViewport();
    this.#teardownPresence = usePresence(this, { ms: 120000 });
  }

  disconnect() {
    this.#sendFocusOut();
    this.#teardownPresence?.();
    this.#teardownPresence = null;
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

    this.#overlay?.remove();
    this.#overlay = null;
  }

  /**
   * Track the virtual keyboard on iOS Safari.
   * Sets --kb-height CSS variable to the visible viewport height and
   * data-mobile-keyboard attribute on body. Consumers use the
   * mobile-keyboard: Tailwind variant to resize the terminal container.
   */
  #bindViewport() {
    if (!window.visualViewport) return;
    this.#viewportHandler = () => {
      const vv = window.visualViewport;
      const keyboardHeight = window.innerHeight - vv.height;
      if (keyboardHeight <= 0) {
        document.body.style.removeProperty("--kb-height");
        delete document.body.dataset.mobileKeyboard;
        return;
      }
      document.body.style.setProperty("--kb-height", `${vv.height}px`);
      document.body.dataset.mobileKeyboard = "";
    };
    window.visualViewport.addEventListener("resize", this.#viewportHandler);
    window.visualViewport.addEventListener("scroll", this.#viewportHandler);
  }

  #unbindViewport() {
    if (!this.#viewportHandler || !window.visualViewport) return;
    window.visualViewport.removeEventListener("resize", this.#viewportHandler);
    window.visualViewport.removeEventListener("scroll", this.#viewportHandler);
    document.body.style.removeProperty("--kb-height");
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
    this.#transport.onReconnect = () => this.#restty?.clearScreen();
    this.#transport.onConnect = () => {
      // Force recalculate grid dimensions. Restty's init() calls updateSize()
      // before the canvas is in the DOM (getBoundingClientRect returns 0x0).
      // The ResizeObserver usually corrects this, but can race with WASM init.
      // Forcing it here guarantees correct dimensions once everything is ready.
      this.#restty?.updateSize(true);
      // Restore VT-driven mouse mode after disconnect override
      this.#restty?.setMouseMode("auto");
      this.#hideOverlay();
      // Viewing the terminal clears any pending notification badge
      this.#hubConn?.clearNotification(this.agentIndexValue);
      this.#connected = true;
      this.#updateFocus();
    };
    this.#transport.onDisconnect = () => {
      this.#restty?.setMouseMode("off");
      this.#showOverlay();
      this.#connected = false;
      this.#updateFocus();
    };

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
        maxScrollbackBytes: 10_000_000, // 10MB — ~7,000 lines at 175 cols
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
          // Restty handles resize internally — calls ptyTransport.resize()
          // directly on grid change and on connect (restty-chunk.js:61035).
          // No onTermSize handler needed.
        },
      },
    });

    // 4. Intercept file paste/drop — capture phase beats Restty's imeInput handler
    this.#bindFileDrop(container);
  }

  static MAX_FILE_SIZE = 50 * 1024 * 1024; // 50MB
  #toastTimeout = null;

  /**
   * Intercept paste and drop events for file transfers.
   * Sends file bytes over WebRTC to the CLI which writes a temp file
   * and injects the path into the PTY. Accepts any file type.
   */
  #bindFileDrop(container) {
    // Paste: intercept files before Restty's imeInput handler.
    // Text-only paste (no files) falls through to Restty normally.
    container.addEventListener("paste", async (e) => {
      const items = [...(e.clipboardData?.items || [])];
      const fileItem = items.find(i => i.kind === "file");
      if (!fileItem) return;

      e.preventDefault();
      e.stopPropagation();

      const blob = fileItem.getAsFile();
      if (!blob) return;

      await this.#sendFile(blob);
    }, { capture: true });

    // Dragover: must preventDefault for browser to allow drop.
    // Capture phase to intercept before Restty's canvas.
    container.addEventListener("dragover", (e) => {
      if (!e.dataTransfer?.types?.includes("Files")) return;
      e.preventDefault();
      if (this.hasDropZoneTarget) this.dropZoneTarget.toggleAttribute("data-visible", true);
    }, { capture: true });

    container.addEventListener("dragleave", (e) => {
      if (!container.contains(e.relatedTarget)) {
        if (this.hasDropZoneTarget) this.dropZoneTarget.removeAttribute("data-visible");
      }
    }, { capture: true });

    // Drop: extract files (capture phase to beat canvas)
    container.addEventListener("drop", async (e) => {
      if (this.hasDropZoneTarget) this.dropZoneTarget.removeAttribute("data-visible");
      const files = [...(e.dataTransfer?.files || [])];
      if (!files.length) return;

      e.preventDefault();
      e.stopPropagation();

      for (const file of files) {
        await this.#sendFile(file);
      }
    }, { capture: true });
  }

  async #sendFile(blob) {
    if (blob.size > this.constructor.MAX_FILE_SIZE) {
      const sizeMB = (blob.size / 1024 / 1024).toFixed(1);
      this.#showToast(`File too large (${sizeMB}MB, max 50MB)`, "error");
      return;
    }

    const buffer = await blob.arrayBuffer();
    const data = new Uint8Array(buffer);
    const filename = blob.name || `paste.${blob.type.split("/")[1] || "bin"}`;
    const ok = this.#transport?.sendFile(data, filename);

    if (ok !== false) {
      this.#showToast(`Sent ${filename}`, "success");
    }
  }

  #showToast(message, type = "success") {
    if (!this.hasToastTarget) return;
    const el = this.toastTarget;

    clearTimeout(this.#toastTimeout);
    el.textContent = message;
    el.dataset.toastType = type;
    el.toggleAttribute("data-visible", true);

    this.#toastTimeout = setTimeout(() => {
      el.removeAttribute("data-visible");
    }, 2000);
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
    //
    // Velocity tracking uses a sliding window of recent samples instead of
    // a simple EMA. This filters out jitter from finger-lift deceleration
    // and gives a more accurate "intent velocity" at release time.

    const VELOCITY_WINDOW_MS = 100; // Only consider samples from last 100ms
    const VELOCITY_MAX_SAMPLES = 8;
    let velocitySamples = []; // { y, t } ring buffer

    canvas.addEventListener("pointerdown", (e) => {
      if (e.pointerType !== "touch") return;
      this.#touchStart = { x: e.clientX, y: e.clientY };
      focusGated = true;
      velocitySamples = [{ y: e.clientY, t: e.timeStamp }];
      if (this.#momentumRafId) {
        cancelAnimationFrame(this.#momentumRafId);
        this.#momentumRafId = null;
      }
    }, true);

    // Track velocity during drag for momentum on release.
    // Restty's onPointerMove handles the actual live scrolling.
    canvas.addEventListener("pointermove", (e) => {
      if (e.pointerType !== "touch" || !this.#touchStart) return;
      velocitySamples.push({ y: e.clientY, t: e.timeStamp });
      if (velocitySamples.length > VELOCITY_MAX_SAMPLES) velocitySamples.shift();
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
        // Compute velocity from recent samples within the time window.
        // Discard stale samples (finger paused before lifting).
        const now = e.timeStamp;
        const cutoff = now - VELOCITY_WINDOW_MS;
        const recent = velocitySamples.filter((s) => s.t >= cutoff);
        let velocity = 0;
        if (recent.length >= 2) {
          const first = recent[0];
          const last = recent[recent.length - 1];
          const dt = last.t - first.t;
          if (dt > 0) velocity = (first.y - last.y) / dt; // px/ms, positive = scroll down
        }

        if (Math.abs(velocity) > 0.3) {
          this.#startMomentum(canvas, velocity);
        }
        if (document.body.hasAttribute("data-mobile-keyboard")) {
          nativeFocus({ preventScroll: true });
        }
      }
      velocitySamples = [];
    });

    canvas.addEventListener("pointercancel", () => {
      this.#touchStart = null;
      focusGated = false;
      velocitySamples = [];
    });
  }

  /**
   * Time-based momentum scroll after touch release. Uses iOS-style
   * exponential deceleration that's frame-rate independent — behaves
   * identically on 60Hz phones and 120Hz ProMotion iPads.
   *
   * Dispatches synthetic WheelEvents with shiftKey=true to bypass
   * Restty's mouse mode routing (apps like Claude TUI capture mouse
   * events, but shift+wheel falls through to viewport scrolling).
   *
   * Physics: v(t) = v0 * e^(-k*t) where k controls deceleration rate.
   * The deceleration constant (5.0) was tuned to match iOS Safari's
   * scroll feel — fast flicks coast far, gentle swipes stop quickly.
   */
  #startMomentum(canvas, initialVelocity) {
    const DECEL_RATE = 1.8;        // Exponential decay constant (higher = stops faster)
    const MAX_VELOCITY = 20.0;     // Cap px/ms to prevent absurd scroll speeds
    const MIN_VELOCITY_PX = 0.15;  // Stop threshold in px/ms

    // Clamp initial velocity to prevent extreme scroll from fast flicks
    const v0 = Math.sign(initialVelocity) * Math.min(Math.abs(initialVelocity), MAX_VELOCITY);
    const startTime = performance.now();

    const tick = (now) => {
      const elapsed = (now - startTime) / 1000; // seconds
      // Exponential decay: velocity diminishes smoothly over time
      const currentVelocity = v0 * Math.exp(-DECEL_RATE * elapsed);

      if (Math.abs(currentVelocity) < MIN_VELOCITY_PX) {
        this.#momentumRafId = null;
        return;
      }

      // Convert px/ms velocity to pixel delta for this frame.
      // currentVelocity is px/ms, multiply by ~16ms for per-frame delta,
      // then 6x to compensate for shiftKey's 0.5x speed modifier.
      const deltaY = currentVelocity * 16 * 6;

      canvas.dispatchEvent(new WheelEvent("wheel", {
        deltaY,
        deltaMode: 0,
        shiftKey: true,  // Bypass mouse mode → viewport scroll
        bubbles: true,
        cancelable: true,
      }));

      this.#momentumRafId = requestAnimationFrame(tick);
    };

    this.#momentumRafId = requestAnimationFrame(tick);
  }

  #showOverlay() {
    if (!this.#overlay) {
      const container = this.hasContainerTarget ? this.containerTarget : this.element;
      container.style.position = "relative";
      this.#overlay = document.createElement("div");
      this.#overlay.className = "absolute inset-0 flex items-center justify-center bg-black/70 z-10 pointer-events-none";
      this.#overlay.innerHTML = `
        <div class="flex items-center gap-2.5 text-zinc-400 text-sm font-medium">
          <svg class="animate-spin size-4" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
            <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
            <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"></path>
          </svg>
          Reconnecting…
        </div>
      `;
      container.appendChild(this.#overlay);
    }
    this.#overlay.hidden = false;
  }

  #hideOverlay() {
    if (this.#overlay) this.#overlay.hidden = true;
  }

  // usePresence callbacks
  away() {
    this.#present = false;
    this.#updateFocus();
  }

  back() {
    this.#present = true;
    this.#updateFocus();
  }

  #updateFocus() {
    const shouldFocus = this.#connected && this.#present;
    if (shouldFocus === this.#focused) return;
    this.#focused = shouldFocus;
    this.#transport?.sendInput(shouldFocus ? "\x1b[I" : "\x1b[O");
  }

  #sendFocusOut() {
    if (this.#focused) {
      this.#focused = false;
      this.#transport?.sendInput("\x1b[O");
    }
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
