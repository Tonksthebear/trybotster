import { describe, it, expect, beforeEach, afterEach } from 'vitest'
import { render, screen, cleanup } from '@testing-library/react'
import { ConnectionCode } from '../components/composites/ConnectionCode'
import { useConnectionCodeStore } from '../store/entities/hub-meta-store'

const ctx = { hubId: 'hub-1', surface: 'workspace_panel' }

describe('ConnectionCode composite', () => {
  beforeEach(() => {
    useConnectionCodeStore.getState()._reset()
  })

  afterEach(() => {
    cleanup()
    useConnectionCodeStore.getState()._reset()
  })

  it('renders a "generating" message before the entity arrives', () => {
    render(<ConnectionCode ctx={ctx} />)
    expect(screen.getByText(/generating qr code/i)).toBeInTheDocument()
  })

  it('renders the URL as a link and the QR ASCII when the entity is present', () => {
    useConnectionCodeStore.setState({
      byId: {
        'hub-local-id': {
          hub_id: 'hub-local-id',
          url: 'https://dev.trybotster.com/hubs/abc/pairing#BUNDLE',
          qr_ascii: '██  ██\n  ████',
        },
      },
      order: ['hub-local-id'],
      snapshotSeq: 1,
    })

    render(<ConnectionCode ctx={ctx} />)

    const link = screen.getByRole('link', { name: /pairing#BUNDLE/i })
    expect(link).toHaveAttribute('href', 'https://dev.trybotster.com/hubs/abc/pairing#BUNDLE')
    expect(link).toHaveAttribute('target', '_blank')
    expect(screen.getByRole('button', { name: /copy pairing url/i })).toBeInTheDocument()
    const qr = document.querySelector('pre')
    expect(qr).not.toBeNull()
    expect(qr?.textContent).toContain('████')
  })

  it('surfaces the entity error message when the hub reports one', () => {
    useConnectionCodeStore.setState({
      byId: {
        'hub-local-id': {
          hub_id: 'hub-local-id',
          error: 'Connection code generation timed out',
        },
      },
      order: ['hub-local-id'],
      snapshotSeq: 1,
    })

    render(<ConnectionCode ctx={ctx} />)
    expect(screen.getByText(/timed out/i)).toBeInTheDocument()
  })
})
