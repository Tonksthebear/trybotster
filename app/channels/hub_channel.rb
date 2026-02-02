# frozen_string_literal: true

require_relative "concerns/health_status"

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
# - Browser: hub:{hub_id}:browser:{identity} (E2E messages)
# - Browser: hub:{hub_id}:health (health updates, shared across all channels)
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

    # Browsers also subscribe to hub-wide health stream
    unless @is_cli
      stream_from health_stream_name do |message|
        handle_health_broadcast(message)
        transmit(message)
      end
    end

    if @is_cli
      Rails.logger.info "[HubChannel] CLI subscribed for browser: hub=#{@hub_id} identity=#{@browser_identity[0..8]}..."
      # Notify THIS browser that CLI is on their E2E channel
      Rails.logger.info "[HubChannel] Broadcasting cli=connected to stream: #{browser_stream_name}"
      ActionCable.server.broadcast(browser_stream_name, HealthStatus.message(HealthStatus::CONNECTED))
    else
      Rails.logger.info "[HubChannel] Browser subscribed: hub=#{@hub_id} identity=#{@browser_identity[0..8]}..."
      # Notify CLI about this browser (don't transmit initial health - browser will request it)
      notify_cli_of_browser
    end
  end

  def unsubscribed
    if @is_cli
      # Notify THIS browser that CLI left their E2E channel
      ActionCable.server.broadcast(browser_stream_name, HealthStatus.message(HealthStatus::DISCONNECTED))
      Rails.logger.info "[HubChannel] CLI unsubscribed from browser: hub=#{@hub_id} identity=#{@browser_identity[0..8]}..."
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

  # Request current health status (called after subscription confirmed)
  def request_health
    return if @is_cli

    hub = current_user.hubs.find_by(id: @hub_id)
    cli_status = hub&.active? ? HealthStatus::ONLINE : HealthStatus::OFFLINE
    transmit_health(cli_status)
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

  # Hub-wide health stream (all browsers for this hub)
  def health_stream_name
    "hub:#{@hub_id}:health"
  end

  # Transmit health message directly to this connection (for initial status)
  def transmit_health(cli_status)
    transmit({ type: "health", cli: cli_status })
    Rails.logger.debug "[HubChannel] Transmit health: cli=#{cli_status}"
  end

  # Broadcast health to ALL browsers subscribed to this hub (any channel type)
  def broadcast_hub_health(cli_status)
    ActionCable.server.broadcast(health_stream_name, { type: "health", cli: cli_status })
    Rails.logger.debug "[HubChannel] Broadcast hub health: cli=#{cli_status}"
  end

  # Handle health broadcasts from HubCommandChannel (CLI online/offline)
  def handle_health_broadcast(message)
    # Message arrives as JSON string from stream_from block
    parsed = message.is_a?(String) ? JSON.parse(message) : message
    cli_status = parsed["cli"] || parsed[:cli]
    Rails.logger.info "[HubChannel] handle_health_broadcast: cli=#{cli_status}"
    return unless cli_status == HealthStatus::ONLINE

    # CLI just came online - notify it about this browser
    Rails.logger.info "[HubChannel] CLI online - calling notify_cli_of_browser"
    notify_cli_of_browser
  end

  # Create Bot::Message to tell CLI about this browser.
  def notify_cli_of_browser
    hub = current_user.hubs.find_by(id: @hub_id)
    unless hub
      Rails.logger.warn "[HubChannel] notify_cli_of_browser: hub not found for id=#{@hub_id}"
      return
    end

    Rails.logger.info "[HubChannel] notify_cli_of_browser: creating browser_connected message"
    Bot::Message.create_for_hub!(hub,
      event_type: "browser_connected",
      payload: { browser_identity: @browser_identity })
  rescue => e
    Rails.logger.warn "[HubChannel] Failed to notify CLI: #{e.message}"
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
