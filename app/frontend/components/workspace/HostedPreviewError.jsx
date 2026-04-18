import React from 'react'
import clsx from 'clsx'
import { UiTree, createHubDispatch } from '../../ui_contract'
import { hostedPreviewErrorInner } from '../../ui_contract/composites'

// HostedPreviewError owns its own danger-tinted Rails-owned div. The Panel
// primitive tone vocabulary is `default | muted` in v1 — we intentionally do
// not extend the shared cross-client spec for a renderer-internal concern,
// per `docs/specs/cross-client-ui-primitives.md` and orchestrator direction.
export default function HostedPreviewError({
  sessionUuid,
  hubId,
  error,
  installUrl,
  density = 'panel',
}) {
  const node = hostedPreviewErrorInner({
    sessionUuid: sessionUuid ?? '',
    error,
    installUrl,
    density,
  })
  if (!node) return null

  const isSidebar = density === 'sidebar'

  return (
    <div className={isSidebar ? 'px-2 pb-2' : 'px-4 pb-3'}>
      <div
        className={clsx(
          'rounded border border-red-500/30 bg-red-500/10',
          isSidebar ? 'px-2 py-1.5' : 'px-3 py-2',
        )}
      >
        <UiTree node={node} dispatch={createHubDispatch(hubId ?? '')} />
      </div>
    </div>
  )
}
