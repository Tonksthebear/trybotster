# frozen_string_literal: true

class TunnelChannel < ApplicationCable::Channel
  def subscribed
    @hub = current_user.hubs.find_by(identifier: params[:hub_id])
    reject and return unless @hub

    # Stream for this hub - will receive requests for all its agents
    stream_from "tunnel_hub_#{@hub.id}"
  end

  def unsubscribed
    # Mark all agents' tunnels as disconnected
    @hub&.hub_agents&.tunnel_connected&.update_all(tunnel_status: "disconnected")
  end

  # CLI registers an agent's tunnel port
  def register_agent_tunnel(data)
    agent = @hub.hub_agents.find_by(session_key: data["session_key"])
    return unless agent

    agent.update!(tunnel_port: data["port"], tunnel_status: "connected", tunnel_connected_at: Time.current)

    # Broadcast tunnel URL to web UI
    Turbo::StreamsChannel.broadcast_update_to(
      "user_#{@hub.user_id}_hubs",
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
