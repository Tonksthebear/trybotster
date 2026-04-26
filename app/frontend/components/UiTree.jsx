import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from 'react'
import {
  UiTreeBody,
  createTransportDispatch,
} from '../ui_contract'
import { waitForHub } from '../lib/hub-bridge'
import { useSurfaceReadinessStore } from '../store/surface-readiness-store'

// ---------------------------------------------------------------------------
// Wire protocol: pre-dispatch tree decoration is gone. The v1 flow
// (hub tree → applyCollapseOverrides → applyNavSelectionOverrides →
// interpreter) turned into (hub tree → interpreter), because:
//
//   * Workspace collapse state now lives on
//     `useUiPresentationStore.collapsedWorkspaceIds` and is read by
//     `<SessionList>` directly when it expands the `ui.session_list{}`
//     composite. No tree walk needed.
//   * Nav-selection highlighting is a future `<NavTreeItem>` wrapper — the
//     hub no longer emits nav entries as tree_items, so the decorator
//     had nothing to match against anyway.
//
// The `useCurrentPathname` hook survives below because `<UiTree>` still
// subscribes to popstate so URL-driven re-renders land on the correct
// selection slice of the presentation store.
// ---------------------------------------------------------------------------

/** Subscribe `useSyncExternalStore` to browser history updates. Triggers
 *  a re-render of any UiTree instance whose selection or nav highlight
 *  depends on the current URL. */
function subscribeToPathname(onChange) {
  if (typeof window === 'undefined') return () => {}
  window.addEventListener('popstate', onChange)
  return () => window.removeEventListener('popstate', onChange)
}

function getPathnameSnapshot() {
  return typeof window !== 'undefined' ? window.location.pathname : ''
}

function getServerPathnameSnapshot() {
  // SSR-safe placeholder. The project doesn't SSR this island but keeping
  // a distinct value avoids hydration mismatches if that ever changes.
  return ''
}

function useCurrentPathname() {
  return useSyncExternalStore(
    subscribeToPathname,
    getPathnameSnapshot,
    getServerPathnameSnapshot,
  )
}

// ---------------------------------------------------------------------------
// Interceptor context
// ---------------------------------------------------------------------------

const InterceptorContext = createContext(null)
const treeSnapshotCache = new Map()

function treeCacheKey(hubId, targetSurface, subpath = '/') {
  if (!hubId || !targetSurface) return null
  const normalisedSubpath =
    typeof subpath === 'string' && subpath !== '' ? subpath : '/'
  return `${String(hubId)}\u0000${targetSurface}\u0000${normalisedSubpath}`
}

function readCachedTree(hubId, targetSurface, subpath) {
  const key = treeCacheKey(hubId, targetSurface, subpath)
  return key ? (treeSnapshotCache.get(key) ?? null) : null
}

function writeCachedTree(hubId, targetSurface, subpath, tree) {
  const key = treeCacheKey(hubId, targetSurface, subpath)
  if (!key || !tree) return
  treeSnapshotCache.set(key, tree)
}

export function _resetUiTreeSnapshotCacheForTests() {
  treeSnapshotCache.clear()
}

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
 * given `targetSurface`. Wire shape:
 *
 *     { type: "ui_tree_snapshot", target_surface: string,
 *       tree: UiNodeV1, tree_version: string, hub_id: string }
 *
 * Mount points: the workspace surface splits into
 *   - `targetSurface="workspace_sidebar"` for the AppShell sidebar
 *   - `targetSurface="workspace_panel"` for HubShow's main panel
 * The hub broadcasts both with the appropriate density.
 *
 * Children are rendered inside the same `InterceptorContext` so composites
 * like `<SessionActionsMenu>` can register handlers via
 * `useUiActionInterceptor`.
 */
