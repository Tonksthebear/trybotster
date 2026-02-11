# frozen_string_literal: true

require_relative "concerns/health_status"

# Hub Command Channel - Reliable plaintext message delivery to CLI
#
# Delivers HubCommands to the CLI in real-time with ordered replay-on-reconnect.
# NOT encrypted — TLS is sufficient for server→CLI plaintext commands.
#
# GitHub events are handled separately by Github::EventsChannel.
#
# Protocol:
# - CLI subscribes with hub_id and start_from (last acked sequence)
# - On subscribe: replay unacked hub commands from start_from sequence
# - Real-time: new messages broadcast via after_create_commit callbacks
# - CLI acks hub commands via perform("ack", { sequence: N })
# - CLI sends heartbeat via perform("heartbeat", { agents: [...] })
#
# Stream: hub_command:{hub_id}
#
# Auth: DeviceToken Bearer (CLI only — browsers use HubSignalingChannel for E2E relay)
class HubCommandChannel < ApplicationCable::Channel
  def subscribed
    hub = find_hub
    reject and return unless hub

    @hub = hub
    @hub.update!(alive: true, last_seen_at: Time.current)
    stream_from "hub_command:#{@hub.id}"

    # Notify all browsers that CLI is now online
    broadcast_hub_health(HealthStatus::ONLINE)

    replay_messages(params[:start_from].to_i)

    Rails.logger.info "[HubCommandChannel] CLI subscribed: hub=#{@hub.id}, start_from=#{params[:start_from]}"
  end

  def unsubscribed
    return unless @hub

    @hub.update!(alive: false)

    # Notify all browsers that CLI is now offline
    broadcast_hub_health(HealthStatus::OFFLINE)

    Rails.logger.info "[HubCommandChannel] CLI unsubscribed: hub=#{@hub.id}"
  end

  def ack(data)
    sequence = data["sequence"].to_i
    msg = @hub.hub_commands.find_by(sequence: sequence)
    if msg && !msg.acknowledged?
      msg.acknowledge!
      Rails.logger.debug "[HubCommandChannel] Acked hub command sequence #{sequence}"
    end
  end

  def heartbeat(data)
    @hub.update!(alive: true, last_seen_at: Time.current)

    if data.key?("agents")
      @hub.sync_agents(data["agents"])
    end

    Rails.logger.debug "[HubCommandChannel] Heartbeat from hub=#{@hub.id}"
  end

  # Relay opaque encrypted envelope from CLI → specific browser
  def signal(data)
    browser_identity = data["browser_identity"]
    return unless browser_identity.present?

    ActionCable.server.broadcast(
      "hub:#{@hub.id}:signal:#{browser_identity}",
      { type: "signal", envelope: data["envelope"] }
    )
  end

  private

  def find_hub
    current_user.hubs.find_by(id: params[:hub_id])
  end

  # Hub-wide health stream (all browsers for this hub)
  def health_stream_name
    "hub:#{@hub.id}:health"
  end

  # Broadcast health to ALL browsers subscribed to this hub (any channel type)
  def broadcast_hub_health(cli_status)
    ActionCable.server.broadcast(health_stream_name, HealthStatus.message(cli_status))
    Rails.logger.debug "[HubCommandChannel] Broadcast hub health: cli=#{cli_status}"
  end

  def replay_messages(start_from)
    messages = @hub.hub_commands.unacked_from(start_from).limit(100)

    messages.each do |msg|
      transmit(hub_command_payload(msg))
    end

    Rails.logger.info "[HubCommandChannel] Replayed #{messages.size} hub commands from sequence #{start_from}"
  end

  def hub_command_payload(msg)
    {
      type: "message",
      sequence: msg.sequence,
      id: msg.id,
      event_type: msg.event_type,
      payload: msg.payload,
      created_at: msg.created_at.iso8601
    }
  end
end
