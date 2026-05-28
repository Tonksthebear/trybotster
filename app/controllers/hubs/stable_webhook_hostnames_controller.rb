# frozen_string_literal: true

module Hubs
  class StableWebhookHostnamesController < ApplicationController
    include ApiKeyAuthenticatable

    skip_before_action :verify_authenticity_token
    before_action :authenticate_hub!

    def create
      hostname = Hubs::StableWebhookHostname.allocate_for!(
        @hub,
        owner_plugin: hostname_params[:owner_plugin],
        owner_key: hostname_params[:owner_key],
        purpose: hostname_params[:purpose],
        local_service_url: hostname_params[:local_service_url]
      )

      render json: { stable_webhook_hostname: hostname.public_json }, status: :created
    rescue Cloudflare::TunnelApi::Error => e
      render_cloudflare_error(e)
    end

    def destroy
      hostname = @hub.stable_webhook_hostnames.find(params[:id])
      hostname.release!
      head :no_content
    rescue ActiveRecord::RecordNotFound
      head :not_found
    rescue Cloudflare::TunnelApi::Error => e
      render_cloudflare_error(e)
    end

    private

    def hostname_params
      params.permit(:owner_plugin, :owner_key, :purpose, :local_service_url)
    end

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
