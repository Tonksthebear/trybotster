# frozen_string_literal: true

class PreviewController < ApplicationController
  include TunnelProxy

  before_action :authenticate_user!
  before_action :find_hub_agent

  def proxy
    return render_not_found("Hub not found") unless @hub
    return render_not_found("Agent not found") unless @hub_agent
    return render_not_found("Tunnel not connected") unless @hub_agent.tunnel_connected?

    proxy_to_tunnel(@hub_agent)
  end

  private

  def find_hub_agent
    # Only allow access to user's own hubs
    @hub = current_user.hubs.find_by(identifier: params[:hub_id])
    @hub_agent = @hub&.hub_agents&.find_by(session_key: params[:agent_id])
  end

  def proxy_base_url
    "/preview/#{params[:hub_id]}/#{params[:agent_id]}/"
  end
end
