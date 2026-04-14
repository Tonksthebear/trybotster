import React, { useRef, useEffect, useCallback } from 'react'
import { connect, disconnect } from '../../lib/hub-bridge'
import ConnectionStatus from '../hub/ConnectionStatus'

const CONNECT_DEBOUNCE_MS = 30
const MAX_FILE_SIZE = 50 * 1024 * 1024

function resolveTerminal() {
  if (window.__botsterTerminal) return window.__botsterTerminal
  throw new Error(
    '[TerminalView] Terminal infrastructure not available. Ensure application.js has loaded.'
  )
}

export default function TerminalView({ hubId, sessionUuid }) {
  const containerRef = useRef(null)
  const stateRef = useRef(null)
  const dropZoneRef = useRef(null)
  const toastRef = useRef(null)

  // Mobile control handlers — stable refs so the DOM buttons don't re-render
  const sendKey = useCallback((key) => {
    stateRef.current?.restty?.sendKeyInput(key)
  }, [])

  useEffect(() => {
    if (!hubId || !sessionUuid) return

    const { Restty, HubConnectionManager, HubTransport, WebRtcPtyTransport } =
      resolveTerminal()

    const isMobile = 'ontouchstart' in window
    const container = containerRef.current
    if (!container) return

    // Mutable state for this terminal instance
    const state = {
      restty: null,
      transport: null,
      hubTransport: null,
      overlay: null,
      imeInput: null,
      touchStart: null,
      momentumRafId: null,
      toastTimeout: null,
      backendReady: false,
      connectPtyRequested: false,
      connectPtyTimer: null,
      pendingSize: null,
      focused: false,
      connected: false,
      present: true,
      destroyed: false,
      listeners: [],
      restoreImeFocus: null,
    }
    stateRef.current = state

    function listen(target, event, handler, options) {
      target.addEventListener(event, handler, options)
      state.listeners.push(() =>
        target.removeEventListener(event, handler, options)
      )
    }

    function showOverlay() {
      if (!state.overlay) {
        container.style.position = 'relative'
        state.overlay = document.createElement('div')
        state.overlay.className =
          'absolute inset-0 flex items-center justify-center bg-black/70 z-10 pointer-events-none'
        state.overlay.innerHTML = `
          <div class="flex items-center gap-2.5 text-zinc-400 text-sm font-medium">
            <svg class="animate-spin size-4" xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24">
              <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4"></circle>
              <path class="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"></path>
            </svg>
            Connecting\u2026
          </div>
        `
        container.appendChild(state.overlay)
      }
      state.overlay.hidden = false
    }

    function hideOverlay() {
      if (state.overlay) state.overlay.hidden = true
    }

    function showToast(message, type = 'success') {
      const el = toastRef.current
      if (!el) return
      clearTimeout(state.toastTimeout)
      el.textContent = message
      el.dataset.toastType = type
      el.toggleAttribute('data-visible', true)
      state.toastTimeout = setTimeout(() => {
        el.removeAttribute('data-visible')
      }, 2000)
    }

    function sendFocusState() {
      state.transport?.sendInput(state.focused ? '\x1b[I' : '\x1b[O')
      if (state.focused) sendColorProfile()
    }

    function sendColorProfile() {
      if (!state.restty) return
      const colors = {}
      const fg = state.restty.getColorForeground?.()
      const bg = state.restty.getColorBackground?.()
      const cursor = state.restty.getColorCursor?.()
      if (fg != null)
        colors[256] = {
          r: (fg >> 16) & 0xff,
          g: (fg >> 8) & 0xff,
          b: fg & 0xff,
        }
      if (bg != null)
        colors[257] = {
          r: (bg >> 16) & 0xff,
          g: (bg >> 8) & 0xff,
          b: bg & 0xff,
        }
      if (cursor != null)
        colors[258] = {
          r: (cursor >> 16) & 0xff,
          g: (cursor >> 8) & 0xff,
          b: cursor & 0xff,
        }
      const palette = state.restty.getPalette?.()
      if (palette) {
        for (let i = 0; i < 256; i++) {
          colors[i] = {
            r: palette[i * 3],
            g: palette[i * 3 + 1],
            b: palette[i * 3 + 2],
          }
        }
      }
      if (Object.keys(colors).length > 0) {
        state.transport?.sendColorProfile(colors)
      }
    }

    function updateFocus() {
      const shouldFocus = state.connected && state.present
      if (shouldFocus === state.focused) return
      state.focused = shouldFocus
      sendFocusState()
    }

    function onTermSize(cols, rows) {
      if (state.connectPtyRequested || state.destroyed || !state.backendReady)
        return
      state.pendingSize = { cols, rows }
      if (state.connectPtyTimer) clearTimeout(state.connectPtyTimer)
      state.connectPtyTimer = setTimeout(() => {
        state.connectPtyTimer = null
        if (state.connectPtyRequested || state.destroyed || !state.backendReady)
          return
        const size = state.pendingSize
        if (!size || size.cols <= 1 || size.rows <= 1) return
        state.connectPtyRequested = true
        state.restty?.connectPty()
      }, CONNECT_DEBOUNCE_MS)
    }

    async function sendFile(blob) {
      if (blob.size > MAX_FILE_SIZE) {
        const sizeMB = (blob.size / 1024 / 1024).toFixed(1)
        showToast(`File too large (${sizeMB}MB, max 50MB)`, 'error')
        return
      }
      const buffer = await blob.arrayBuffer()
      const data = new Uint8Array(buffer)
      const filename =
        blob.name || `paste.${blob.type.split('/')[1] || 'bin'}`
      const ok = state.transport?.sendFile(data, filename)
      if (ok !== false) showToast(`Sent ${filename}`, 'success')
    }

    function bindFileDrop() {
      listen(
        container,
        'paste',
        async (e) => {
          const items = [...(e.clipboardData?.items || [])]
          const fileItem = items.find((i) => i.kind === 'file')
          if (!fileItem) return
          e.preventDefault()
          e.stopPropagation()
          const blob = fileItem.getAsFile()
          if (blob) await sendFile(blob)
        },
        { capture: true }
      )

      listen(
        container,
        'dragover',
        (e) => {
          if (!e.dataTransfer?.types?.includes('Files')) return
          e.preventDefault()
          dropZoneRef.current?.toggleAttribute('data-visible', true)
        },
        { capture: true }
      )

      listen(
        container,
        'dragleave',
        (e) => {
          if (!container.contains(e.relatedTarget)) {
            dropZoneRef.current?.removeAttribute('data-visible')
          }
        },
        { capture: true }
      )

      listen(
        container,
        'drop',
        async (e) => {
          dropZoneRef.current?.removeAttribute('data-visible')
          const files = [...(e.dataTransfer?.files || [])]
          if (!files.length) return
          e.preventDefault()
          e.stopPropagation()
          for (const file of files) await sendFile(file)
        },
        { capture: true }
      )
    }

    function bindTapDetection(canvas) {
      const TAP_THRESHOLD = 10
      const VELOCITY_WINDOW_MS = 100
      const VELOCITY_MAX_SAMPLES = 8
      const ime = state.imeInput
      if (!ime) return

      const nativeFocus = ime.focus.bind(ime)
      let focusGated = false
      let velocitySamples = []

      ime.focus = (opts) => {
        if (!focusGated) nativeFocus(opts)
      }
      state.restoreImeFocus = () => {
        ime.focus = nativeFocus
      }

      listen(
        canvas,
        'pointerdown',
        (e) => {
          if (e.pointerType !== 'touch') return
          state.touchStart = { x: e.clientX, y: e.clientY }
          focusGated = true
          velocitySamples = [{ y: e.clientY, t: e.timeStamp }]
          if (state.momentumRafId) {
            cancelAnimationFrame(state.momentumRafId)
            state.momentumRafId = null
          }
        },
        true
      )

      listen(canvas, 'pointermove', (e) => {
        if (e.pointerType !== 'touch' || !state.touchStart) return
        velocitySamples.push({ y: e.clientY, t: e.timeStamp })
        if (velocitySamples.length > VELOCITY_MAX_SAMPLES)
          velocitySamples.shift()
      })

      listen(canvas, 'pointerup', (e) => {
        if (e.pointerType !== 'touch' || !state.touchStart) {
          focusGated = false
          return
        }
        const dx = Math.abs(e.clientX - state.touchStart.x)
        const dy = Math.abs(e.clientY - state.touchStart.y)
        state.touchStart = null
        focusGated = false

        if (dx < TAP_THRESHOLD && dy < TAP_THRESHOLD) {
          nativeFocus({ preventScroll: true })
        } else {
          const now = e.timeStamp
          const cutoff = now - VELOCITY_WINDOW_MS
          const recent = velocitySamples.filter((s) => s.t >= cutoff)
          let velocity = 0
          if (recent.length >= 2) {
            const first = recent[0]
            const last = recent[recent.length - 1]
            const dt = last.t - first.t
            if (dt > 0) velocity = (first.y - last.y) / dt
          }
          if (Math.abs(velocity) > 0.3) startMomentum(canvas, velocity)
          if (document.body.hasAttribute('data-mobile-keyboard')) {
            nativeFocus({ preventScroll: true })
          }
        }
        velocitySamples = []
      })

      listen(canvas, 'pointercancel', () => {
        state.touchStart = null
        focusGated = false
        velocitySamples = []
      })
    }

    function startMomentum(canvas, initialVelocity) {
      const DECEL_RATE = 1.8
      const MAX_VELOCITY = 20.0
      const MIN_VELOCITY_PX = 0.15
      const v0 =
        Math.sign(initialVelocity) *
        Math.min(Math.abs(initialVelocity), MAX_VELOCITY)
      const startTime = performance.now()

      const tick = (now) => {
        const elapsed = (now - startTime) / 1000
        const currentVelocity = v0 * Math.exp(-DECEL_RATE * elapsed)
        if (Math.abs(currentVelocity) < MIN_VELOCITY_PX) {
          state.momentumRafId = null
          return
        }
        const deltaY = currentVelocity * 16 * 6
        canvas.dispatchEvent(
          new WheelEvent('wheel', {
            deltaY,
            deltaMode: 0,
            shiftKey: true,
            bubbles: true,
            cancelable: true,
          })
        )
        state.momentumRafId = requestAnimationFrame(tick)
      }
      state.momentumRafId = requestAnimationFrame(tick)
    }

    // Viewport tracking for iOS virtual keyboard
    let viewportHandler = null
    if (window.visualViewport) {
      viewportHandler = () => {
        const vv = window.visualViewport
        const keyboardHeight = window.innerHeight - vv.height
        if (keyboardHeight <= 0) {
          document.body.style.removeProperty('--kb-height')
          delete document.body.dataset.mobileKeyboard
          return
        }
        document.body.style.setProperty('--kb-height', `${vv.height}px`)
        document.body.dataset.mobileKeyboard = ''
      }
      window.visualViewport.addEventListener('resize', viewportHandler)
      window.visualViewport.addEventListener('scroll', viewportHandler)
    }

    // --- Presence tracking (visibility + AFK) ---
    const AFK_TIMEOUT_MS = 120000
    let afkTimer = null
    let lastActivity = 0
    const ACTIVITY_EVENTS = [
      'mousemove',
      'mousedown',
      'keydown',
      'touchstart',
      'wheel',
      'resize',
    ]

    function onActivity() {
      const now = Date.now()
      if (now - lastActivity < 1000) return
      lastActivity = now
      if (!state.present) {
        state.present = true
        updateFocus()
      }
      clearTimeout(afkTimer)
      afkTimer = setTimeout(() => {
        state.present = false
        updateFocus()
      }, AFK_TIMEOUT_MS)
    }

    function onVisibilityChange() {
      if (document.hidden) {
        state.present = false
      } else {
        state.present = true
        onActivity()
      }
      updateFocus()
    }

    document.addEventListener('visibilitychange', onVisibilityChange)
    ACTIVITY_EVENTS.forEach((e) =>
      document.addEventListener(e, onActivity, { passive: true })
    )
    onActivity()

    // --- Hub bridge (for ConnectionStatus) ---
    let bridgeConnectionId = null
    connect(hubId, { surface: 'terminal' }).then(({ connectionId }) => {
      if (state.destroyed) {
        disconnect(connectionId)
      } else {
        bridgeConnectionId = connectionId
      }
    })

    // --- Init terminal ---
    async function init() {
      // 1. Acquire hub transport
      state.hubTransport = await HubConnectionManager.acquire(
        HubTransport,
        hubId,
        { hubId }
      )
      if (state.destroyed) {
        state.hubTransport.release()
        state.hubTransport = null
        return
      }

      // 2. Create PTY transport
      state.transport = new WebRtcPtyTransport({ hubId, sessionUuid })
      state.transport.onReconnect = () => {}
      state.transport.onBinarySnapshot = (data) => {
        const loaded = state.restty
          ? state.restty.loadBinarySnapshot(data)
          : false
        if (loaded) sendColorProfile()
      }
      state.transport.onFocusReportingChanged = () => {
        sendFocusState()
      }
      state.transport.onConnect = () => {
        state.restty?.updateSize(true)
        state.restty?.setMouseMode('auto')
        hideOverlay()
        state.hubTransport?.clearNotification(sessionUuid)
        state.connected = true
        updateFocus()
      }
      state.transport.onDisconnect = () => {
        state.restty?.setMouseMode('off')
        showOverlay()
        state.connected = false
        updateFocus()
      }

      // 3. Create Restty
      state.restty = new Restty({
        root: container,
        defaultContextMenu: false,
        onPaneCreated: (pane) => {
          if (isMobile) {
            state.imeInput = pane.imeInput
            Object.assign(pane.imeInput, {
              autocorrect: 'off',
              autocomplete: 'off',
              autocapitalize: 'off',
              spellcheck: false,
            })
            bindTapDetection(pane.canvas)
          }
        },
        appOptions: {
          ptyTransport: state.transport,
          readOnly: true,
          autoResize: true,
          fontSize: 14,
          fontPreset: 'none',
          fontSources: [
            {
              type: 'local',
              matchers: [
                'jetbrainsmono nerd font',
                'jetbrains mono nerd font',
                'jetbrains mono',
              ],
              label: 'JetBrains Mono (Local)',
            },
            {
              type: 'url',
              url: 'https://cdn.jsdelivr.net/gh/ryanoasis/nerd-fonts@v3.4.0/patched-fonts/JetBrainsMono/NoLigatures/Regular/JetBrainsMonoNLNerdFontMono-Regular.ttf',
              label: 'JetBrains Mono Nerd Font Regular',
            },
            {
              type: 'url',
              url: 'https://cdn.jsdelivr.net/gh/ryanoasis/nerd-fonts@v3.4.0/patched-fonts/JetBrainsMono/NoLigatures/Bold/JetBrainsMonoNLNerdFontMono-Bold.ttf',
              label: 'JetBrains Mono Nerd Font Bold',
            },
            {
              type: 'url',
              url: 'https://cdn.jsdelivr.net/gh/ryanoasis/nerd-fonts@v3.4.0/patched-fonts/JetBrainsMono/NoLigatures/Italic/JetBrainsMonoNLNerdFontMono-Italic.ttf',
              label: 'JetBrains Mono Nerd Font Italic',
            },
            {
              type: 'url',
              url: 'https://cdn.jsdelivr.net/gh/ryanoasis/nerd-fonts@v3.4.0/patched-fonts/JetBrainsMono/NoLigatures/BoldItalic/JetBrainsMonoNLNerdFontMono-BoldItalic.ttf',
              label: 'JetBrains Mono Nerd Font Bold Italic',
            },
            {
              type: 'local',
              matchers: ['symbols nerd font mono', 'symbols nerd font'],
              label: 'Symbols Nerd Font (Local)',
            },
            {
              type: 'url',
              url: 'https://cdn.jsdelivr.net/gh/ryanoasis/nerd-fonts@v3.4.0/patched-fonts/NerdFontsSymbolsOnly/SymbolsNerdFontMono-Regular.ttf',
            },
            {
              type: 'local',
              matchers: [
                'apple symbols',
                'applesymbols',
                'apple symbols regular',
              ],
              label: 'Apple Symbols (Local)',
            },
            {
              type: 'url',
              url: 'https://cdn.jsdelivr.net/gh/notofonts/noto-fonts@main/unhinted/ttf/NotoSansSymbols2/NotoSansSymbols2-Regular.ttf',
            },
            {
              type: 'url',
              url: 'https://cdn.jsdelivr.net/gh/ChiefMikeK/ttf-symbola@master/Symbola.ttf',
            },
            {
              type: 'local',
              matchers: [
                'noto sans canadian aboriginal',
                'notosanscanadianaboriginal',
                'euphemia ucas',
                'euphemiaucas',
              ],
              label: 'Noto Sans Canadian Aboriginal / Euphemia UCAS',
            },
            {
              type: 'url',
              url: 'https://cdn.jsdelivr.net/gh/notofonts/noto-fonts@main/unhinted/ttf/NotoSansCanadianAboriginal/NotoSansCanadianAboriginal-Regular.ttf',
              label: 'Noto Sans Canadian Aboriginal',
            },
            {
              type: 'local',
              matchers: ['apple color emoji', 'applecoloremoji'],
              label: 'Apple Color Emoji',
            },
            {
              type: 'url',
              url: 'https://cdn.jsdelivr.net/gh/googlefonts/noto-emoji@main/fonts/NotoColorEmoji.ttf',
            },
            {
              type: 'url',
              url: 'https://cdn.jsdelivr.net/gh/hfg-gmuend/openmoji@master/font/OpenMoji-black-glyf/OpenMoji-black-glyf.ttf',
            },
            {
              type: 'url',
              url: 'https://cdn.jsdelivr.net/gh/notofonts/noto-cjk@main/Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf',
            },
          ],
          maxScrollbackBytes: 10_000_000,
          touchSelectionMode: 'long-press',
          callbacks: {
            onLog: (line) => console.debug(`[restty] ${line}`),
            onBackend: () => {
              state.backendReady = true
              if (!isMobile) state.restty?.focus()
            },
            onTermSize: (cols, rows) => onTermSize(cols, rows),
          },
        },
      })

      // 4. File paste/drop
      bindFileDrop()
    }

    init()

    // Cleanup
    return () => {
      state.destroyed = true

      // Send focus-out
      if (state.focused) {
        state.focused = false
        state.transport?.sendInput('\x1b[O')
      }

      // Presence
      document.removeEventListener('visibilitychange', onVisibilityChange)
      ACTIVITY_EVENTS.forEach((e) =>
        document.removeEventListener(e, onActivity)
      )
      clearTimeout(afkTimer)

      // Viewport
      if (viewportHandler && window.visualViewport) {
        window.visualViewport.removeEventListener('resize', viewportHandler)
        window.visualViewport.removeEventListener('scroll', viewportHandler)
        document.body.style.removeProperty('--kb-height')
        delete document.body.dataset.mobileKeyboard
      }

      // Listeners
      for (const teardown of state.listeners) teardown()
      state.listeners = []
      state.restoreImeFocus?.()

      // Momentum
      if (state.momentumRafId) cancelAnimationFrame(state.momentumRafId)
      if (state.toastTimeout) clearTimeout(state.toastTimeout)
      if (state.connectPtyTimer) clearTimeout(state.connectPtyTimer)

      // Hub bridge
      if (bridgeConnectionId != null) disconnect(bridgeConnectionId)

      // Resources
      state.hubTransport?.release()
      state.hubTransport = null
      state.restty?.destroy()
      state.restty = null
      state.transport?.destroy()
      state.transport = null
      state.overlay?.remove()
      state.overlay = null

      stateRef.current = null
    }
  }, [hubId, sessionUuid])

  const isMobile = typeof window !== 'undefined' && 'ontouchstart' in window

  return (
    <div className="h-dvh flex flex-col overflow-hidden mobile-keyboard:h-(--kb-height)">
      {/* Header */}
      <div className="shrink-0 border-b border-zinc-800 bg-zinc-900/50 mobile-keyboard:hidden">
        <div className="px-3 py-2 lg:px-4 lg:py-3">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2 lg:gap-4 min-w-0">
              <div className="flex items-center bg-zinc-800/50 rounded p-0.5">
                <span className="px-2 py-1 text-xs font-medium rounded bg-zinc-700 text-zinc-100">
                  Terminal
                </span>
              </div>
            </div>
            <div className="flex items-center gap-4">
              <ConnectionStatus hubId={hubId} />
              <span className="text-xs text-zinc-600 hidden lg:inline">
                scroll: mouse wheel
              </span>
            </div>
          </div>
        </div>
      </div>

      {/* Terminal Panel */}
      <div className="flex-1 min-h-0 flex flex-col">
        <div className="flex-1 flex flex-col min-h-0 bg-zinc-950 overflow-hidden">
          {/* Terminal content */}
          <div
            ref={containerRef}
            className="terminal-container relative flex-1 min-h-0 bg-zinc-950"
          >
            {/* Drop zone overlay */}
            <div
              ref={dropZoneRef}
              className="absolute inset-0 z-20 flex items-center justify-center pointer-events-none opacity-0 transition-opacity duration-150 data-[visible]:opacity-100"
              aria-hidden="true"
            >
              <div className="absolute inset-2 rounded-lg border-2 border-dashed border-primary-400/50 bg-primary-500/5" />
              <span className="relative text-sm font-medium text-primary-300">
                Drop file
              </span>
            </div>

            {/* File transfer toast */}
            <output
              ref={toastRef}
              role="status"
              className="absolute bottom-4 left-1/2 -translate-x-1/2 z-30 px-3 py-1.5 rounded-md border text-xs font-medium opacity-0 transition-opacity duration-300 pointer-events-none data-[visible]:opacity-100 data-[toast-type=success]:bg-zinc-800/90 data-[toast-type=success]:text-zinc-200 data-[toast-type=success]:border-zinc-700/50 data-[toast-type=error]:bg-red-900/90 data-[toast-type=error]:text-red-200 data-[toast-type=error]:border-red-700/50"
            />
          </div>
        </div>
      </div>

      {/* Mobile Touch Controls */}
      {isMobile && (
        <div className="shrink-0 border-t border-zinc-700/50 bg-zinc-900/95 backdrop-blur-sm pb-[env(safe-area-inset-bottom)]">
          <div className="flex items-center justify-between px-2 py-1">
            <div className="flex items-center gap-0.5">
              <button
                type="button"
                onClick={() => sendKey('\x03')}
                className="px-2 py-1 text-xs font-mono font-medium text-red-300 hover:bg-red-500/10 rounded transition-colors"
              >
                ^C
              </button>
              <button
                type="button"
                onClick={() => sendKey('\x1b')}
                className="px-2 py-1 text-xs font-medium text-zinc-300 hover:bg-zinc-800 rounded transition-colors"
              >
                Esc
              </button>
              <button
                type="button"
                onClick={() => sendKey('\t')}
                className="px-2 py-1 text-xs font-medium text-zinc-300 hover:bg-zinc-800 rounded transition-colors"
              >
                Tab
              </button>
              <button
                type="button"
                onClick={() => sendKey('\r')}
                className="px-2 py-1 text-xs font-medium text-primary-300 bg-primary-500/10 hover:bg-primary-500/20 rounded transition-colors"
              >
                Enter
              </button>
            </div>
            <div className="flex items-center gap-0.5">
              <button
                type="button"
                onClick={() => sendKey('\x1b[D')}
                className="p-1 text-zinc-400 hover:bg-zinc-800 rounded-full transition-colors"
              >
                <svg className="size-4" viewBox="0 0 20 20" fill="currentColor">
                  <path
                    fillRule="evenodd"
                    d="M11.78 5.22a.75.75 0 010 1.06L8.06 10l3.72 3.72a.75.75 0 11-1.06 1.06l-4.25-4.25a.75.75 0 010-1.06l4.25-4.25a.75.75 0 011.06 0z"
                    clipRule="evenodd"
                  />
                </svg>
              </button>
              <button
                type="button"
                onClick={() => sendKey('\x1b[B')}
                className="p-1 text-zinc-400 hover:bg-zinc-800 rounded-full transition-colors"
              >
                <svg className="size-4" viewBox="0 0 20 20" fill="currentColor">
                  <path
                    fillRule="evenodd"
                    d="M5.22 8.22a.75.75 0 011.06 0L10 11.94l3.72-3.72a.75.75 0 111.06 1.06l-4.25 4.25a.75.75 0 01-1.06 0l-4.25-4.25a.75.75 0 010-1.06z"
                    clipRule="evenodd"
                  />
                </svg>
              </button>
              <button
                type="button"
                onClick={() => sendKey('\x1b[A')}
                className="p-1 text-zinc-400 hover:bg-zinc-800 rounded-full transition-colors"
              >
                <svg className="size-4" viewBox="0 0 20 20" fill="currentColor">
                  <path
                    fillRule="evenodd"
                    d="M14.78 11.78a.75.75 0 01-1.06 0L10 8.06l-3.72 3.72a.75.75 0 01-1.06-1.06l4.25-4.25a.75.75 0 011.06 0l4.25 4.25a.75.75 0 010 1.06z"
                    clipRule="evenodd"
                  />
                </svg>
              </button>
              <button
                type="button"
                onClick={() => sendKey('\x1b[C')}
                className="p-1 text-zinc-400 hover:bg-zinc-800 rounded-full transition-colors"
              >
                <svg className="size-4" viewBox="0 0 20 20" fill="currentColor">
                  <path
                    fillRule="evenodd"
                    d="M8.22 5.22a.75.75 0 011.06 0l4.25 4.25a.75.75 0 010 1.06l-4.25 4.25a.75.75 0 01-1.06-1.06L11.94 10 8.22 6.28a.75.75 0 010-1.06z"
                    clipRule="evenodd"
                  />
                </svg>
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
