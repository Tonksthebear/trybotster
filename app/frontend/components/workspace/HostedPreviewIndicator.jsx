import React from 'react'
import { Badge, BadgeButton } from '../catalyst/badge'
import { safeUrl } from '../../lib/actions'

const statusColor = {
  running: 'emerald',
  starting: 'amber',
  error: 'red',
}

const statusLabel = {
  running: 'Running',
  starting: 'Starting\u2026',
  error: 'Error',
}

export default function HostedPreviewIndicator({ status, url, error, density }) {
  const visible = status === 'running' || status === 'starting' || status === 'error'
  if (!visible) return null

  const color = statusColor[status]
  const label = statusLabel[status]
  const validUrl = safeUrl(url)
  const isClickable = status === 'running' && validUrl

  if (isClickable) {
    return (
      <BadgeButton
        color={color}
        href={validUrl}
        target="_blank"
        rel="noopener noreferrer"
        onClick={(e) => e.stopPropagation()}
        title="Open Cloudflare preview"
        className="shrink-0"
      >
        {label}
      </BadgeButton>
    )
  }

  return (
    <Badge
      color={color}
      className="shrink-0"
      title={
        status === 'starting'
          ? 'Cloudflare preview is starting'
          : error || 'Cloudflare preview status'
      }
    >
      {label}
    </Badge>
  )
}
