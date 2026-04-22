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
import { getHub } from '../lib/hub-bridge'
import { useWorkspaceStore } from '../store/workspace-store'

// ---------------------------------------------------------------------------
// Tree decoration — applies browser-local ephemeral UI state (workspace
// collapse/expand) on top of the hub-authored tree before it reaches the
// primitive interpreter. The hub doesn't track per-client collapse state
// (collapse is a purely visual concern owned by the browser per the
// "browser owns ephemeral UI state" invariant), so when a user clicks a
// workspace header:
//
//   1. The action dispatcher (LOCAL_ONLY) routes `botster.workspace.toggle`
//      to `lib/actions.js`, which flips the id in
//      `useWorkspaceStore.collapsedWorkspaceIds`.
//   2. This subscription re-renders with the new Set.
//   3. `applyCollapseOverrides` walks the hub tree and overrides
//      `expanded: false` on matching tree_items.
//   4. The interpreter renders; `renderTreeItem` honours `expanded === false`
//      by hiding the children slot.
//
// Pure function keyed only on `(tree, collapsedIds)` — safe to memoise.
// ---------------------------------------------------------------------------

function applyCollapseOverrides(node, collapsedIds) {
  if (!node || typeof node !== 'object') return node
  // `$kind` discriminator marks a conditional wrapper
  // (`ui.when` / `ui.hidden`). Recurse into its inner node so any
  // descendant tree_items still get their expansion state overridden.
  if (node.$kind === 'when' || node.$kind === 'hidden') {
    return { ...node, node: applyCollapseOverrides(node.node, collapsedIds) }
  }

  const decorated = { ...node }

  if (
    node.type === 'tree_item' &&
    typeof node.id === 'string' &&
    collapsedIds.has(node.id)
  ) {
    decorated.props = { ...(node.props ?? {}), expanded: false }
  }

  if (Array.isArray(node.children)) {
    decorated.children = node.children.map((c) =>
      applyCollapseOverrides(c, collapsedIds),
    )
  }

  if (node.slots && typeof node.slots === 'object') {
    decorated.slots = Object.fromEntries(
      Object.entries(node.slots).map(([name, children]) => [
        name,
        Array.isArray(children)
          ? children.map((c) => applyCollapseOverrides(c, collapsedIds))
          : children,
      ]),
    )
  }

  return decorated
}

// ---------------------------------------------------------------------------
// Nav-selection decorator — applies browser-local "which nav entry is the
// current page" state on top of the hub-authored tree before it reaches the
// primitive interpreter.
//
// Phase 4a registered-surface nav entries use a shared action shape:
//
//     ui.action("botster.nav.open", { path = "/plugins/hello" })
//
// The hub doesn't track per-client URL state — that's a browser-ephemeral
// concern (parallel to the workspace collapse invariant). So we decorate
// here: if a tree_item's action targets `botster.nav.open` with a `path`
// that matches the current hub-scoped URL, set `selected: true` on it.
// Pure function keyed only on `(tree, hubId, pathname)` — safe to memoise.
// ---------------------------------------------------------------------------

/** Subscribe `useSyncExternalStore` to browser history updates. Triggers
 *  a re-render of any UiTree instance whose nav-entry highlight depends
 *  on the current URL. Cross-component because multiple UiTrees can be
 *  mounted (sidebar + panel) simultaneously. */
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

/** React hook wrapping the store-contract listener. */
function useCurrentPathname() {
  return useSyncExternalStore(
    subscribeToPathname,
    getPathnameSnapshot,
    getServerPathnameSnapshot,
  )
}

/** Is `treeItemNode`'s action a `botster.nav.open` whose hub-relative
 *  `path` field, combined with `hubId`, matches `pathname`? Tolerant of
 *  leading-slash discrepancies — the hub's Lua code emits a
 *  leading-slash path ("/plugins/hello"), but users editing overrides
 *  may omit it. */
function navEntryMatchesPathname(actionProp, hubId, pathname) {
  if (!actionProp || typeof actionProp !== 'object') return false
  if (actionProp.id !== 'botster.nav.open') return false
  const path =
    actionProp.payload && typeof actionProp.payload.path === 'string'
      ? actionProp.payload.path
      : null
  if (path == null || typeof hubId !== 'string' || hubId.length === 0) return false

  const trimmed = path.startsWith('/') ? path : '/' + path
  // Root path "/" collapses to "/hubs/<hubId>" (not "/hubs/<hubId>/") so a
  // deep-linked `/hubs/:id` lands on the hub-root nav entry.
  const expected = trimmed === '/' ? `/hubs/${hubId}` : `/hubs/${hubId}${trimmed}`
  if (pathname === expected) return true
  // Also match trailing-slash variants so either is considered "current".
  if (expected === `/hubs/${hubId}` && pathname === `/hubs/${hubId}/`) return true
  if (pathname === expected + '/') return true
  return false
}

function applyNavSelectionOverrides(node, hubId, pathname) {
  if (!node || typeof node !== 'object') return node

  const decorated = { ...node }

  if (node.type === 'tree_item') {
    const action = node.props?.action
    if (navEntryMatchesPathname(action, hubId, pathname)) {
      decorated.props = { ...(node.props ?? {}), selected: true }
    }
  }

  if (Array.isArray(node.children)) {
    decorated.children = node.children.map((c) =>
      applyNavSelectionOverrides(c, hubId, pathname),
    )
  }

  if (node.slots && typeof node.slots === 'object') {
    decorated.slots = Object.fromEntries(
      Object.entries(node.slots).map(([name, children]) => [
        name,
        Array.isArray(children)
          ? children.map((c) => applyNavSelectionOverrides(c, hubId, pathname))
          : children,
      ]),
    )
  }

  return decorated
}

