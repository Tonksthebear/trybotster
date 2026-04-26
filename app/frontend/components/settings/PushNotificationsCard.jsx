// Web push notifications card — React implementation of the original
// push subscription flow.
//
// Wire protocol:
//   browser → CLI: { type: "push_status_req", browser_id }
//   CLI → browser: { type: "push_status", hub_id, has_keys, browser_subscribed, vapid_pub }
//   browser → CLI: { type: "vapid_generate" }                             (no keys yet)
//   browser → CLI: { type: "vapid_pub_req" }                              (already has keys)
//   CLI → browser: { type: "push_vapid_key", hub_id, key }
//   browser → CLI: { type: "push_sub", browser_id, endpoint, p256dh, auth }
//   CLI → browser: { type: "push_sub_ack" }
//   browser → CLI: { type: "push_test" }
//   CLI → browser: { type: "push_test_ack", sent }
//   browser → CLI: { type: "push_disable" }
//   CLI → browser: { type: "push_disable_ack" }
//
// 4 visible states, mapped from CLI status:
//   - unpaired: hub transport reports unpaired
//   - no_keys: CLI has no VAPID keys yet (offer Enable → vapid_generate)
//   - has_keys_browser_unsubscribed: CLI has keys but THIS browser not subscribed
//     (offer "Enable on this browser" → vapid_pub_req)
//   - subscribed: this browser is subscribed (offer Test, Disable)
//
// Copy-keys-from-source-hub multi-device flow is intentionally deferred —
// see commit message for the omission notice.

import React, { useEffect, useState, useCallback, useRef } from 'react'
import { Subheading } from '../catalyst/heading'
import { Text } from '../catalyst/text'
import { Button } from '../catalyst/button'
import { Badge } from '../catalyst/badge'
import { IconGlyph } from '../../ui_contract/icons'
import { waitForHub } from '../../lib/hub-bridge'

const VAPID_LOCAL_KEY = 'botster_vapid_key'
const BROWSER_ID_KEY = 'botster_browser_id'

const STATE_UNPAIRED = 'unpaired'
const STATE_NO_KEYS = 'no_keys'
const STATE_HAS_KEYS_UNSUBSCRIBED = 'has_keys_browser_unsubscribed'
const STATE_SUBSCRIBED = 'subscribed'
const STATE_LOADING = 'loading'
const STATE_NOT_SUPPORTED = 'not_supported'
const STATE_DENIED = 'denied'

function getBrowserId() {
  let id = localStorage.getItem(BROWSER_ID_KEY)
  if (!id) {
    id = crypto.randomUUID()
    localStorage.setItem(BROWSER_ID_KEY, id)
  }
  return id
}

function urlBase64ToUint8Array(base64String) {
  const padding = '='.repeat((4 - (base64String.length % 4)) % 4)
  const base64 = (base64String + padding).replace(/-/g, '+').replace(/_/g, '/')
  const rawData = atob(base64)
  const out = new Uint8Array(rawData.length)
  for (let i = 0; i < rawData.length; i++) out[i] = rawData.charCodeAt(i)
  return out
}

function pushSupported() {
  return typeof navigator !== 'undefined'
    && 'serviceWorker' in navigator
    && typeof window !== 'undefined'
    && 'PushManager' in window
    && typeof Notification !== 'undefined'
}

async function subscribeBrowser(vapidPub) {
  const registration = await navigator.serviceWorker.register('/service-worker', { scope: '/' })
  await navigator.serviceWorker.ready

  // Force-refresh: an existing subscription with a different VAPID key would
  // make pushManager.subscribe() reject. Drop any stale registration first.
  const existing = await registration.pushManager.getSubscription()
  if (existing) await existing.unsubscribe()

  const subscription = await registration.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: urlBase64ToUint8Array(vapidPub),
  })
  localStorage.setItem(VAPID_LOCAL_KEY, vapidPub)
  return subscription.toJSON()
}

