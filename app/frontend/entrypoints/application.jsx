import React from 'react'
import { createRoot } from 'react-dom/client'
import ProofOfLife from '../components/ProofOfLife'
import App from '../components/App'
import DialogHost from '../components/DialogHost'
import PairingPage from '../components/pairing/PairingPage'
import SettingsApp from '../components/settings/SettingsApp'
import TerminalView from '../components/terminal/TerminalView'
import ShareHub from '../components/hub/ShareHub'
import ConnectionStatus from '../components/hub/ConnectionStatus'

// Side-effect import: registers singleton event listeners for
// rename/move/delete CustomEvents dispatched by the action system.
import '../lib/modal-bridge'

// Component registry — maps data-component names to React components.
// Future agents add entries here as new components are built.
const COMPONENTS = {
  ProofOfLife,
  App,
  PairingPage,
  SettingsApp,
  TerminalView,
  ShareHub,
  ConnectionStatus,
}

// Track mounted roots for cleanup on disconnect
const roots = new WeakMap()

// --- DialogHost singleton ---
// Mounted once on a dedicated DOM element so dialogs exist exactly once,
// regardless of how many App instances are on the page.
let dialogRoot = null
let dialogHubId = null

function ensureDialogHost(hubId) {
  if (dialogRoot && dialogHubId === hubId) return
  dialogHubId = hubId

  if (!dialogRoot) {
    let container = document.getElementById('dialog-host')
    if (!container) {
      container = document.createElement('div')
      container.id = 'dialog-host'
      document.body.appendChild(container)
    }
    dialogRoot = createRoot(container)
  }

  dialogRoot.render(<DialogHost hubId={hubId} />)
}

// Expose mount/unmount on window so the importmap-side Stimulus controller
// can drive React lifecycle across Turbo navigations.
window.__viteReact = {
  mount(element) {
    if (roots.has(element)) return // already mounted

    const componentName = element.dataset.component
    const Component = COMPONENTS[componentName]
    if (!Component) {
      console.warn(`[vite-react] Unknown component: ${componentName}`)
      return
    }

    const props = JSON.parse(element.dataset.props || '{}')

    // If this is an App component, ensure DialogHost is mounted with the same hubId
    if (componentName === 'App' && props.hubId) {
      ensureDialogHost(props.hubId)
    }

    const root = createRoot(element)
    root.render(<Component {...props} />)
    roots.set(element, root)
  },

  unmount(element) {
    const root = roots.get(element)
    if (root) {
      root.unmount()
      roots.delete(element)
    }
  },
}
