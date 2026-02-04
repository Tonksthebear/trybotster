# frozen_string_literal: true

require_relative "concerns/health_status"

# Hub Signaling Channel - Browser-facing WebRTC signal relay
#
# Thin relay for encrypted WebRTC signaling between browser and CLI.
# Rails is a dumb pipe — it never inspects envelope contents.
# Only the endpoints (browser/CLI) can decrypt after Signal Protocol handshake.
#
# Streams:
# - hub:{hub_id}:signal:{browser_identity} — scoped signals from CLI (opaque envelopes)
# - hub:{hub_id}:health — shared CLI online/offline status
#
# Single action:
# - signal(envelope) — relays opaque envelope to CLI via hub_command broadcast
#
# Auth: Browser session (Warden) — NOT for CLI use
class HubSignalingChannel < ApplicationCable::Channel
  def subscribed
    @hub = current_user.hubs.find_by(id: params[:hub_id])
    @browser_identity = params[:browser_identity]

    unless @hub && @browser_identity.present?
      reject
      return
    end

    # Scoped signal stream — only this browser tab receives signals
    stream_from "hub:#{@hub.id}:signal:#{@browser_identity}"

    # Shared health stream — all browsers see CLI status
    stream_from "hub:#{@hub.id}:health"

    # Immediately tell browser current CLI status
    transmit(HealthStatus.message(@hub.alive? ? HealthStatus::ONLINE : HealthStatus::OFFLINE))

    Rails.logger.info "[HubSignalingChannel] Browser subscribed: hub=#{@hub.id}, identity=#{@browser_identity}"
  end

  def unsubscribed
    Rails.logger.info "[HubSignalingChannel] Browser unsubscribed: hub=#{@hub&.id}"
  end

  # Relay opaque encrypted envelope from browser → CLI
  def signal(data)
    return unless @hub

    ActionCable.server.broadcast(
      "hub_command:#{@hub.id}",
      { type: "signal", browser_identity: @browser_identity, envelope: data["envelope"] }
    )
  end
end
