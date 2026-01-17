# frozen_string_literal: true

# Hub Channel - E2E Encrypted Hub-Level Communication
#
# This channel handles hub-level commands and broadcasts between browser clients
# and the CLI. Used for:
# - Agent list updates (broadcast to all browsers)
# - Agent creation progress (broadcast)
# - Browser commands (create agent, select agent, etc.)
# - Browser handshake and connection management
#
# Terminal I/O (PTY output/input) is handled by agent-owned channels.
#
# Architecture:
# - CLI subscribes with hub_id only
# - Browsers subscribe with hub_id + browser_identity
# - Server routes messages to appropriate streams
# - All encryption/decryption happens at endpoints
#
# Streams:
# - CLI:     hub:{hub_id}:cli
# - Browser: hub:{hub_id}:browser:{identity}
#
# Security:
# - Server never sees plaintext content
# - Double Ratchet provides forward secrecy
# - Post-quantum security via Kyber/PQXDH
class HubChannel < ApplicationCable::Channel
  def subscribed
    @hub_id = params[:hub_id]
    @browser_identity = params[:browser_identity]

    unless @hub_id.present?
      Rails.logger.warn "[HubChannel] Missing hub_id"
      reject
      return
    end

    # Subscribe to appropriate stream based on client type
    stream_from my_stream_name

    if @browser_identity.present?
      Rails.logger.info "[HubChannel] Browser subscribed: hub=#{@hub_id} identity=#{@browser_identity[0..8]}..."
    else
      Rails.logger.info "[HubChannel] CLI subscribed: hub=#{@hub_id}"
    end
  end

  def unsubscribed
    if @browser_identity.present?
      Rails.logger.info "[HubChannel] Browser unsubscribed: hub=#{@hub_id}"
    else
      Rails.logger.info "[HubChannel] CLI unsubscribed: hub=#{@hub_id}"
    end
  end

  # Relay encrypted message to appropriate recipient
  #
  # @param data [Hash] Contains encrypted SignalEnvelope
  # @option data [String] :envelope The encrypted SignalEnvelope JSON
  # @option data [String] :recipient_identity Target browser (CLI->browser only)
  def relay(data)
    envelope = data["envelope"]

    unless envelope.present?
      Rails.logger.warn "[HubChannel] Missing envelope in relay"
      return
    end

    recipient_identity = data["recipient_identity"]

    if recipient_identity.present?
      # CLI -> specific browser: route to that browser's stream
      target_stream = browser_stream_name(recipient_identity)
      ActionCable.server.broadcast(target_stream, { envelope: envelope })
      Rails.logger.debug "[HubChannel] Routed to browser: #{recipient_identity[0..8]}..."
    else
      # Browser -> CLI: route to CLI stream
      ActionCable.server.broadcast(cli_stream_name, { envelope: envelope })
      Rails.logger.debug "[HubChannel] Routed to CLI"
    end
  end

  # Relay SenderKey distribution (for group messaging)
  #
  # @param data [Hash] Contains SenderKey distribution message
  # @option data [String] :distribution Base64 SenderKeyDistributionMessage
  def distribute_sender_key(data)
    distribution = data["distribution"]

    unless distribution.present?
      Rails.logger.warn "[HubChannel] Missing distribution in distribute_sender_key"
      return
    end

    # SenderKey distribution goes to all browsers (broadcast pattern)
    ActionCable.server.broadcast(cli_stream_name, { sender_key_distribution: distribution })

    Rails.logger.debug "[HubChannel] Distributed SenderKey for hub=#{@hub_id}"
  end

  private

  # Stream this client subscribes to
  def my_stream_name
    if @browser_identity.present?
      browser_stream_name(@browser_identity)
    else
      cli_stream_name
    end
  end

  def cli_stream_name
    "hub:#{@hub_id}:cli"
  end

  def browser_stream_name(identity)
    "hub:#{@hub_id}:browser:#{identity}"
  end
end