async function unsubscribeBrowser() {
  try {
    const registration = await navigator.serviceWorker.getRegistration('/')
    if (!registration) return
    const subscription = await registration.pushManager.getSubscription()
    if (subscription) await subscription.unsubscribe()
  } finally {
    localStorage.removeItem(VAPID_LOCAL_KEY)
  }
}

export default function PushNotificationsCard({ hubId }) {
  const [state, setState] = useState(STATE_LOADING)
  const [statusDetail, setStatusDetail] = useState('')
  const [errorMessage, setErrorMessage] = useState('')
  const browserIdRef = useRef(null)
  const browserId = (browserIdRef.current ??= getBrowserId())

  // Send through the shared hub subscription so client.lua command dispatch
  // sees push controls before Rust performs VAPID/subscription mechanics.
  const sendCli = useCallback(async (message) => {
    try {
      const hub = await waitForHub(hubId)
      return await hub?.send(message.type, message)
    } catch (e) {
      console.warn('[PushNotifications] hub command failed:', e)
      return false
    }
  }, [hubId])

  useEffect(() => {
    if (!pushSupported()) {
      setState(STATE_NOT_SUPPORTED)
      return
    }
    if (!hubId) return

    let cancelled = false
    const cleanups = []

    waitForHub(hubId).then((hub) => {
      if (cancelled || !hub) return

      cleanups.push(
        hub.on('push:status', ({ hasKeys, browserSubscribed, vapidPub }) => {
          // Detect VAPID rotation: CLI thinks we're subscribed but our stored
          // key disagrees. Resubscribe transparently.
          if (hasKeys && browserSubscribed && vapidPub) {
            const stored = localStorage.getItem(VAPID_LOCAL_KEY)
            if (stored && stored !== vapidPub) {
              handleVapidKey(vapidPub).catch(() => {})
              return
            }
          }
          if (!hasKeys) {
            setState(STATE_NO_KEYS)
          } else if (!browserSubscribed) {
            setState(STATE_HAS_KEYS_UNSUBSCRIBED)
          } else {
            setState(STATE_SUBSCRIBED)
          }
          setStatusDetail('')
        }),
      )

      cleanups.push(
        hub.on('push:vapid_key', ({ key }) => {
          handleVapidKey(key).catch((e) => {
            console.error('[PushNotifications] subscribe failed:', e)
            setErrorMessage(e?.message || 'Subscribe failed')
            setState(STATE_HAS_KEYS_UNSUBSCRIBED)
          })
        }),
      )

      cleanups.push(
        hub.on('push:sub_ack', () => {
          setState(STATE_SUBSCRIBED)
          setStatusDetail('')
          setErrorMessage('')
        }),
      )

      cleanups.push(
        hub.on('push:test_ack', ({ sent }) => {
          setStatusDetail(sent > 0 ? 'Test notification sent' : 'No active subscriptions')
        }),
      )

      cleanups.push(
        hub.on('push:disable_ack', async () => {
          await unsubscribeBrowser().catch(() => {})
          setState(STATE_NO_KEYS)
          setStatusDetail('')
          setErrorMessage('')
        }),
      )

      sendCli({ type: 'push_status_req', browser_id: browserId })
    })

    async function handleVapidKey(key) {
      const sub = await subscribeBrowser(key)
      await sendCli({
        type: 'push_sub',
        browser_id: browserId,
        endpoint: sub.endpoint,
        p256dh: sub.keys.p256dh,
        auth: sub.keys.auth,
      })
    }

    return () => {
      cancelled = true
      cleanups.forEach((fn) => fn())
    }
  }, [hubId, browserId, sendCli])

  async function handleEnable() {
    setErrorMessage('')
    const permission = await Notification.requestPermission()
    if (permission !== 'granted') {
      setState(STATE_DENIED)
      return
    }
    setStatusDetail('Generating VAPID keys…')
    await sendCli({ type: 'vapid_generate' })
  }

  async function handleEnableOnThisBrowser() {
    setErrorMessage('')
    const permission = await Notification.requestPermission()
    if (permission !== 'granted') {
      setState(STATE_DENIED)
      return
    }
    setStatusDetail('Subscribing this browser…')
    await sendCli({ type: 'vapid_pub_req' })
  }

  async function handleTest() {
    setErrorMessage('')
    setStatusDetail('Sending test notification…')
    await sendCli({ type: 'push_test' })
  }

  async function handleDisable() {
    setErrorMessage('')
    setStatusDetail('Disabling…')
    await sendCli({ type: 'push_disable' })
  }

  return (
    <section
      data-testid="push-notifications-card"
      data-push-state={state}
      className="border border-zinc-800 rounded-lg"
    >
      <header className="px-4 py-3 border-b border-zinc-800 flex items-center justify-between gap-2">
        <Subheading className="!text-sm !text-zinc-400">Push Notifications</Subheading>
        <PushStateBadge state={state} />
      </header>
      <div className="px-4 py-4 space-y-3">
        <PushStateBody
          state={state}
          statusDetail={statusDetail}
          errorMessage={errorMessage}
          onEnable={handleEnable}
          onEnableOnThisBrowser={handleEnableOnThisBrowser}
          onTest={handleTest}
          onDisable={handleDisable}
        />
      </div>
    </section>
  )
}

