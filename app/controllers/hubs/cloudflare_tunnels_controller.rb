# frozen_string_literal: true

module Hubs
  class CloudflareTunnelsController < ApplicationController
    include ApiKeyAuthenticatable

    skip_before_action :verify_authenticity_token
    before_action :authenticate_hub!

    def show
      tunnel = @hub.cloudflare_tunnel
      return render json: { cloudflare_tunnel: nil } unless tunnel

      render json: { cloudflare_tunnel: tunnel.public_json }
    end

    def create
      tunnel = Hubs::CloudflareTunnel.ensure_for!(@hub)
      render json: { cloudflare_tunnel: tunnel.public_json(include_connector_token: true) }, status: :created
    rescue ActiveRecord::RecordNotUnique => e
      render json: { error: e.message }, status: :conflict
    rescue Cloudflare::TunnelApi::Error => e
      render_cloudflare_error(e)
    end

    def rotate
      tunnel = @hub.cloudflare_tunnel || Hubs::CloudflareTunnel.ensure_for!(@hub)
      tunnel.rotate!
      render json: { cloudflare_tunnel: tunnel.public_json(include_connector_token: true) }
    rescue Cloudflare::TunnelApi::Error => e
      render_cloudflare_error(e)
    end

    def destroy
      @hub.cloudflare_tunnel&.revoke!
      head :no_content
    rescue Cloudflare::TunnelApi::Error => e
      render_cloudflare_error(e)
    end

    private

    def authenticate_hub!
      token = extract_api_key
      return render_unauthorized("API key required") if token.blank?

      hub_token = HubToken.find_by(token: token)
      return render_unauthorized("Invalid API key") unless hub_token&.hub_id == params[:hub_id].to_i

      hub_token.touch_usage!(ip: request.remote_ip)
      @hub = hub_token.hub
    end

    def render_cloudflare_error(error)
      status = error.rate_limited? ? :service_unavailable : :bad_gateway
      payload = { error: "Cloudflare API request failed" }
      payload[:retry_after] = error.retry_after if error.retry_after.present?
      render json: payload, status: status
    end
  end
end