// Exported for vitest coverage without round-tripping through React.
export {
  applyNavSelectionOverrides as _applyNavSelectionOverridesForTests,
  navEntryMatchesPathname as _navEntryMatchesPathnameForTests,
}

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
  subpath = '/',
  capabilities,
  initialTree = null,
  loadingFallback = defaultLoadingFallback,
  errorFallback = defaultErrorFallback,
  children,
}) {
  const [tree, setTree] = useState(initialTree)
  const [transport, setTransport] = useState(null)

  // Browser-local ephemeral UI state — two independent decorators stack
  // on top of the hub-authored tree before it reaches the interpreter:
  //
  //   1. `applyCollapseOverrides` — workspace collapse/expand state owned
  //      by `useWorkspaceStore.collapsedWorkspaceIds`. Triggered by the
  //      LOCAL_ONLY `botster.workspace.toggle` action.
  //   2. `applyNavSelectionOverrides` — Phase 4a nav highlight for the
  //      current URL, subscribed via `useSyncExternalStore` on popstate
  //      so `botster.nav.open` pushState updates re-render automatically.
  //
  // Both are pure functions; order is fixed (collapse first, then
  // selection) because selection walks the same node shape collapse
  // produces and never depends on collapse state. Malformed trees must
  // still reach `UiTreeErrorBoundary` — if decoration blows up, pass the
  // original tree through and let the interpreter's render routes React
  // to the error boundary.
  const collapsedIds = useWorkspaceStore((s) => s.collapsedWorkspaceIds)
  const currentPathname = useCurrentPathname()
  const decoratedTree = useMemo(() => {
    if (!tree) return tree
    try {
      const collapsed = applyCollapseOverrides(tree, collapsedIds)
      return applyNavSelectionOverrides(collapsed, hubId ?? '', currentPathname)
    } catch {
      return tree
    }
  }, [tree, collapsedIds, hubId, currentPathname])

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

  // Phase 4b: subpath OR targetSurface changes within a mount must reset
  // the tree. The hub renders a different tree for each subpath within a
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
    setTree(null)
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
  //
  // Phase 4b: also filter on `subpath`. The hub echoes back the subpath it
  // routed for, so a frame produced from the OLD subpath (still en route
  // when the URL changed) is discarded. A frame without a subpath field
  // (older hub) falls through the filter so back-compat still works.
  useEffect(() => {
    if (!transport || !targetSurface) return undefined
    const handler = (message) => {
      if (!message || message.type !== 'ui_layout_tree_v1') return
      if (message.target_surface !== targetSurface) return
      if (
        typeof message.subpath === 'string' &&
        message.subpath !== subpath
      ) {
        return
      }
      setTree(message.tree ?? null)
    }
    return transport.on('message', handler)
  }, [transport, targetSurface, subpath])

  // Phase 4b: whenever the (target_surface, subpath) pair changes, tell
  // the hub so it can re-render for this client. The initial subpath for
  // the surface the user LANDED on is already primed via the subscribe
  // envelope (`channelParams().surface_subpaths` in `hub_connection.js`);
  // that primed surface+subpath is skipped so we don't double-send on
  // cold load.
  //
  // All other cases fire:
  //   * Cross-surface navigation on the SAME transport (hub has never
  //     heard about the new surface's subpath — the envelope only primed
  //     the one surface the user cold-loaded into).
  //   * Intra-surface navigation (subpath changes within the current
  //     targetSurface).
  //   * Hub switch: the new transport is brand-new, so re-prime from
  //     whatever surface the user is currently viewing.
  //
  // Tracking: per-transport map of `surface -> last-sent subpath`. A
  // brand-new transport installs a single entry reflecting the currently
  // rendered surface (that's what the subscribe envelope primed) and
  // suppresses the send. Every other transition updates the map and
  // sends.
  const subpathSyncRef = useRef({ transport: null, sentBySurface: null })
  useEffect(() => {
    if (!transport || !targetSurface) return undefined
    const sync = subpathSyncRef.current
    const normalisedSubpath =
      typeof subpath === 'string' && subpath !== '' ? subpath : '/'

    const isColdTransport = sync.transport !== transport
    if (isColdTransport) {
      // Brand-new subscription — the subscribe envelope already primed
      // the hub for THIS (surface, subpath). Seed the sent-cache so
      // future renders compare against it.
      subpathSyncRef.current = {
        transport,
        sentBySurface: new Map([[targetSurface, normalisedSubpath]]),
      }
      return undefined
    }

    // Same transport — did we already tell the hub about this
    // (surface, subpath)? If not, send.
    const lastSent = sync.sentBySurface.get(targetSurface)
    if (lastSent === normalisedSubpath) return undefined
    sync.sentBySurface.set(targetSurface, normalisedSubpath)

    // Best-effort — a failed send is harmless; the hub's default is "/"
    // and the next user action will re-fire. No await; fire-and-forget
    // keeps the render path fast.
    try {
      void transport.send('ui_action_v1', {
        target_surface: targetSurface,
        envelope: {
          id: 'botster.surface.subpath',
          payload: { target_surface: targetSurface, subpath: normalisedSubpath },
        },
      })
    } catch (err) {
      console.warn('[UiTree] failed to send surface.subpath', err)
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

  return (
    <InterceptorContext.Provider value={interceptorValue}>
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
    </InterceptorContext.Provider>
  )
}
