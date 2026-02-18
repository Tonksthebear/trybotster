/**
 * Connection constants — shared by Connection, HealthTracker, and HubEventHandlers.
 *
 * Extracted to break circular dependency: connection.js imports the managers,
 * managers import these constants. A direct import back to connection.js would
 * hit TDZ (temporal dead zone) because connection.js hasn't finished evaluating.
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
