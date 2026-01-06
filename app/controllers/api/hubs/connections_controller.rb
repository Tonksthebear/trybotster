# frozen_string_literal: true

module Api
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

      # GET /api/hubs/:hub_identifier/connection
      # Returns device public key for Diffie-Hellman key exchange
      # ONLY if user has opted into server-assisted pairing
      def show
        unless @hub.device
          render json: { error: "Hub has no registered device for E2E encryption" }, status: :unprocessable_entity
          return
        end

        render json: {
          hub_id: @hub.id,
          identifier: @hub.identifier,
          server_assisted_pairing: true, # Flag to indicate this is convenience mode
          device: {
            id: @hub.device.id,
            public_key: @hub.device.public_key,
            fingerprint: @hub.device.fingerprint,
            name: @hub.device.name
          }
        }
      end

      private

      def set_hub
        @hub = current_user.hubs.find_by(identifier: params[:hub_identifier])
        render json: { error: "Hub not found" }, status: :not_found unless @hub
      end

      # Require user to opt-in to server-assisted pairing
      # By default, key exchange must happen via QR code URL fragment (MITM-proof)
      def check_server_assisted_pairing
        return if current_user.server_assisted_pairing?

        # Include device fingerprint so browser can check for cached keys
        # The fingerprint is NOT secret - it's derived from the public key
        device_info = if @hub&.device
                        { fingerprint: @hub.device.fingerprint, name: @hub.device.name }
                      end

        render json: {
          error: "Server-assisted pairing is disabled",
          message: "For security, key exchange requires scanning the QR code displayed on your CLI. " \
                   "The key is transmitted via URL fragment which never reaches the server (MITM-proof). " \
                   "To enable server-assisted pairing (less secure), update your settings.",
          secure_connect_url: "/agents/connect",
          enable_convenience_url: "/settings/security",
          device: device_info
        }, status: :forbidden
      end
    end
  end
end
