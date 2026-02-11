# frozen_string_literal: true

# Health status constants for ActionCable channels.
#
# These map to the JavaScript CliStatus enum in connection.js:
#   UNKNOWN, OFFLINE, ONLINE, NOTIFIED, CONNECTING, CONNECTED, DISCONNECTED
#
# Two conceptual levels:
#   - Hub-level: CLI connected to Rails at all (ONLINE/OFFLINE)
#   - Channel-level: CLI on this specific E2E channel (CONNECTED/DISCONNECTED)
#
module HealthStatus
  # Hub-level: CLI ↔ Rails (from hub.active?)
  ONLINE = "online"
  OFFLINE = "offline"

  # Channel-level: CLI ↔ Browser E2E channel
  CONNECTED = "connected"
  DISCONNECTED = "disconnected"

  # Build a health message hash
  def self.message(cli_status)
    { type: "health", cli: cli_status }
  end
end
