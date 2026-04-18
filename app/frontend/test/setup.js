import '@testing-library/jest-dom/vitest'

// jsdom does not implement matchMedia. Provide a minimal shim so code that
// reads viewport media queries (e.g. ui_contract's useViewport hook) does
// not crash under test.
if (typeof window !== 'undefined' && typeof window.matchMedia !== 'function') {
  window.matchMedia = (query) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  })
}
