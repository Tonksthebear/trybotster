import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import {
  UiTreeBody,
  createTransportDispatch,
} from '../ui_contract'
import { getHub } from '../lib/hub-bridge'

// ---------------------------------------------------------------------------
// Interceptor context
// ---------------------------------------------------------------------------

const InterceptorContext = createContext(null)

/**
 * Register a per-action interceptor on the surrounding `<UiTree>`. Returning
 * truthy from `handler` consumes the action so it does NOT continue down to
 * the transport dispatcher. Returning falsy lets the action proceed normally.
 *
 * Used by composites like `SessionActionsMenu` that catch a hub-emitted
 * action id (e.g. `botster.session.menu.open`) and render their own UI.
 */
export function useUiActionInterceptor(actionId, handler) {
  const ctx = useContext(InterceptorContext)
  if (ctx === null) {
    throw new Error(
      'useUiActionInterceptor must be used inside <UiTree>',
    )
  }
  // Stable handler ref so re-registrations on every render don't fire churn.
  const handlerRef = useRef(handler)
  useEffect(() => {
    handlerRef.current = handler
  }, [handler])

  useEffect(() => {
    return ctx.register(actionId, (action, source) =>
      handlerRef.current?.(action, source),
    )
  }, [actionId, ctx])
}

/**
 * Read the surrounding UiTree's dispatch function. Composites that need to
 * fire actions back through the same transport (e.g. menu items dispatching
 * preview.toggle) call this and forward to it.
 */
export function useUiTreeDispatch() {
  const ctx = useContext(InterceptorContext)
  if (ctx === null) {
    throw new Error(
      'useUiTreeDispatch must be used inside <UiTree>',
    )
  }
  return ctx.dispatch
}

// ---------------------------------------------------------------------------
// Error boundary
// ---------------------------------------------------------------------------

class UiTreeErrorBoundary extends React.Component {
  constructor(props) {
    super(props)
    this.state = { error: null }
  }

  static getDerivedStateFromError(error) {
    return { error }
  }

  componentDidCatch(error, errorInfo) {
    console.error('[UiTree] render error', error, errorInfo)
  }

  componentDidUpdate(prevProps) {
    // When the tree changes (a fresh broadcast arrived) clear any prior
    // error so the new tree gets a chance to render.
    if (this.state.error && prevProps.tree !== this.props.tree) {
      this.setState({ error: null })
    }
  }

  render() {
    if (this.state.error) {
      return this.props.fallback({ error: this.state.error })
    }
    return this.props.children
  }
}

function defaultErrorFallback({ error }) {
  return (
    <div className="rounded-md border border-red-500/30 bg-red-500/10 p-3 text-sm text-red-300">
      <div className="font-medium">UI tree failed to render</div>
      <div className="mt-1 text-xs text-red-400/80">
        {error?.message || String(error)}
      </div>
    </div>
  )
}

function defaultLoadingFallback() {
  return (
    <div className="flex items-center justify-center p-4 text-sm text-zinc-500">
      Loading…
    </div>
  )
}

// ---------------------------------------------------------------------------
// UiTree mount
// ---------------------------------------------------------------------------

/**
 * Hub-subscribing React island that renders the active layout tree for a
 * given `targetSurface`. Wire shape (confirmed with Phase 2b transport):
 *
 *     { type: "ui_layout_tree_v1", target_surface: string,
 *       tree: UiNodeV1, version: string, hub_id: string }
 *
 * Mount points (Phase 2c): the workspace surface splits into
 *   - `targetSurface="workspace_sidebar"` for the AppShell sidebar
 *   - `targetSurface="workspace_panel"` for HubShow's main panel
 * Phase 2b broadcasts both with the appropriate `state.surface` density.
 *
 * Children are rendered inside the same `InterceptorContext` so composites
 * like `<SessionActionsMenu>` can register handlers via
 * `useUiActionInterceptor`.
 */
