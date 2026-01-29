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
# - Per-browser bidirectional streams: both browser AND CLI provide browser_identity
# - Browser subscribes first, listens on `hub:{id}:browser:{identity}`
# - CLI receives browser_connected event, creates BrowserClient
# - BrowserClient subscribes with same identity, listens on `hub:{id}:browser:{identity}:cli`
# - Server routes messages between the paired streams
#
# Streams:
# - Browser: hub:{hub_id}:browser:{identity}
# - CLI:     hub:{hub_id}:browser:{identity}:cli
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

    # Both browser AND CLI must provide browser_identity for per-browser streams
    unless @browser_identity.present?
      Rails.logger.warn "[HubChannel] Missing browser_identity - rejecting subscription"
      reject
      return
    end

    # Determine if this is a CLI subscription (has cli_subscription param)
    @is_cli = params[:cli_subscription].present?

    # Subscribe to appropriate stream based on client type
    stream_from my_stream_name

    if @is_cli
      Rails.logger.info "[HubChannel] CLI subscribed for browser: hub=#{@hub_id} identity=#{@browser_identity[0..8]}..."
    else
      Rails.logger.info "[HubChannel] Browser subscribed: hub=#{@hub_id} identity=#{@browser_identity[0..8]}..."
      notify_cli_of_browser_connection
    end
  end

  def unsubscribed
    if @is_cli
      Rails.logger.info "[HubChannel] CLI unsubscribed for browser: hub=#{@hub_id}"
    else
      Rails.logger.info "[HubChannel] Browser unsubscribed: hub=#{@hub_id}"
      notify_cli_of_browser_disconnection
    end
  end

  # Relay encrypted message to the paired stream
  #
  # Per-browser bidirectional streams mean each subscription knows its browser_identity.
  # Browser sends -> routed to CLI stream for that browser
  # CLI sends -> routed to browser stream for that browser
  #
  # @param data [Hash] Contains encrypted SignalEnvelope
  # @option data [String] :envelope The encrypted SignalEnvelope JSON
  def relay(data)
    envelope = data["envelope"]

    unless envelope.present?
      Rails.logger.warn "[HubChannel] Missing envelope in relay"
      return
    end

    if @is_cli
      # CLI -> browser: route to the browser's stream
      target_stream = browser_stream_name
      ActionCable.server.broadcast(target_stream, { envelope: envelope })
      Rails.logger.debug "[HubChannel] CLI->Browser: #{@browser_identity[0..8]}..."
    else
      # Browser -> CLI: route to CLI stream for this browser
      target_stream = cli_stream_name
      ActionCable.server.broadcast(target_stream, { envelope: envelope })
      Rails.logger.debug "[HubChannel] Browser->CLI: #{@browser_identity[0..8]}..."
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
    if @is_cli
      cli_stream_name
    else
      browser_stream_name
    end
  end

  # CLI stream for this browser identity
  def cli_stream_name
    "hub:#{@hub_id}:browser:#{@browser_identity}:cli"
  end

  # Browser stream for this browser identity
  def browser_stream_name
    "hub:#{@hub_id}:browser:#{@browser_identity}"
  end

  def notify_cli_of_browser_connection
    hub = current_user.hubs.find_by(id: @hub_id)
    return unless hub

    Bot::Message.create_for_hub!(hub,
      event_type: "browser_connected",
      payload: { browser_identity: @browser_identity })
  rescue => e
    Rails.logger.warn "[HubChannel] Failed to notify CLI of browser connection: #{e.message}"
  end

  def notify_cli_of_browser_disconnection
    hub = current_user.hubs.find_by(id: @hub_id)
    return unless hub

    Bot::Message.create_for_hub!(hub,
      event_type: "browser_disconnected",
      payload: { browser_identity: @browser_identity })
  rescue => e
    Rails.logger.warn "[HubChannel] Failed to notify CLI of browser disconnection: #{e.message}"
  end
end