function PushStateBadge({ state }) {
  if (state === STATE_SUBSCRIBED) return <Badge color="emerald">Active</Badge>
  if (state === STATE_NOT_SUPPORTED) return <Badge color="zinc">Not supported</Badge>
  if (state === STATE_DENIED) return <Badge color="red">Permission denied</Badge>
  if (state === STATE_UNPAIRED) return <Badge color="amber">Unpaired</Badge>
  return <Badge color="zinc">Off</Badge>
}

function PushStateBody({
  state,
  statusDetail,
  errorMessage,
  onEnable,
  onEnableOnThisBrowser,
  onTest,
  onDisable,
}) {
  if (state === STATE_NOT_SUPPORTED) {
    return (
      <Text className="text-xs">
        This browser does not support web push. Use a recent Chrome, Firefox,
        or Safari (18.4+).
      </Text>
    )
  }

  if (state === STATE_DENIED) {
    return (
      <Text className="text-xs">
        Notification permission was denied. Re-enable it in your browser
        settings, then reload this page.
      </Text>
    )
  }

  if (state === STATE_UNPAIRED) {
    return <Text className="text-xs">Pair this hub before enabling push.</Text>
  }

  if (state === STATE_LOADING) {
    return <Text className="text-xs">Checking push status…</Text>
  }

  return (
    <>
      <Text className="text-xs">
        Receive notifications when an agent needs your attention, even when
        this tab is closed.
      </Text>
      {state === STATE_NO_KEYS && (
        <div className="flex items-center gap-2">
          <Button color="emerald" onClick={onEnable}>Enable push</Button>
        </div>
      )}
      {state === STATE_HAS_KEYS_UNSUBSCRIBED && (
        <div className="flex items-center gap-2">
          <Button color="emerald" onClick={onEnableOnThisBrowser}>
            Enable on this browser
          </Button>
        </div>
      )}
      {state === STATE_SUBSCRIBED && (
        <div className="flex items-center gap-2">
          <Button outline onClick={onTest}>Send test notification</Button>
          <Button plain onClick={onDisable}>Disable</Button>
        </div>
      )}
      {statusDetail && (
        <Text className="text-xs text-zinc-500">{statusDetail}</Text>
      )}
      {errorMessage && (
        <div className="flex items-start gap-2 text-amber-400">
          <IconGlyph name="exclamation-triangle" className="mt-0.5 size-4 shrink-0" />
          <Text className="text-xs text-amber-300">{errorMessage}</Text>
        </div>
      )}
    </>
  )
}
