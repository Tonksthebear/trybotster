import React from 'react'
import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'

import { SessionTerminal } from '../components/composites/SessionTerminal'

vi.mock('../components/terminal/TerminalView', () => ({
  default: ({ hubId, sessionUuid }) => (
    <div data-testid="terminal-view">{`${hubId}:${sessionUuid}`}</div>
  ),
}))

describe('<SessionTerminal>', () => {
  it('mounts the core terminal viewer for the requested session', () => {
    render(
      <SessionTerminal
        sessionUuid="sess-1"
        ctx={{ hubId: 'hub-1' }}
      />,
    )

    expect(screen.getByTestId('terminal-view')).toHaveTextContent('hub-1:sess-1')
    const wrapper = screen.getByTestId('terminal-view').parentElement
    expect(wrapper).toHaveClass('h-full')
    expect(wrapper).toHaveClass('min-h-0')
    expect(wrapper).toHaveClass('overflow-hidden')
    expect(wrapper).not.toHaveClass('min-h-[70vh]')
  })
})
