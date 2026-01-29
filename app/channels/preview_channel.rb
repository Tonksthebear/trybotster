# frozen_string_literal: true

# Preview Channel - E2E Encrypted HTTP Tunnel for Agent Server Preview
#
# This channel relays encrypted HTTP requests/responses between browser
# and agent server PTY. Like TerminalRelayChannel, the server CANNOT
# decrypt messages - it only forwards encrypted blobs.
#
# Architecture:
# - Agent subscribes with hub_id + agent_index (no browser_identity)
# - Each browser subscribes with hub_id + agent_index + browser_identity
# - Server routes messages to appropriate streams
# - All encryption/decryption happens at endpoints
#
# Streams:
# - Agent:   preview:{hub_id}:{agent_index}:agent
# - Browser: preview:{hub_id}:{agent_index}:browser:{identity}
#
# Security:
# - Server never sees HTTP request/response content
# - Reuses Signal Protocol session from TerminalRelayChannel
# - Forward secrecy via Double Ratchet
class PreviewChannel < ApplicationCable::Channel
  def subscribed
    @hub_id = params[:hub_id]
    @agent_index = params[:agent_index]
    @browser_identity = params[:browser_identity]

    unless @hub_id.present? && @agent_index.present?
      Rails.logger.warn "[Preview] Missing hub_id or agent_index"
      reject
      return
    end

    # Subscribe to appropriate stream based on client type
    stream_from my_stream_name

    if @browser_identity.present?
      Rails.logger.info "[Preview] Browser subscribed: hub=#{@hub_id} agent=#{@agent_index} identity=#{truncate_identity(@browser_identity)}"
      notify_cli_of_preview_request
    else
      Rails.logger.info "[Preview] Agent subscribed: hub=#{@hub_id} agent=#{@agent_index}"
    end
  end

  def unsubscribed
    if @browser_identity.present?
      Rails.logger.info "[Preview] Browser unsubscribed: hub=#{@hub_id} agent=#{@agent_index}"
    else
      Rails.logger.info "[Preview] Agent unsubscribed: hub=#{@hub_id} agent=#{@agent_index}"
    end
  end

  # Relay encrypted message to appropriate recipient
  #
  # @param data [Hash] Contains encrypted SignalEnvelope
  # @option data [String] :envelope The encrypted SignalEnvelope JSON
  # @option data [String] :recipient_identity Target browser (Agent->browser only)
  def relay(data)
    envelope = data["envelope"]

    unless envelope.present?
      Rails.logger.warn "[Preview] Missing envelope in relay"
      return
    end

    recipient_identity = data["recipient_identity"]

    if @browser_identity.present?
      # Browser -> Agent: route to agent's stream
      ActionCable.server.broadcast(agent_stream_name, { envelope: envelope })
      Rails.logger.debug "[Preview] Routed to agent: hub=#{@hub_id} agent=#{@agent_index}"
    elsif recipient_identity.present?
      # Agent -> specific browser: route to that browser's stream
      target_stream = browser_stream_name(recipient_identity)
      ActionCable.server.broadcast(target_stream, { envelope: envelope })
      Rails.logger.debug "[Preview] Routed to browser: #{truncate_identity(recipient_identity)}"
    else
      Rails.logger.warn "[Preview] Agent relay missing recipient_identity"
    end
  end

  private

  # Stream this client subscribes to
  def my_stream_name
    if @browser_identity.present?
      browser_stream_name(@browser_identity)
    else
      agent_stream_name
    end
  end

  def agent_stream_name
    "preview:#{@hub_id}:#{@agent_index}:agent"
  end

  def browser_stream_name(identity)
    "preview:#{@hub_id}:#{@agent_index}:browser:#{identity}"
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
      payload: { agent_index: @agent_index, browser_identity: @browser_identity })
  rescue => e
    Rails.logger.warn "[Preview] Failed to notify CLI of preview request: #{e.message}"
  end
end
