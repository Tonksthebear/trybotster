# frozen_string_literal: true

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
# - Browser stream: terminal_relay:{hub}:{agent}:{pty}:{browser_identity}
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

    if @is_cli
      Rails.logger.info "[TerminalRelay] CLI subscribed: hub=#{@hub_id} agent=#{@agent_index} pty=#{@pty_index} browser=#{@browser_identity[0..8]}..."
    else
      Rails.logger.info "[TerminalRelay] Browser subscribed: hub=#{@hub_id} agent=#{@agent_index} pty=#{@pty_index} identity=#{@browser_identity[0..8]}..."
      notify_cli_of_terminal_connection
    end
  end

  def unsubscribed
    if @is_cli
      Rails.logger.info "[TerminalRelay] CLI unsubscribed: hub=#{@hub_id} agent=#{@agent_index} pty=#{@pty_index}"
    else
      Rails.logger.info "[TerminalRelay] Browser unsubscribed: hub=#{@hub_id} agent=#{@agent_index} pty=#{@pty_index}"
      notify_cli_of_terminal_disconnection
    end
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

    Rails.logger.debug "[TerminalRelay] Relayed to #{@is_cli ? 'browser' : 'CLI'}"
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

  def find_hub
    current_user.hubs.find_by(id: @hub_id)
  end

  def notify_cli_of_terminal_connection
    hub = find_hub
    return unless hub

    Bot::Message.create_for_hub!(hub,
      event_type: "terminal_connected",
      payload: { agent_index: @agent_index, pty_index: @pty_index,
                 browser_identity: @browser_identity })
  rescue => e
    Rails.logger.warn "[TerminalRelay] Failed to notify CLI of terminal connection: #{e.message}"
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