export default function UiTree({
  hubId,
  targetSurface,
  capabilities,
  initialTree = null,
  loadingFallback = defaultLoadingFallback,
  errorFallback = defaultErrorFallback,
  children,
}) {
  const [tree, setTree] = useState(initialTree)
  const [transport, setTransport] = useState(null)

  // Reset tree + transport synchronously on hubId change. Without this, a
  // hub switch (A → B) keeps the old tree visible until B's first broadcast
  // arrives and routes the user's clicks through A's transport in the
  // interim — cross-hub misrouting. This pattern (compare prop in render,
  // bump state via useRef sentinel) is the React-recommended way to derive
  // state from a changing prop without an extra render pass.
  const lastHubIdRef = useRef(hubId)
  if (lastHubIdRef.current !== hubId) {
    lastHubIdRef.current = hubId
    setTree(null)
    setTransport(null)
  }

  // Acquire the hub transport (HubTransport) for send + message subscription.
  // hub-bridge.connect() resolves asynchronously, so we poll briefly until
  // getHub(hubId) returns a session. Bounded to MAX_POLL_MS so a hub that
  // never connects (network failure, paused tab) does not leak a forever
  // timer. After that, the dispatch path falls through to the legacy
  // fallback for well-known action ids and the loading fallback stays put
  // until either a tree arrives or hubId changes.
  useEffect(() => {
    if (!hubId) {
      setTransport(null)
      return undefined
    }
    let cancelled = false
    let pollTimer = null
    const startedAt = Date.now()
    const MAX_POLL_MS = 10000
    const POLL_INTERVAL_MS = 100

    function attach() {
      if (cancelled) return
      const hub = getHub(hubId)
      const t = hub?.transport ?? null
      if (t) {
        setTransport(t)
        return
      }
      if (Date.now() - startedAt >= MAX_POLL_MS) return
      pollTimer = window.setTimeout(attach, POLL_INTERVAL_MS)
    }
    attach()

    return () => {
      cancelled = true
      if (pollTimer) window.clearTimeout(pollTimer)
    }
  }, [hubId])

  // Subscribe to ui_layout_tree_v1 frames matching this target_surface.
  useEffect(() => {
    if (!transport || !targetSurface) return undefined
    const handler = (message) => {
      if (!message || message.type !== 'ui_layout_tree_v1') return
      if (message.target_surface !== targetSurface) return
      setTree(message.tree ?? null)
    }
    return transport.on('message', handler)
  }, [transport, targetSurface])

  // -------- Interceptor registry --------
  const interceptorsRef = useRef(new Map())

  const register = useCallback((actionId, handler) => {
    let set = interceptorsRef.current.get(actionId)
    if (!set) {
      set = new Set()
      interceptorsRef.current.set(actionId, set)
    }
    set.add(handler)
    return () => {
      const s = interceptorsRef.current.get(actionId)
      if (!s) return
      s.delete(handler)
      if (s.size === 0) interceptorsRef.current.delete(actionId)
    }
  }, [])

  // -------- Dispatch chain --------
  const transportDispatch = useMemo(
    () =>
      createTransportDispatch({
        transport,
        hubId: hubId ?? '',
        targetSurface,
      }),
    [transport, hubId, targetSurface],
  )

  const dispatch = useCallback(
    (action, source) => {
      if (!action) return
      // Try interceptors first; first truthy return consumes the action.
      const set = interceptorsRef.current.get(action.id)
      if (set) {
        for (const handler of set) {
          try {
            if (handler(action, source) === true) return
          } catch (err) {
            console.error('[UiTree] interceptor threw', err)
          }
        }
      }
      transportDispatch(action, source)
    },
    [transportDispatch],
  )

  const interceptorValue = useMemo(
    () => ({ register, dispatch }),
    [register, dispatch],
  )

  return (
    <InterceptorContext.Provider value={interceptorValue}>
      <UiTreeErrorBoundary tree={tree} fallback={errorFallback}>
        {tree ? (
          <UiTreeBody
            node={tree}
            dispatch={dispatch}
            capabilities={capabilities}
            hubId={hubId}
          />
        ) : (
          loadingFallback()
        )}
      </UiTreeErrorBoundary>
      {children}
    </InterceptorContext.Provider>
  )
}
