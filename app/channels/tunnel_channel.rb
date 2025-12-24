# frozen_string_literal: true

class TunnelChannel < ApplicationCable::Channel
  def subscribed
    # Store the hub identifier for later use
    # We stream based on identifier, not DB id, so the hub doesn't need to exist yet
    @hub_identifier = params[:hub_id]
    @user_id = current_user.id

    # Stream for this hub identifier - hub record will be created by heartbeat
    stream_from "tunnel_hub_#{@user_id}_#{@hub_identifier}"
  end

  def unsubscribed
    # Mark all agents' tunnels as disconnected
    hub = current_user.hubs.find_by(identifier: @hub_identifier)
    hub&.hub_agents&.tunnel_connected&.update_all(tunnel_status: "disconnected")
  end

  # CLI registers an agent's tunnel port
  def register_agent_tunnel(data)
    hub = current_user.hubs.find_by(identifier: @hub_identifier)
    return unless hub

    agent = hub.hub_agents.find_by(session_key: data["session_key"])
    return unless agent

    agent.update!(tunnel_port: data["port"], tunnel_status: "connected", tunnel_connected_at: Time.current)

    # Broadcast tunnel URL to web UI
    Turbo::StreamsChannel.broadcast_update_to(
      "user_#{hub.user_id}_hubs",
      target: "hub_agent_#{agent.id}",
      partial: "hub_agents/hub_agent",
      locals: { hub_agent: agent }
    )
  rescue StandardError => e
    Rails.logger.error("Failed to register agent tunnel: #{e.message}")
  end

  # CLI sends HTTP response back
  def http_response(data)
    TunnelResponseStore.fulfill(data["request_id"], data)
  end
end
