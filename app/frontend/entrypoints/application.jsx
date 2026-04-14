import React from 'react'
import { createRoot } from 'react-dom/client'
import AppShell from '../components/AppShell'

// Side-effect import: registers singleton event listeners for
// rename/move/delete CustomEvents dispatched by the action system.
import '../lib/modal-bridge'

// Mount the SPA shell
const container = document.getElementById('app')
if (container) {
  const root = createRoot(container)
  root.render(<AppShell />)
}
