import React from 'react'
import clsx from 'clsx'
import {
  UiTree,
  createHubDispatch,
  DEFAULT_WEB_CAPABILITIES,
} from '../../ui_contract'
import { hostedPreviewIndicator } from '../../ui_contract/composites'

export default function HostedPreviewIndicator({
  sessionId,
  sessionUuid,
  hubId,
  status,
  url,
  error,
  density = 'panel',
  capabilities,
}) {
  const result = hostedPreviewIndicator({
    sessionId: sessionId ?? '',
    sessionUuid: sessionUuid ?? '',
    hubId: hubId ?? '',
    status,
    url,
    error,
    density,
  })
  if (!result.node) return null

  // Capability-gated native tooltip. Badge/Button primitives don't carry a
  // tooltip prop — keeping them spec-clean — so we wrap at the composite
  // layer when `capabilities.tooltip === true`. Per Phase A's
  // UiCapabilitySetV1: callers pass the live capability set to override the
  // default. Without a caller override we fall back to DEFAULT_WEB_CAPABILITIES
  // so plain browser surfaces keep working. The SAME capability set flows into
  // UiTree so primitives render under the same assumptions.
  const effectiveCapabilities = capabilities ?? DEFAULT_WEB_CAPABILITIES

  const tree = (
    <UiTree
      node={result.node}
      dispatch={createHubDispatch(hubId ?? '')}
      capabilities={effectiveCapabilities}
    />
  )

  const supportsTooltip = effectiveCapabilities.tooltip === true
  const content =
    supportsTooltip && result.tooltipTitle ? (
      <span title={result.tooltipTitle}>{tree}</span>
    ) : (
      tree
    )

  return (
    <span
      className={clsx(
        'inline-flex shrink-0',
        density === 'sidebar' ? 'mr-1' : 'mr-2',
      )}
      onClick={(e) => e.stopPropagation()}
    >
      {content}
    </span>
  )
}
