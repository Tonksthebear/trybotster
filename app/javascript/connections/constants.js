/**
 * Connection constants — shared by HubRoute, HealthTracker, and HubEventHandlers.
 *
 * Extracted to break circular dependency: HubRoute imports managers/constants
 * and manager internals import these constants. Keeping shared enums in this
 * leaf module avoids TDZ (temporal dead zone) issues during evaluation.
 */

// Connection state (combines browser subscription + CLI handshake status)
export const ConnectionState = {
  DISCONNECTED: "disconnected",
  LOADING: "loading",
  CONNECTING: "connecting",
  CONNECTED: "connected",
  CLI_DISCONNECTED: "cli_disconnected",
  ERROR: "error",
}

// Browser connection status (from this tab's perspective)
export const BrowserStatus = {
  DISCONNECTED: "disconnected",
  CONNECTING: "connecting",
  SUBSCRIBING: "subscribing",
  SUBSCRIBED: "subscribed",
  ERROR: "error",
}

// CLI connection status (reported by Rails via health messages)
export const CliStatus = {
  UNKNOWN: "unknown",           // Initial state, waiting for health message
  OFFLINE: "offline",           // CLI not connected to Rails at all
  ONLINE: "online",             // CLI connected to Rails, but not yet on this E2E channel
  NOTIFIED: "notified",         // HubCommand sent to tell CLI about browser
  CONNECTING: "connecting",     // CLI connecting to this channel
  CONNECTED: "connected",       // CLI connected to this channel, ready for handshake
  DISCONNECTED: "disconnected", // CLI was connected but disconnected
}

// Connection mode (P2P vs relayed through TURN)
export const ConnectionMode = {
  UNKNOWN: "unknown",
  DIRECT: "direct",    // P2P connection (host, srflx, prflx)
  RELAYED: "relayed",  // Relayed through TURN server
}
