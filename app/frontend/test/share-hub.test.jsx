import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import ShareHub from '../components/hub/ShareHub'
import { useConnectionCodeStore } from '../store/entities'

const requestConnectionCode = vi.hoisted(() => vi.fn())

vi.mock('../lib/hub-bridge', () => ({
  waitForHub: vi.fn(async () => ({ requestConnectionCode })),
}))

describe('ShareHub', () => {
  beforeEach(() => {
    requestConnectionCode.mockReset()
    requestConnectionCode.mockResolvedValue(true)
    useConnectionCodeStore.getState()._reset()
  })

  afterEach(cleanup)

  it('requests the current connection code when cached code exists', async () => {
    useConnectionCodeStore.getState().applyUpsert('42', {
      hub_id: '42',
      url: 'https://example.test/hubs/42/pairing#STALE',
      qr_ascii: ['stale qr'],
    }, 1)

    render(<ShareHub hubId="42" />)
    fireEvent.click(screen.getByRole('button', { name: 'Share' }))

    await waitFor(() => expect(requestConnectionCode).toHaveBeenCalledOnce())
    expect(screen.getByText('Generating connection code...')).toBeInTheDocument()
    expect(screen.queryByText('stale qr')).not.toBeInTheDocument()

    act(() => {
      useConnectionCodeStore.getState().applyUpsert('42', {
        hub_id: '42',
        url: 'https://example.test/hubs/42/pairing#FRESH',
        qr_ascii: ['fresh qr'],
      }, 2)
    })

    expect(await screen.findByText('fresh qr')).toBeInTheDocument()
  })
})
