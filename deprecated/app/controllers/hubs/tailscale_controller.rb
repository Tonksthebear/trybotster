# frozen_string_literal: true

module Hubs
  # Tailscale integration endpoints for CLI
  #
  # The CLI calls these endpoints to:
  # - Get browser pre-auth keys (for QR code generation)
  # - Update its tailnet hostname (after joining)
  class TailscaleController < ApplicationController
    include ApiKeyAuthenticatable

    before_action :authenticate_with_api_key!
    before_action :set_hub

    # POST /hubs/:hub_identifier/tailscale/browser_key
    #
    # CLI requests a browser pre-auth key to put in QR code URL fragment.
    # The key is ephemeral (1 hour expiry) and auto-cleanup on disconnect.
    #
    # Response: { key: "hskey_..." }
    def browser_key
      key = @hub.create_browser_preauth_key

      if key.present?
        render json: { key: key }
      else
        render json: { error: "Failed to create browser key" }, status: :service_unavailable
      end
    rescue HeadscaleClient::Error => e
      render json: { error: e.message }, status: :service_unavailable
    end

    # PATCH /hubs/:hub_identifier/tailscale/hostname
    #
    # CLI updates its tailnet hostname after successfully joining.
    # This lets the browser know how to reach the CLI via SSH.
    #
    # Params: { hostname: "cli-abc123.tail.local" }
    def update_hostname
      hostname = params[:hostname]

      if hostname.blank?
        render json: { error: "hostname is required" }, status: :bad_request
        return
      end

      @hub.update!(tailscale_hostname: hostname)
      render json: { success: true, hostname: hostname }
    end

    # GET /hubs/:hub_identifier/tailscale/status
    #
    # Get the hub's Tailscale connection status
    def status
      render json: {
        connected: @hub.tailscale_connected?,
        hostname: @hub.tailscale_hostname,
        preauth_key_present: @hub.tailscale_preauth_key.present?
      }
    end

    private

    def set_hub
      @hub = current_api_user.hubs.find_by(identifier: params[:hub_identifier])
      render json: { error: "Hub not found" }, status: :not_found unless @hub
    end
  end
end
