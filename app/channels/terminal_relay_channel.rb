# frozen_string_literal: true

require_relative "concerns/health_status"

# Terminal Relay Channel - E2E Encrypted Browser-CLI Communication
#
# This channel acts as a pure relay for Signal Protocol encrypted messages
# between browser clients and the CLI. The server CANNOT decrypt messages -
# it only forwards encrypted blobs.
#
# Architecture:
# Each browser has dedicated bidirectional streams with the CLI (like TUI).
# This is required because each browser has its own Signal session.
#
# Streams per (hub, agent, pty, browser):
# - Browser stream: terminal_relay:{hub}:{agent}:{pty}:{browser_identity} (E2E messages)
# - Browser stream: hub:{hub_id}:health (health updates, shared across all channels)
# - CLI stream:     terminal_relay:{hub}:{agent}:{pty}:{browser_identity}:cli
#
# Routing:
# - Browser subscribes to browser stream, receives from CLI
# - CLI subscribes to CLI stream, receives from browser
# - Browser -> CLI: routed to CLI stream
# - CLI -> Browser: routed to browser stream
#
# PTY indices:
# - 0: CLI PTY (Claude agent terminal)
# - 1: Server PTY (development server)
#
# Security:
# - Server never sees plaintext terminal content
# - E2E encryption via Signal Protocol (per-browser sessions)
class TerminalRelayChannel < ApplicationCable::Channel
  def subscribed
    @hub_id = params[:hub_id]
    @agent_index = params[:agent_index] || 0
    @pty_index = params[:pty_index] || 0
    @browser_identity = params[:browser_identity]
    @is_cli = params[:cli_subscription] == true

    unless @hub_id.present? && @browser_identity.present?
      Rails.logger.warn "[TerminalRelay] Missing hub_id or browser_identity"
      reject
      return
    end

    stream_from my_stream_name

    # Browsers also subscribe to hub-wide health stream
    unless @is_cli
      stream_from health_stream_name do |message|
        handle_health_broadcast(message)
        transmit(message)
      end
    end

    if @is_cli
      Rails.logger.info "[TerminalRelay] CLI subscribed: hub=#{@hub_id} agent=#{@agent_index} pty=#{@pty_index} browser=#{@browser_identity[0..8]}..."
      # Notify THIS browser that CLI is on their E2E channel
      ActionCable.server.broadcast(browser_stream_name, HealthStatus.message(HealthStatus::CONNECTED))
    else
      Rails.logger.info "[TerminalRelay] Browser subscribed: hub=#{@hub_id} agent=#{@agent_index} pty=#{@pty_index} identity=#{@browser_identity[0..8]}..."
      # Notify CLI about this terminal (don't transmit initial health - browser will request it)
      notify_cli_of_terminal
    end
  end

  def unsubscribed
    if @is_cli
      # Notify THIS browser that CLI left their E2E channel
      ActionCable.server.broadcast(browser_stream_name, HealthStatus.message(HealthStatus::DISCONNECTED))
      Rails.logger.info "[TerminalRelay] CLI unsubscribed from browser: hub=#{@hub_id} identity=#{@browser_identity[0..8]}..."
    else
      Rails.logger.info "[TerminalRelay] Browser unsubscribed: hub=#{@hub_id} agent=#{@agent_index} pty=#{@pty_index}"
      notify_cli_of_terminal_disconnection
    end
  end

  # Request current health status (called after subscription confirmed)
  def request_health
    return if @is_cli

    hub = find_hub
    cli_status = hub&.active? ? HealthStatus::ONLINE : HealthStatus::OFFLINE
    transmit_health(cli_status)
  end

  # Relay encrypted message to the other party
  #
  # @param data [Hash] Contains encrypted SignalEnvelope
  # @option data [String] :envelope The encrypted SignalEnvelope JSON
  def relay(data)
    envelope = data["envelope"]

    unless envelope.present?
      Rails.logger.warn "[TerminalRelay] Missing envelope in relay"
      return
    end

    # Route to the other party's stream
    target_stream = @is_cli ? browser_stream_name : cli_stream_name
    ActionCable.server.broadcast(target_stream, { envelope: envelope })

    Rails.logger.info "[TerminalRelay] Relayed to #{@is_cli ? 'browser' : 'CLI'}: envelope_size=#{envelope.to_s.length}"
  end

  private

  def my_stream_name
    @is_cli ? cli_stream_name : browser_stream_name
  end

  def browser_stream_name
    "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:#{@browser_identity}"
  end

  def cli_stream_name
    "terminal_relay:#{@hub_id}:#{@agent_index}:#{@pty_index}:#{@browser_identity}:cli"
  end

  # Hub-wide health stream (all browsers for this hub)
  def health_stream_name
    "hub:#{@hub_id}:health"
  end

  def find_hub
    current_user.hubs.find_by(id: @hub_id)
  end

  # Transmit health message directly to this connection (for initial status)
  def transmit_health(cli_status)
    transmit({ type: "health", cli: cli_status })
    Rails.logger.debug "[TerminalRelay] Transmit health: cli=#{cli_status}"
  end

  # Handle health broadcasts from HubCommandChannel (CLI online/offline)
  def handle_health_broadcast(message)
    # Message arrives as JSON string from stream_from block
    parsed = message.is_a?(String) ? JSON.parse(message) : message
    cli_status = parsed["cli"] || parsed[:cli]
    return unless cli_status == HealthStatus::ONLINE

    # CLI just came online - notify it about this terminal
    notify_cli_of_terminal
  end

  # Create Bot::Message to tell CLI about this terminal.
  def notify_cli_of_terminal
    hub = find_hub
    return unless hub

    Rails.logger.info "[TerminalRelay] Creating terminal_connected message for agent=#{@agent_index} pty=#{@pty_index}"
    Bot::Message.create_for_hub!(hub,
      event_type: "terminal_connected",
      payload: { agent_index: @agent_index, pty_index: @pty_index,
                 browser_identity: @browser_identity })
  rescue => e
    Rails.logger.warn "[TerminalRelay] Failed to notify CLI: #{e.message}"
  end

  def notify_cli_of_terminal_disconnection
    hub = find_hub
    return unless hub

    Bot::Message.create_for_hub!(hub,
      event_type: "terminal_disconnected",
      payload: { agent_index: @agent_index, pty_index: @pty_index,
                 browser_identity: @browser_identity })
  rescue => e
    Rails.logger.warn "[TerminalRelay] Failed to notify CLI of terminal disconnection: #{e.message}"
  end
end
