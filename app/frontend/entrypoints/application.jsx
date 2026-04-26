import React from 'react'
import { createRoot } from 'react-dom/client'
import AppShell from '../components/AppShell'

// Side-effect import: registers singleton event listeners for
// rename/move/delete CustomEvents dispatched by the action system.
import '../lib/modal-bridge'

// Service worker — production only. Registers `/service-worker` (served by
// Rails PWA middleware, see app/views/pwa/service-worker.js) so the
// PushNotificationsCard flow can subscribe via PushManager and receive web
// push events even when the tab is closed.
//
// Dev mode unregisters instead. Vite HMR + an active service worker
// intercepting fetches is a known footgun; the SW caches stale module
// graphs and HMR updates stop arriving. Push notifications can be tested
// in production builds.
if ('serviceWorker' in navigator) {
  if (import.meta.env.DEV) {
    navigator.serviceWorker.getRegistrations().then((regs) => {
      regs.forEach((reg) => reg.unregister())
    }).catch(() => {})
  } else {
    navigator.serviceWorker.register('/service-worker', { scope: '/' }).catch((e) => {
      console.warn('[boot] service worker registration failed:', e)
    })
  }
}

const container = document.getElementById('app')
if (container) {
  createRoot(container).render(<AppShell />)
}
