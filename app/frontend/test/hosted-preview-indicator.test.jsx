import React from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render } from '@testing-library/react'

vi.mock('../lib/hub-bridge', () => ({
  getHub: () => null,
}))

import HostedPreviewIndicator from '../components/workspace/HostedPreviewIndicator'
import { DEFAULT_WEB_CAPABILITIES } from '../ui_contract'

afterEach(() => {
  cleanup()
})

describe('HostedPreviewIndicator capability gating', () => {
  it('wraps the tree in <span title> when tooltip capability is true (default)', () => {
    const { container } = render(
      <HostedPreviewIndicator
        sessionId="s-1"
        sessionUuid="u-1"
        hubId="h-1"
        status="starting"
        density="panel"
      />,
    )
    const withTitle = container.querySelector('span[title]')
    expect(withTitle).not.toBeNull()
    expect(withTitle?.getAttribute('title')).toMatch(/starting/i)
  })

  it('omits <span title> when capabilities.tooltip is false', () => {
    const { container } = render(
      <HostedPreviewIndicator
        sessionId="s-1"
        sessionUuid="u-1"
        hubId="h-1"
        status="starting"
        density="panel"
        capabilities={{ ...DEFAULT_WEB_CAPABILITIES, tooltip: false }}
      />,
    )
    const withTitle = container.querySelector('span[title]')
    expect(withTitle).toBeNull()
    // The badge itself still renders — just without the tooltip wrapper.
    expect(container.textContent).toMatch(/Starting/)
  })

  it('passes the caller-provided capability set through to UiTree', () => {
    // The specific capabilities don't affect the badge DOM, but this smoke
    // test ensures passing a non-default capability set does not crash the
    // composite. Primary behavioral coverage is the tooltip gate above.
    const fullCaps = {
      hover: false,
      dialog: false,
      tooltip: false,
      externalLinks: false,
      binaryTerminalSnapshots: false,
    }
    const { container } = render(
      <HostedPreviewIndicator
        sessionId="s-1"
        sessionUuid="u-1"
        hubId="h-1"
        status="running"
        url="https://example.com"
        density="panel"
        capabilities={fullCaps}
      />,
    )
    // Running state renders a button with visible "Running" label regardless.
    expect(container.textContent).toMatch(/Running/)
    expect(container.querySelector('span[title]')).toBeNull()
  })
})
