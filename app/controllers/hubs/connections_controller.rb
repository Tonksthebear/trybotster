# frozen_string_literal: true

module Hubs
  # Returns connection info for E2E encrypted terminal access.
  #
  # SECURITY NOTE:
  # By default, this endpoint does NOT return the CLI's public key.
  # Users must scan the QR code to get the key via URL fragment (MITM-proof).
  #
  # If user has opted into "server-assisted pairing", the public key IS returned.
  # This is more convenient but allows potential MITM attacks by the server.
  class ConnectionsController < ApplicationController
    before_action :authenticate_user!
    before_action :set_hub
    before_action :check_server_assisted_pairing

    # GET /hubs/:id/connection
    # Returns device public key for Diffie-Hellman key exchange
    # ONLY if user has opted into server-assisted pairing
    def show
      unless Current.hub.device
        render json: { error: "Hub has no registered device for E2E encryption" }, status: :unprocessable_entity
        return
      end

      render json: {
        hub_id: Current.hub.id,
        identifier: Current.hub.identifier,
        server_assisted_pairing: true,
        device: {
          id: Current.hub.device.id,
          public_key: Current.hub.device.public_key,
          fingerprint: Current.hub.device.fingerprint,
          name: Current.hub.device.name
        }
      }
    end

    private

    def set_hub
      Current.hub = current_user.hubs.find_by(id: params[:hub_id])
      render json: { error: "Hub not found" }, status: :not_found unless Current.hub
    end

    def check_server_assisted_pairing
      return if current_user.server_assisted_pairing?

      device_info = if Current.hub&.device
                      { fingerprint: Current.hub.device.fingerprint, name: Current.hub.device.name }
      end

      render json: {
        error: "Server-assisted pairing is disabled",
        message: "For security, key exchange requires scanning the QR code displayed on your CLI. " \
                 "The key is transmitted via URL fragment which never reaches the server (MITM-proof). " \
                 "To enable server-assisted pairing (less secure), update your settings.",
        secure_connect_url: Current.hub ? "/hubs/#{Current.hub.id}" : "/hubs",
        enable_convenience_url: "/settings",
        device: device_info
      }, status: :forbidden
    end
  end
end
