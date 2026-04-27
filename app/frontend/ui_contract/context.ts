import { createContext, useContext } from 'react'
import type { UiAction, UiCapabilitySet, UiViewport } from './types'

/**
 * Optional metadata a primitive may attach when dispatching an action. Used
 * by composite interceptors (e.g. SessionActionsMenu) that need the trigger
 * element to anchor a dropdown.
 */
export type ActionDispatchSource = {
  /** The DOM element that triggered the action (e.g. the clicked button). */
  element?: Element | null
}

/**
 * Called when a primitive's bound action is activated. Implementations route
 * the `UiAction` envelope to hub transport (Phase 2b's `ui_action` wire
 * message), local store updates, or composite interceptors depending on the
 * action id.
 */
export type ActionDispatch = (
  action: UiAction,
  source?: ActionDispatchSource,
) => void

/** Context handed to every primitive renderer by the interpreter. */
export type RenderContext = {
  viewport: UiViewport
  capabilities: UiCapabilitySet
  dispatch: ActionDispatch
  /**
   * Current hub id, threaded through by <UiTree>. Web-specific; optional
   * because pure interpreter tests may not supply one. Renderers can use it
   * to construct URLs (e.g. session row anchor hrefs) so right-click /
   * middle-click browser navigation works.
   */
  hubId?: string
}

/** Default web capability set. Renderers may override in tests. */
export const DEFAULT_WEB_CAPABILITIES: UiCapabilitySet = {
  hover: true,
  dialog: true,
  tooltip: true,
  externalLinks: true,
  binaryTerminalSnapshots: true,
}

const RenderContextCtx = createContext<RenderContext | null>(null)

export const RenderContextProvider = RenderContextCtx.Provider

export function useRenderContext(): RenderContext {
  const ctx = useContext(RenderContextCtx)
  if (!ctx) {
    throw new Error(
      'ui_contract: useRenderContext called outside RenderContextProvider',
    )
  }
  return ctx
}
