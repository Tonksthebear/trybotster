// Public entry point for the Rails-owned React UI primitive registry.
//
// Phase C of the cross-client UI DSL. Consumes the `UiNode` wire format
// defined by `cli/src/ui_contract/` (Phase A) and renders it with React +
// Catalyst-aligned components.

export type {
  ActionDispatch,
  ActionDispatchSource,
  RenderContext,
} from './context'
export {
  DEFAULT_WEB_CAPABILITIES,
  RenderContextProvider,
  useRenderContext,
} from './context'

export {
  createRawDispatch,
  createTransportDispatch,
} from './dispatch'
export type {
  CreateTransportDispatchOptions,
  UiActionTransport,
} from './dispatch'

export { renderNode, UiTreeBody } from './interpreter'
export type { UiTreeBodyProps } from './interpreter'

export { PRIMITIVE_REGISTRY } from './registry'
export type { PrimitiveRenderer, PrimitiveRendererArgs } from './registry'

export * from './types'

export {
  heightClassForPx,
  HEIGHT_REGULAR_MAX,
  HEIGHT_SHORT_MAX,
  matchesCondition,
  resolveValue,
  useViewport,
  widthClassForPx,
  WIDTH_COMPACT_MAX,
  WIDTH_REGULAR_MAX,
} from './viewport'
