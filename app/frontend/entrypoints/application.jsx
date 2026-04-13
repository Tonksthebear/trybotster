import React from 'react'
import { createRoot } from 'react-dom/client'
import ProofOfLife from '../components/ProofOfLife'

// Component registry — maps data-component names to React components.
// Future agents add entries here as new components are built.
const COMPONENTS = {
  ProofOfLife,
}

// Track mounted roots for cleanup on disconnect
const roots = new WeakMap()

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