export default function UiTree({
  hubId,
  targetSurface,
  subpath = '/',
  capabilities,
  initialTree = null,
  loadingFallback = defaultLoadingFallback,
  errorFallback = defaultErrorFallback,
  children,
}) {
  const [tree, setTree] = useState(() =>
    initialTree ?? readCachedTree(hubId, targetSurface, subpath),
  )
  const [transport, setTransport] = useState(null)

  // Wire protocol: collapse + nav-selection state moved into the
  // composite primitives themselves. `<SessionList>` reads collapse state
  // from `useUiPresentationStore.collapsedWorkspaceIds`; the nav-selection
  // highlight lives inside the (future) `NavTreeItem` wrapper. The hub
  // tree no longer needs decoration passes — it ships the same composite
  // bundle to every subscriber.
  //
  // We still subscribe to URL changes so URL-driven nav re-renders (e.g.
  // selection following a back-button press); the subscription is a no-op
  // for layouts that don't depend on URL state.
  const _currentPathname = useCurrentPathname()
  const decoratedTree = tree

  // Reset tree + transport synchronously on hubId change. Without this, a
  // hub switch (A → B) keeps the old tree visible until B's first broadcast
  // arrives and routes the user's clicks through A's transport in the
  // interim — cross-hub misrouting. This pattern (compare prop in render,
  // bump state via useRef sentinel) is the React-recommended way to derive
  // state from a changing prop without an extra render pass.
  const lastHubIdRef = useRef(hubId)
  if (lastHubIdRef.current !== hubId) {
    lastHubIdRef.current = hubId
    setTree(readCachedTree(hubId, targetSurface, subpath))
    setTransport(null)
  }

  // Subpath OR targetSurface changes within a mount must reset the tree.
  // The hub renders a different tree for each subpath within a
  // surface AND for each surface entirely; keeping the stale tree visible
  // while the new one arrives would flash the wrong content for one
  // frame. Pair this with the subpath filter in the frame subscriber
  // below (accept only frames whose subpath matches the current prop) so
  // we're guaranteed the next render is for the new subpath, not a
  // late-arriving frame from the old.
  const lastSurfaceRef = useRef(targetSurface)
  const lastSubpathRef = useRef(subpath)
  if (
    lastSurfaceRef.current !== targetSurface ||
    lastSubpathRef.current !== subpath
  ) {
    lastSurfaceRef.current = targetSurface
    lastSubpathRef.current = subpath
    setTree(readCachedTree(hubId, targetSurface, subpath))
  }

  // Read the route-owned hub transport (HubTransport) for send + message
  // subscription. `hub-store` is the single owner that calls
  // hub-bridge.connect(); leaf components wait for that shared session.
  // After timeout, hub-directed clicks are ignored until a transport is
  // available; browser-local actions still run through their local handlers.
  useEffect(() => {
    if (!hubId) {
      setTransport(null)
      return undefined
    }
    let cancelled = false
    waitForHub(hubId).then((hub) => {
      if (cancelled) return
      const t = hub?.transport ?? null
      if (t) {
        setTransport(t)
      }
    })

    return () => {
      cancelled = true
    }
  }, [hubId])

  // Subscribe to ui_tree_snapshot frames matching this target_surface.
  //
  // Subpath filter: the hub echoes back the subpath it routed for, so a
  // frame produced from the OLD subpath (still en route when the URL
  // changed) is discarded. A frame without a subpath field falls through
  // the filter, which is the right behaviour for surfaces that don't
  // declare any sub-routes.
  useEffect(() => {
    if (!transport || !targetSurface) return undefined
    const handler = (message) => {
      if (!message || message.type !== 'ui_tree_snapshot') {
        return
      }
      if (message.target_surface !== targetSurface) return
      if (
        typeof message.subpath === 'string' &&
        message.subpath !== subpath
      ) {
        return
      }
      const nextTree = message.tree ?? null
      setTree(nextTree)
      writeCachedTree(hubId, targetSurface, subpath, nextTree)
      // System-test readiness: first tree for this (hub, surface) pair
      // records into the readiness store, feeding `<html data-hub-snapshot>`.
      // Record on EVERY frame: the store is idempotent per (hub, surface),
      // so this is cheap, and it means hub-switch + remount still records
      // correctly after `clearForHub`.
      if (hubId) {
        useSurfaceReadinessStore
          .getState()
          .recordFirstTree(hubId, targetSurface)
      }
    }
    return transport.on('message', handler)
  }, [transport, targetSurface, subpath, hubId])

  // Whenever the (target_surface, subpath) pair changes, tell the hub so it
  // can re-render for this client.
  //
  // We send unconditionally on every mount / change. Idempotency lives on
  // the hub: `Client:set_surface_subpath` early-returns when the incoming
  // subpath equals the previously-stored value, so repeating what the
  // subscribe envelope already primed is a cheap no-op.
  //
  // An earlier revision tried to skip the first send on a "cold transport"
  // under the assumption that a single UiTree instance would persist
  // across surface transitions. In practice UiTree is mounted by
  // different parent routes (HubShow for workspace_panel, DynamicSurface
  // for plugin surfaces), so crossing a Route boundary mounts a FRESH
  // UiTree instance whose per-instance skip-cache looks "cold" even
  // though the transport has been open for a while. That left the hub
  // without the new surface's subpath and the UiTree stuck on loading.
  // The always-send path above sidesteps the remount-vs-rerender
  // distinction entirely.
  useEffect(() => {
    if (!transport || !targetSurface) return undefined
    const normalisedSubpath =
      typeof subpath === 'string' && subpath !== '' ? subpath : '/'

    // transport.send returns a Promise that REJECTS when the DataChannel
    // isn't open yet (e.g. cold mount during WebRTC handshake). If we
    // don't attach a .catch handler, the rejection surfaces as an
    // "Uncaught Error: DataChannel closed" in the browser console —
    // harmless for the user (the subscribe envelope already primed the
    // hub and the hub will dedup if we resend later) but the system
    // tests assert on an empty console.
    let sendPromise
    try {
      sendPromise = transport.send('ui_action', {
        target_surface: targetSurface,
        envelope: {
          id: 'botster.surface.subpath',
          payload: { target_surface: targetSurface, subpath: normalisedSubpath },
        },
      })
    } catch (err) {
      console.warn('[UiTree] failed to send surface.subpath', err)
      return undefined
    }
    if (sendPromise && typeof sendPromise.catch === 'function') {
      sendPromise.catch((err) => {
        console.warn('[UiTree] surface.subpath send rejected', err)
      })
    }
    return undefined
  }, [transport, targetSurface, subpath])

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

  // System-test readiness attributes: Capybara waits `[data-surface-ready=X]`
  // to exist and `[...state=loading]` to disappear before interacting with the
  // surface. The wrapper div is a zero-cost addition (display: contents) so it
  // doesn't affect layout of the hub-authored tree inside.
  const surfaceReadyState = tree ? 'ready' : 'loading'

  return (
    <InterceptorContext.Provider value={interceptorValue}>
      <div
        data-surface-ready={targetSurface || undefined}
        data-surface-ready-state={targetSurface ? surfaceReadyState : undefined}
        style={{ display: 'contents' }}
      >
        <UiTreeErrorBoundary tree={decoratedTree} fallback={errorFallback}>
          {decoratedTree ? (
            <UiTreeBody
              node={decoratedTree}
              dispatch={dispatch}
              capabilities={capabilities}
              hubId={hubId}
            />
          ) : (
            loadingFallback()
          )}
        </UiTreeErrorBoundary>
        {children}
      </div>
    </InterceptorContext.Provider>
  )
}
