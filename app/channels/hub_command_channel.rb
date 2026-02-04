# frozen_string_literal: true

require_relative "concerns/health_status"

# Hub Command Channel - Reliable plaintext message delivery to CLI
#
# Delivers Bot::Messages to the CLI in real-time with ordered replay-on-reconnect.
# NOT encrypted — TLS is sufficient for server→CLI plaintext commands.
#
# Protocol:
# - CLI subscribes with hub_id and start_from (last acked sequence)
# - On subscribe: replay unacked messages from start_from sequence
# - Real-time: new messages broadcast via Bot::Message after_create_commit
# - CLI acks each message via perform("ack", { sequence: N })
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
    @hub.broadcast_update!
    stream_from "hub_command:#{@hub.id}"

    # Notify all browsers that CLI is now online
    # Each browser channel's health stream handler will re-notify CLI if needed
    broadcast_hub_health(HealthStatus::ONLINE)

    replay_messages(params[:start_from].to_i)

    Rails.logger.info "[HubCommandChannel] CLI subscribed: hub=#{@hub.id}, start_from=#{params[:start_from]}"
  end

  def unsubscribed
    return unless @hub

    @hub.update!(alive: false)
    @hub.broadcast_update!

    # Notify all browsers that CLI is now offline
    broadcast_hub_health(HealthStatus::OFFLINE)

    Rails.logger.info "[HubCommandChannel] CLI unsubscribed: hub=#{@hub.id}"
  end

  # Touch last_seen_at on any channel action
  def perform_action(data)
    @hub&.touch(:last_seen_at)
    super
  end

  def ack(data)
    sequence = data["sequence"].to_i
    msg = @hub.bot_messages.find_by(sequence: sequence)
    if msg && !msg.acknowledged?
      msg.acknowledge!
      Rails.logger.debug "[HubCommandChannel] Acked sequence #{sequence}"
    end
  end

  def heartbeat(data)
    @hub.update!(alive: true)

    if data["agents"].present?
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
    messages = @hub.bot_messages
      .where("sequence > ?", start_from)
      .where.not(status: "acknowledged")
      .order(sequence: :asc)
      .limit(100)

    messages.each do |msg|
      transmit(message_payload(msg))
    end

    Rails.logger.info "[HubCommandChannel] Replayed #{messages.size} messages from sequence #{start_from}"
  end

  def message_payload(msg)
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
