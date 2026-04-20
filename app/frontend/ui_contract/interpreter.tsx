import React, { Fragment, type ReactElement, type ReactNode } from 'react'
import {
  DEFAULT_WEB_CAPABILITIES,
  RenderContextProvider,
  type ActionDispatch,
  type RenderContext,
} from './context'
import { PRIMITIVE_REGISTRY, type PrimitiveRenderer } from './registry'
import type {
  UiCapabilitySetV1,
  UiChildV1,
  UiConditionalV1,
  UiNodeV1,
  UiViewportV1,
} from './types'
import { isConditional } from './types'
import { matchesCondition, useViewport } from './viewport'

function renderChild(
  child: UiChildV1,
  ctx: RenderContext,
  key: string | number,
): ReactNode {
  if (isConditional(child)) {
    return renderConditional(child, ctx, key)
  }
  return renderInternal(child, ctx, key)
}

function renderConditional(
  wrapper: UiConditionalV1,
  ctx: RenderContext,
  key: string | number,
): ReactNode {
  const matches = matchesCondition(wrapper.condition, ctx.viewport)
  const shouldRender = wrapper.$kind === 'when' ? matches : !matches
  if (!shouldRender) return null
  return renderInternal(wrapper.node, ctx, key)
}

function renderInternal(
  node: UiNodeV1,
  ctx: RenderContext,
  key: string | number,
): ReactNode {
  const renderer = PRIMITIVE_REGISTRY[node.type] as
    | PrimitiveRenderer
    | undefined
  if (!renderer) {
    if (typeof console !== 'undefined') {
      console.warn(`[ui_contract] unknown primitive type: ${node.type}`)
    }
    return null
  }

  const children: ReactNode[] = (node.children ?? []).map((child, idx) =>
    renderChild(child, ctx, idx),
  )

  const slots: Record<string, ReactNode[]> = {}
  if (node.slots) {
    for (const [slotName, slotChildren] of Object.entries(node.slots)) {
      slots[slotName] = slotChildren.map((child, idx) =>
        renderChild(child, ctx, `${slotName}-${idx}`),
      )
    }
  }

  const element = renderer({
    node,
    props: node.props ?? {},
    children,
    slots,
    ctx,
  })

  return <Fragment key={node.id ?? key}>{element}</Fragment>
}

/** Walk a UiNodeV1 tree into React elements, driven by `ctx`. */
export function renderNode(node: UiNodeV1, ctx: RenderContext): ReactElement {
  const rendered = renderInternal(node, ctx, 'root')
  return <>{rendered}</>
}

export type UiTreeBodyProps = {
  node: UiNodeV1
  dispatch: ActionDispatch
  /** Override the default web capability set if needed (e.g. tests). */
  capabilities?: UiCapabilitySetV1
  /** Inject a viewport for tests; defaults to `useViewport()`. */
  viewport?: UiViewportV1
  /**
   * Current hub id. Used by web renderers to construct URLs (e.g. session
   * anchor hrefs). Optional so pure-interpreter tests don't have to supply
   * one.
   */
  hubId?: string
}

/**
 * Pure tree-walker: renders a supplied `UiNodeV1` through the primitive
 * registry. Owns the `RenderContext` and hooks into `useViewport`.
 *
 * The hub-subscribing mount lives in `components/UiTree.jsx` — it owns
 * transport subscription + interceptor context and delegates rendering to
 * this body component. Keeping transport concerns out of `ui_contract/` so
 * the primitive registry stays a pure view layer.
 */
export function UiTreeBody({
  node,
  dispatch,
  capabilities,
  viewport,
  hubId,
}: UiTreeBodyProps): ReactElement {
  const liveViewport = useViewport()
  const effectiveViewport = viewport ?? liveViewport
  const ctx: RenderContext = {
    viewport: effectiveViewport,
    capabilities: capabilities ?? DEFAULT_WEB_CAPABILITIES,
    dispatch,
    hubId,
  }
  return (
    <RenderContextProvider value={ctx}>
      {renderNode(node, ctx)}
    </RenderContextProvider>
  )
}
