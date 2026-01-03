# frozen_string_literal: true

class SharedTunnelsController < ApplicationController
  include TunnelProxy

  # No authentication required - token-based access
  skip_before_action :verify_authenticity_token
  before_action :find_shared_agent

  def proxy
    return render_not_found("Invalid or expired share link") unless @hub_agent
    return render_not_found("Sharing disabled") unless @hub_agent.sharing_enabled?
    return render_not_found("Tunnel not connected") unless @hub_agent.tunnel_connected?

    proxy_to_tunnel(@hub_agent)
  end

  private

  def find_shared_agent
    @hub_agent = HubAgent.find_by(tunnel_share_token: params[:token])
  end

  def proxy_base_url
    "/share/#{params[:token]}/"
  end
end
