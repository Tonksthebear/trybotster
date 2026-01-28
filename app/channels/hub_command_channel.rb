# frozen_string_literal: true

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
# Auth: DeviceToken Bearer (CLI only — browsers use HubChannel for E2E relay)
class HubCommandChannel < ApplicationCable::Channel
  def subscribed
    hub = find_hub
    reject and return unless hub

    @hub = hub
    stream_from "hub_command:#{@hub.id}"

    replay_messages(params[:start_from].to_i)

    Rails.logger.info "[HubCommandChannel] CLI subscribed: hub=#{@hub.id}, start_from=#{params[:start_from]}"
  end

  def unsubscribed
    Rails.logger.info "[HubCommandChannel] CLI unsubscribed: hub=#{@hub&.id}"
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
    @hub.touch(:last_seen_at)
    @hub.update!(alive: true)

    if data["agents"].present?
      @hub.sync_agents(data["agents"])
    end

    Rails.logger.debug "[HubCommandChannel] Heartbeat from hub=#{@hub.id}"
  end

  private

  def find_hub
    current_user.hubs.find_by(identifier: params[:hub_id]) ||
      current_user.hubs.find_by(id: params[:hub_id])
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
