# frozen_string_literal: true

# Terminal Relay Channel - E2E Encrypted Browser-CLI Communication
#
# This channel acts as a pure relay for Signal Protocol encrypted messages
# between browser clients and the CLI. The server CANNOT decrypt messages -
# it only forwards encrypted blobs.
#
# Architecture:
# - CLI subscribes with hub_identifier (no browser_identity)
# - Each browser subscribes with hub_identifier + browser_identity
# - Server routes messages to appropriate streams based on recipient_identity
# - All encryption/decryption happens at endpoints
#
# Streams:
# - CLI:     terminal_relay:{hub}:cli
# - Browser: terminal_relay:{hub}:browser:{identity}
#
# Security:
# - Server never sees plaintext terminal content
# - Double Ratchet provides forward secrecy
# - Post-quantum security via Kyber/PQXDH
class TerminalRelayChannel < ApplicationCable::Channel
  def subscribed
    @hub_identifier = params[:hub_identifier]
    @browser_identity = params[:browser_identity]

    unless @hub_identifier.present?
      Rails.logger.warn "[TerminalRelay] Missing hub_identifier"
      reject
      return
    end

    # Subscribe to appropriate stream based on client type
    stream_from my_stream_name

    if @browser_identity.present?
      Rails.logger.info "[TerminalRelay] Browser subscribed: hub=#{@hub_identifier} identity=#{@browser_identity[0..8]}..."
    else
      Rails.logger.info "[TerminalRelay] CLI subscribed: hub=#{@hub_identifier}"
    end
  end

  def unsubscribed
    if @browser_identity.present?
      Rails.logger.info "[TerminalRelay] Browser unsubscribed: hub=#{@hub_identifier}"
    else
      Rails.logger.info "[TerminalRelay] CLI unsubscribed: hub=#{@hub_identifier}"
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
      Rails.logger.warn "[TerminalRelay] Missing envelope in relay"
      return
    end

    recipient_identity = data["recipient_identity"]

    if recipient_identity.present?
      # CLI -> specific browser: route to that browser's stream
      target_stream = browser_stream_name(recipient_identity)
      ActionCable.server.broadcast(target_stream, { envelope: envelope })
      Rails.logger.debug "[TerminalRelay] Routed to browser: #{recipient_identity[0..8]}..."
    else
      # Browser -> CLI: route to CLI stream
      ActionCable.server.broadcast(cli_stream_name, { envelope: envelope })
      Rails.logger.debug "[TerminalRelay] Routed to CLI"
    end
  end

  # Relay SenderKey distribution (for group messaging)
  #
  # @param data [Hash] Contains SenderKey distribution message
  # @option data [String] :distribution Base64 SenderKeyDistributionMessage
  def distribute_sender_key(data)
    distribution = data["distribution"]

    unless distribution.present?
      Rails.logger.warn "[TerminalRelay] Missing distribution in distribute_sender_key"
      return
    end

    # SenderKey distribution goes to all browsers (broadcast pattern)
    # TODO: When implementing SenderKey, need a browser broadcast stream
    ActionCable.server.broadcast(cli_stream_name, { sender_key_distribution: distribution })

    Rails.logger.debug "[TerminalRelay] Distributed SenderKey for hub=#{@hub_identifier}"
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
    "terminal_relay:#{@hub_identifier}:cli"
  end

  def browser_stream_name(identity)
    "terminal_relay:#{@hub_identifier}:browser:#{identity}"
  end
end
