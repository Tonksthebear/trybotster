import React from 'react'
import { createRoot } from 'react-dom/client'
import AppShell from '../components/AppShell'

// Side-effect import: registers singleton event listeners for
// rename/move/delete CustomEvents dispatched by the action system.
import '../lib/modal-bridge'

// Dev-only: unregister any stale service workers left over from the
// pre-SPA Stimulus era. The current app registers no SW, so any active
// registration is orphaned and can intercept fetches in ways that look
// like "I need to clear my cache again". Cheap to run; runs once per
// page load and is a no-op if nothing is registered.
if (import.meta.env.DEV && 'serviceWorker' in navigator) {
  navigator.serviceWorker.getRegistrations().then((regs) => {
    regs.forEach((reg) => reg.unregister())
  }).catch(() => {})
}

const container = document.getElementById('app')
if (container) {
  createRoot(container).render(<AppShell />)
}
