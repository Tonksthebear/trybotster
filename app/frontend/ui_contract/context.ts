import { createContext, useContext } from 'react'
import type { UiActionV1, UiCapabilitySetV1, UiViewportV1 } from './types'

/**
 * Called when a primitive's bound action is activated. Implementations route
 * to the hub transport (e.g. `hub.selectAgent`, `hub.toggleHostedPreview`),
 * browser navigation, or local UI state depending on the action id.
 */
export type ActionDispatch = (action: UiActionV1) => void

/** Context handed to every primitive renderer by the interpreter. */
export type RenderContext = {
  viewport: UiViewportV1
  capabilities: UiCapabilitySetV1
  dispatch: ActionDispatch
}

/** Default web capability set. Renderers may override in tests. */
export const DEFAULT_WEB_CAPABILITIES: UiCapabilitySetV1 = {
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
