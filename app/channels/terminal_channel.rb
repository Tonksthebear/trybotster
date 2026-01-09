# frozen_string_literal: true

# TerminalChannel relays Olm E2E encrypted terminal data between CLI and browser.
# The server never sees plaintext - it only forwards encrypted envelopes.
#
# Flow:
#   1. Browser subscribes with hub_identifier
#   2. CLI subscribes with hub_identifier
#   3. Browser sends PreKey message via presence to establish Olm session
#   4. Both send/receive Olm-encrypted envelopes via relay
class TerminalChannel < ApplicationCable::Channel
  def subscribed
    @hub_identifier = params[:hub_identifier]
    @device_type = params[:device_type] # 'cli' or 'browser'

    hub = current_user.hubs.find_by(identifier: @hub_identifier)
    unless hub
      reject
      return
    end

    Rails.logger.info "[TerminalChannel] Subscribed: user=#{current_user.id} hub=#{@hub_identifier} type=#{@device_type}"
    stream_from terminal_stream_name
  end

  def unsubscribed
    Rails.logger.info "[TerminalChannel] Unsubscribed: hub=#{@hub_identifier} type=#{@device_type}"

    ActionCable.server.broadcast(
      terminal_stream_name,
      {
        type: "presence",
        event: "leave",
        device_type: @device_type,
        timestamp: Time.current.iso8601
      }
    )
  end

  # Relay Olm-encrypted terminal data
  def relay(data)
    unless data["version"].present? && data["ciphertext"].present?
      Rails.logger.warn "[TerminalChannel] Invalid relay: missing Olm envelope fields"
      return
    end

    ActionCable.server.broadcast(
      terminal_stream_name,
      {
        type: "terminal",
        from: @device_type,
        version: data["version"],
        message_type: data["message_type"],
        ciphertext: data["ciphertext"],
        sender_key: data["sender_key"],
        timestamp: Time.current.iso8601
      }
    )
  end

  # Announce presence - browser sends prekey_message for Olm session establishment
  def presence(data)
    ActionCable.server.broadcast(
      terminal_stream_name,
      {
        type: "presence",
        event: data["event"] || "join",
        device_type: @device_type,
        device_name: data["device_name"],
        prekey_message: data["prekey_message"],
        timestamp: Time.current.iso8601
      }
    )
  end

  private

  def terminal_stream_name
    "terminal_#{current_user.id}_#{@hub_identifier}"
  end
end
