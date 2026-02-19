/**
 * Maps notification kind to generic display text.
 * Falls back to kind-based text when no custom body is stored.
 */

const DISPLAY_TEXT = {
  agent_alert: "Your attention is needed",
  test: "Test notification — push is working!",
}

const DEFAULT_TEXT = "You have a new notification"

/**
 * Get display text for a notification.
 * Uses stored body text if available, otherwise falls back to kind-based text.
 * @param {string} kind
 * @param {string|null} [body] - Custom body from the push payload
 * @returns {string}
 */
export function displayText(kind, body) {
  if (body) return body
  return DISPLAY_TEXT[kind] || DEFAULT_TEXT
}

/**
 * Get the URL to navigate to for a notification.
 * Uses stored URL if available, otherwise routes to the hub page.
 * @param {string} _kind - notification kind (reserved for future routing)
 * @param {string} hubId
 * @param {string|null} [url] - Custom URL from the push payload
 * @returns {string}
 */
export function notificationUrl(_kind, hubId, url) {
  if (url) return url
  if (hubId) {
    return `/hubs/${hubId}`
  }
  return "/"
}

/**
 * Format a notification's timestamp for display.
 * @param {string} isoString
 * @returns {string}
 */
export function formatTime(isoString) {
  const date = new Date(isoString)
  const now = new Date()
  const diffMs = now - date
  const diffMin = Math.floor(diffMs / 60000)
  const diffHr = Math.floor(diffMs / 3600000)
  const diffDays = Math.floor(diffMs / 86400000)

  if (diffMin < 1) return "just now"
  if (diffMin < 60) return `${diffMin}m ago`
  if (diffHr < 24) return `${diffHr}h ago`
  if (diffDays < 7) return `${diffDays}d ago`

  return date.toLocaleDateString()
}
