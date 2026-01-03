# frozen_string_literal: true

class PreviewController < ApplicationController
  include TunnelProxy

  # Tunnel proxy security model:
  # 1. Authentication: User must be logged in (authenticate_user!)
  # 2. Authorization: User can only access their own hubs (find_hub_agent scopes to current_user)
  # 3. Hub IDs are UUIDs (not guessable)
  #
  # We skip forgery protection because:
  # - Service Worker fetches and script tags don't include CSRF tokens
  # - verify_same_origin_request blocks cross-origin JS serving (needed for assets)
  # - Requests only proxy to the user's own local dev server
  #
  # Risk: An attacker with hub UUID + agent session_key could CSRF POST/PUT/DELETE
  # to the user's own dev server. Mitigated by UUID unpredictability and the fact
  # that the target is the user's own development environment.
  skip_forgery_protection only: [:service_worker, :proxy]

  before_action :authenticate_user!
  before_action :find_hub_agent

  # Main proxy endpoint - serves bootstrap or proxies content
  def proxy
    return render_not_found("Hub not found") unless @hub
    return render_not_found("Agent not found") unless @hub_agent
    return render_not_found("Tunnel not connected") unless @hub_agent.tunnel_connected?

    # Always show bootstrap on first load to ensure SW is current
    # The SW version hash ensures updates are detected
    unless service_worker_ready?
      return render_bootstrap
    end

    proxy_to_tunnel(@hub_agent)
  end

  # Serves the Service Worker JavaScript
  def service_worker
    return render_not_found("Hub not found") unless @hub
    return render_not_found("Agent not found") unless @hub_agent

    @proxy_base = proxy_base_url

    response.headers["Content-Type"] = "application/javascript"
    response.headers["Service-Worker-Allowed"] = scope_path
    render template: "preview/service_worker", formats: [:js], layout: false
  end

  private

  def find_hub_agent
    @hub = current_user.hubs.find_by(identifier: params[:hub_id])
    @hub_agent = @hub&.hub_agents&.find_by(session_key: params[:agent_id])
  end

  def service_worker_ready?
    # Check if SW cookie matches current version
    cookies[:tunnel_sw] == sw_version
  end

  def sw_version
    @sw_version ||= Digest::MD5.hexdigest(
      File.read(Rails.root.join("app/views/preview/service_worker.js.erb"))
    )[0..7]
  end

  def render_bootstrap
    @sw_path = service_worker_path
    @scope = scope_path
    @sw_version = sw_version
    render template: "preview/bootstrap", layout: false
  end

  def service_worker_path
    "/preview/#{params[:hub_id]}/#{params[:agent_id]}/sw.js"
  end

  def scope_path
    # No trailing slash - SW scope must cover the root URL without slash
    "/preview/#{params[:hub_id]}/#{params[:agent_id]}"
  end

  def proxy_base_url
    "/preview/#{params[:hub_id]}/#{params[:agent_id]}"
  end
end
