# frozen_string_literal: true

module Hubs
  # WebRTC configuration endpoint
  #
  # Returns ICE server configuration (STUN/TURN) for WebRTC connection setup.
  # TURN credentials are time-limited per RFC 5389.
  #
  # Auth:
  # - Browser: session auth (current_user)
  # - CLI: DeviceToken Bearer auth
  class WebrtcController < ApplicationController
    skip_before_action :verify_authenticity_token
    before_action :authenticate_user_or_device!
    before_action :set_hub

    # GET /hubs/:hub_id/webrtc
    # Returns ICE server configuration
    def show
      render json: { ice_servers: ice_servers }
    end

    private

    def authenticate_user_or_device!
      return if current_user
      return if authenticate_device_from_token

      render json: { error: "Unauthorized" }, status: :unauthorized
    end

    def authenticate_device_from_token
      auth_header = request.headers["Authorization"]
      return false unless auth_header&.start_with?("Bearer ")

      token = auth_header.split(" ", 2).last
      device = DeviceToken.find_by(token: token)&.device
      return false unless device

      @current_device = device
      true
    end

    def current_device
      @current_device
    end

    def set_hub
      @hub = if current_user
               current_user.hubs.find_by(id: params[:hub_id])
      elsif current_device
               current_device.hub
      end

      render json: { error: "Hub not found" }, status: :not_found unless @hub
    end

    def ice_servers
      servers = [
        { urls: "stun:stun.l.google.com:19302" },
        { urls: "stun:stun1.l.google.com:19302" }
      ]

      turn = turn_credentials
      servers << turn if turn

      servers
    end

    # Generate time-limited TURN credentials (RFC 5389)
    def turn_credentials
      return nil unless ENV["TURN_SERVER_URL"].present? && ENV["TURN_SECRET"].present?

      timestamp = 24.hours.from_now.to_i
      username = "#{timestamp}:#{@hub.id}"
      password = Base64.strict_encode64(
        OpenSSL::HMAC.digest("SHA1", ENV["TURN_SECRET"], username)
      )

      {
        urls: ENV["TURN_SERVER_URL"],
        username: username,
        credential: password
      }
    end
  end
end
