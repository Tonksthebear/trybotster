# frozen_string_literal: true

# Terminal Relay Channel - E2E Encrypted Browser-CLI Communication
#
# This channel acts as a pure relay for Signal Protocol encrypted messages
# between browser clients and the CLI. The server CANNOT decrypt messages -
# it only forwards encrypted blobs.
#
# Architecture:
# - Browser subscribes with hub_identifier
# - CLI subscribes with hub_identifier (via separate WebSocket)
# - Messages are relayed between them unchanged
# - All encryption/decryption happens at endpoints
#
# Security:
# - Server never sees plaintext terminal content
# - Double Ratchet provides forward secrecy
# - Post-quantum security via Kyber/PQXDH
class TerminalRelayChannel < ApplicationCable::Channel
  def subscribed
    @hub_identifier = params[:hub_identifier]

    unless @hub_identifier.present?
      Rails.logger.warn "[TerminalRelay] Missing hub_identifier"
      reject
      return
    end

    # Stream for this hub - both CLI and browsers subscribe to same stream
    stream_from stream_name

    Rails.logger.info "[TerminalRelay] Subscribed: hub=#{@hub_identifier}"
  end

  def unsubscribed
    Rails.logger.info "[TerminalRelay] Unsubscribed: hub=#{@hub_identifier}"
  end

  # Relay encrypted message from browser to CLI (or CLI to browser)
  #
  # @param data [Hash] Contains encrypted SignalEnvelope
  # @option data [String] :envelope The encrypted SignalEnvelope JSON
  def relay(data)
    envelope = data["envelope"]

    unless envelope.present?
      Rails.logger.warn "[TerminalRelay] Missing envelope in relay"
      return
    end

    # Broadcast to all subscribers (CLI + other browsers)
    # Each endpoint decrypts what's meant for them
    ActionCable.server.broadcast(stream_name, { envelope: envelope })

    Rails.logger.debug "[TerminalRelay] Relayed message for hub=#{@hub_identifier}"
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

    ActionCable.server.broadcast(stream_name, { sender_key_distribution: distribution })

    Rails.logger.debug "[TerminalRelay] Distributed SenderKey for hub=#{@hub_identifier}"
  end

  private

  def stream_name
    "terminal_relay:#{@hub_identifier}"
  end
end
