# frozen_string_literal: true

class SharedTunnelsController < ApplicationController
  include TunnelProxy

  # Public tunnel sharing security model:
  # 1. Token-based access (no authentication required)
  # 2. Token is cryptographically random (SecureRandom)
  # 3. Sharing must be explicitly enabled by hub owner
  # 4. Owner can revoke access by disabling sharing (regenerates token)
  #
  # We skip forgery protection because:
  # - Public access means no session/CSRF context
  # - verify_same_origin_request blocks cross-origin JS serving (needed for assets)
  # - The token acts as the authorization mechanism
  skip_forgery_protection only: :proxy
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
