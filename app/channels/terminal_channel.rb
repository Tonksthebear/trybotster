# frozen_string_literal: true

# TerminalChannel relays E2E encrypted terminal data between CLI and browser.
# The server never sees plaintext - it only forwards encrypted blobs.
#
# Flow:
#   1. Browser subscribes with hub_identifier
#   2. CLI subscribes with hub_identifier
#   3. Both send/receive encrypted blobs via this channel
#   4. Server just broadcasts to all subscribers of the hub
class TerminalChannel < ApplicationCable::Channel
  def subscribed
    @hub_identifier = params[:hub_identifier]
    @device_type = params[:device_type] # 'cli' or 'browser'

    # Verify the hub belongs to this user
    hub = current_user.hubs.find_by(identifier: @hub_identifier)
    unless hub
      reject
      return
    end

    Rails.logger.info "[TerminalChannel] Subscribed: user=#{current_user.id} hub=#{@hub_identifier} type=#{@device_type}"

    # Stream for this specific hub
    stream_from terminal_stream_name
  end

  def unsubscribed
    Rails.logger.info "[TerminalChannel] Unsubscribed: hub=#{@hub_identifier} type=#{@device_type}"

    # Notify other subscribers that this device disconnected
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

  # Relay encrypted terminal data (output from CLI, input from browser)
  # Server does NOT decrypt - just forwards the blob
  def relay(data)
    # Validate required fields
    unless data["blob"].present? && data["nonce"].present?
      Rails.logger.warn "[TerminalChannel] Invalid relay data: missing blob or nonce"
      return
    end

    # Broadcast to all subscribers (CLI + browser)
    # Each will decrypt using their shared secret
    ActionCable.server.broadcast(
      terminal_stream_name,
      {
        type: "terminal",
        blob: data["blob"],
        nonce: data["nonce"],
        from: @device_type,
        timestamp: Time.current.iso8601
      }
    )
  end

  # Announce presence (browser connected, CLI ready, etc.)
  # Browser sends public_key for E2E key exchange with CLI
  def presence(data)
    ActionCable.server.broadcast(
      terminal_stream_name,
      {
        type: "presence",
        event: data["event"] || "join",
        device_type: @device_type,
        device_name: data["device_name"],
        public_key: data["public_key"], # For E2E key exchange
        timestamp: Time.current.iso8601
      }
    )
  end

  # Relay terminal resize events (need to be in sync)
  def resize(data)
    ActionCable.server.broadcast(
      terminal_stream_name,
      {
        type: "resize",
        cols: data["cols"],
        rows: data["rows"],
        from: @device_type,
        timestamp: Time.current.iso8601
      }
    )
  end

  private

  def terminal_stream_name
    "terminal_#{current_user.id}_#{@hub_identifier}"
  end
end
