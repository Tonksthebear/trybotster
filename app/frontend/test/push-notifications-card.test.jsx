import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import { render, screen, cleanup, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'

const sendMock = vi.fn(() => Promise.resolve(true))
const listeners = new Map()
function emit(eventName, payload) {
  listeners.get(eventName)?.forEach((cb) => cb(payload))
}

const fakeHub = {
  send: (...args) => sendMock(...args),
  on: (eventName, cb) => {
    if (!listeners.has(eventName)) listeners.set(eventName, new Set())
    listeners.get(eventName).add(cb)
    return () => listeners.get(eventName)?.delete(cb)
  },
}

vi.mock('../lib/hub-bridge', () => ({
  waitForHub: vi.fn(() => ({
    then(resolve) {
      resolve(fakeHub)
      return Promise.resolve(fakeHub)
    },
  })),
}))

import PushNotificationsCard from '../components/settings/PushNotificationsCard'

describe('PushNotificationsCard', () => {
  beforeEach(() => {
    sendMock.mockClear()
    listeners.clear()
    localStorage.clear()
    // Stub the push APIs vitest's jsdom doesn't provide.
    Object.defineProperty(global.navigator, 'serviceWorker', {
      configurable: true,
      value: {
        register: vi.fn(() => Promise.resolve({
          pushManager: {
            getSubscription: vi.fn(() => Promise.resolve(null)),
            subscribe: vi.fn(() => Promise.resolve({
              toJSON: () => ({
                endpoint: 'https://push.test/abc',
                keys: { p256dh: 'p256dh-key', auth: 'auth-key' },
              }),
            })),
          },
        })),
        getRegistration: vi.fn(() => Promise.resolve(null)),
        get ready() { return Promise.resolve() },
      },
    })
    global.PushManager = function () {}
    global.Notification = {
      requestPermission: vi.fn(() => Promise.resolve('granted')),
    }
  })

  afterEach(() => {
    cleanup()
    delete global.PushManager
    delete global.Notification
  })

  function render_() {
    return render(<PushNotificationsCard hubId="hub-1" />)
  }

  it('queries push status on mount', async () => {
    render_()
    await waitFor(() => {
      expect(sendMock).toHaveBeenCalledWith(
        'push_status_req',
        expect.objectContaining({ type: 'push_status_req' }),
      )
    })
  })

  it('shows the "Enable push" button when CLI has no VAPID keys', async () => {
    render_()
    emit('push:status', { hubId: 'hub-1', hasKeys: false, browserSubscribed: false })

    const enableButton = await screen.findByRole('button', { name: /enable push/i })
    expect(enableButton).toBeInTheDocument()

    const card = screen.getByTestId('push-notifications-card')
    expect(card.dataset.pushState).toBe('no_keys')
  })

  it('shows "Enable on this browser" when CLI has keys but this browser is not subscribed', async () => {
    render_()
    emit('push:status', { hubId: 'hub-1', hasKeys: true, browserSubscribed: false, vapidPub: 'pub' })

    const button = await screen.findByRole('button', { name: /enable on this browser/i })
    expect(button).toBeInTheDocument()
  })

  it('shows Test + Disable when subscribed', async () => {
    render_()
    emit('push:status', { hubId: 'hub-1', hasKeys: true, browserSubscribed: true, vapidPub: 'pub' })

    expect(await screen.findByRole('button', { name: /send test notification/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /disable/i })).toBeInTheDocument()
  })

  it('vapid_generate flow: clicking Enable requests permission then sends vapid_generate', async () => {
    const user = userEvent.setup()
    render_()
    emit('push:status', { hubId: 'hub-1', hasKeys: false, browserSubscribed: false })

    const enableButton = await screen.findByRole('button', { name: /enable push/i })
    await user.click(enableButton)

    expect(global.Notification.requestPermission).toHaveBeenCalled()
    await waitFor(() => {
      expect(sendMock).toHaveBeenCalledWith('vapid_generate', { type: 'vapid_generate' })
    })
  })

  it('subscribes the browser when push:vapid_key arrives, then sends push_sub', async () => {
    render_()
    emit('push:status', { hubId: 'hub-1', hasKeys: true, browserSubscribed: false, vapidPub: 'pub' })

    // Simulate the CLI replying with the VAPID public key (real VAPID keys
    // are base64url-encoded P-256 ECDSA public keys, 65 raw bytes)
    const fakeKey = btoa(String.fromCharCode(...new Uint8Array(65))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
    emit('push:vapid_key', { hubId: 'hub-1', key: fakeKey })

    await waitFor(() => {
      expect(navigator.serviceWorker.register).toHaveBeenCalledWith('/service-worker', { scope: '/' })
    })

    await waitFor(() => {
      expect(sendMock).toHaveBeenCalledWith(
        'push_sub',
        expect.objectContaining({
          type: 'push_sub',
          endpoint: 'https://push.test/abc',
          p256dh: 'p256dh-key',
          auth: 'auth-key',
        }),
      )
    })
  })

  it('flips to subscribed state when push:sub_ack arrives', async () => {
    render_()
    emit('push:status', { hubId: 'hub-1', hasKeys: true, browserSubscribed: false, vapidPub: 'pub' })
    emit('push:sub_ack', { hubId: 'hub-1' })

    await waitFor(() => {
      const card = screen.getByTestId('push-notifications-card')
      expect(card.dataset.pushState).toBe('subscribed')
    })
  })

  it('renders not-supported state in environments without PushManager', () => {
    delete global.PushManager
    render_()
    expect(screen.getByText(/does not support web push/i)).toBeInTheDocument()
  })

  it('handles hub-scoped events from the shared hub session', async () => {
    render_()
    emit('push:status', { hasKeys: true, browserSubscribed: true, vapidPub: 'pub' })

    await waitFor(() => {
      const card = screen.getByTestId('push-notifications-card')
      expect(card.dataset.pushState).toBe('subscribed')
    })
  })
})
