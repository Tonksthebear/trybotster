# frozen_string_literal: true

# Preview Channel - E2E Encrypted HTTP Tunnel for Agent Server Preview
#
# This channel relays encrypted HTTP requests/responses between browser
# and CLI. Like TerminalRelayChannel, the server CANNOT decrypt messages -
# it only forwards encrypted blobs.
#
# Architecture:
# Each browser has dedicated bidirectional streams with the CLI.
# This is required because each browser has its own Signal session.
#
# Streams per (hub, agent, pty, browser):
# - Browser stream: preview:{hub}:{agent}:{pty}:{browser_identity}
# - CLI stream:     preview:{hub}:{agent}:{pty}:{browser_identity}:cli
#
# Routing:
# - Browser subscribes to browser stream, receives from CLI
# - CLI subscribes to CLI stream, receives from browser
# - Browser -> CLI: routed to CLI stream
# - CLI -> Browser: routed to browser stream
#
# Security:
# - Server never sees HTTP request/response content
# - Reuses Signal Protocol session from TerminalRelayChannel
# - Forward secrecy via Double Ratchet
class PreviewChannel < ApplicationCable::Channel
  def subscribed
    @hub_id = params[:hub_id]
    @agent_index = params[:agent_index]
    @pty_index = params[:pty_index] || 1  # Default to server PTY
    @browser_identity = params[:browser_identity]
    @is_cli = params[:cli_subscription] == true

    unless @hub_id.present? && @browser_identity.present?
      Rails.logger.warn "[Preview] Missing hub_id or browser_identity"
      reject
      return
    end

    stream_from my_stream_name

    if @is_cli
      Rails.logger.info "[Preview] CLI subscribed: hub=#{@hub_id} agent=#{@agent_index} pty=#{@pty_index} browser=#{truncate_identity(@browser_identity)}"
    else
      Rails.logger.info "[Preview] Browser subscribed: hub=#{@hub_id} agent=#{@agent_index} pty=#{@pty_index} identity=#{truncate_identity(@browser_identity)}"
      notify_cli_of_preview_request
    end
  end

  def unsubscribed
    if @is_cli
      Rails.logger.info "[Preview] CLI unsubscribed: hub=#{@hub_id} agent=#{@agent_index} pty=#{@pty_index}"
    else
      Rails.logger.info "[Preview] Browser unsubscribed: hub=#{@hub_id} agent=#{@agent_index} pty=#{@pty_index}"
    end
  end

  # Relay encrypted message to the other party
  #
  # @param data [Hash] Contains encrypted SignalEnvelope
  # @option data [String] :envelope The encrypted SignalEnvelope JSON
  def relay(data)
    envelope = data["envelope"]

    unless envelope.present?
      Rails.logger.warn "[Preview] Missing envelope in relay"
      return
    end

    # Route to the other party's stream
    target_stream = @is_cli ? browser_stream_name : cli_stream_name
    ActionCable.server.broadcast(target_stream, { envelope: envelope })

    Rails.logger.debug "[Preview] Relayed to #{@is_cli ? 'browser' : 'CLI'}"
  end

  private

  def my_stream_name
    @is_cli ? cli_stream_name : browser_stream_name
  end

  def browser_stream_name
    "preview:#{@hub_id}:#{@agent_index}:#{@pty_index}:#{@browser_identity}"
  end

  def cli_stream_name
    "preview:#{@hub_id}:#{@agent_index}:#{@pty_index}:#{@browser_identity}:cli"
  end

  def truncate_identity(identity)
    return "nil" unless identity.present?

    identity.length > 8 ? "#{identity[0..8]}..." : identity
  end

  def find_hub
    current_user.hubs.find_by(id: @hub_id)
  end

  def notify_cli_of_preview_request
    hub = find_hub
    return unless hub

    Bot::Message.create_for_hub!(hub,
      event_type: "browser_wants_preview",
      payload: { agent_index: @agent_index, pty_index: @pty_index, browser_identity: @browser_identity })
  rescue => e
    Rails.logger.warn "[Preview] Failed to notify CLI of preview request: #{e.message}"
  end
end
