# frozen_string_literal: true

require "digest"

module Hubs
  # WebRTC configuration endpoint
  #
  # Returns ICE server configuration (STUN/TURN) for WebRTC connection setup.
  # TURN credentials are time-limited per RFC 5389.
  #
  # Auth:
  # - Browser: session auth (current_user)
  # - CLI: HubToken Bearer auth
  class WebrtcController < ApplicationController
    skip_before_action :verify_authenticity_token
    before_action :authenticate_user_or_hub!
    before_action :set_hub

    # GET /hubs/:hub_id/webrtc
    # Returns ICE server configuration
    def show
      render json: { ice_servers: ice_servers }
    end

    private

    def authenticate_user_or_hub!
      return if current_user
      return if authenticate_hub_from_token

      render json: { error: "Unauthorized" }, status: :unauthorized
    end

    def authenticate_hub_from_token
      auth_header = request.headers["Authorization"]
      return false unless auth_header&.start_with?("Bearer ")

      token = auth_header.split(" ", 2).last
      hub_token = HubToken.find_by(token: token)
      return false unless hub_token&.hub

      @current_hub_from_token = hub_token.hub
      true
    end

    def set_hub
      @hub = if current_user
               current_user.hubs.find_by(id: params[:hub_id])
      elsif @current_hub_from_token
               # Verify the token's hub matches the requested hub
               @current_hub_from_token if @current_hub_from_token.id == params[:hub_id].to_i
      end

      render json: { error: "Hub not found" }, status: :not_found unless @hub
    end

    def ice_servers
      servers = [
        { urls: "stun:stun.l.google.com:19302" },
        { urls: "stun:stun1.l.google.com:19302" }
      ]

      turn = turn_credentials
      servers.concat(Array(turn))

      servers
    end

    # TURN credentials
    # Supports two modes:
    # 1. Metered.co API: METERED_DOMAIN + METERED_SECRET_KEY (generates temp credentials)
    # 2. Time-limited credentials (RFC 5389, coturn style): TURN_SERVER_URL + TURN_SECRET
    # Returns array of servers (may be empty)
    def turn_credentials
      if ENV["METERED_DOMAIN"].present? && ENV["METERED_SECRET_KEY"].present?
        fetch_metered_credentials
      elsif ENV["TURN_SERVER_URL"].present? && ENV["TURN_SECRET"].present?
        # Time-limited credentials (RFC 5389, self-hosted coturn)
        timestamp = 24.hours.from_now.to_i
        username = "#{timestamp}:#{@hub.id}"
        password = Base64.strict_encode64(
          OpenSSL::HMAC.digest("SHA1", ENV["TURN_SECRET"], username)
        )
        [ {
          urls: ENV["TURN_SERVER_URL"],
          username: username,
          credential: password
        } ]
      else
        []
      end
    end

    # Fetch temporary TURN credentials from metered.co API
    # Returns array of all STUN/TURN servers (metered returns multiple)
    def fetch_metered_credentials
      domain = ENV["METERED_DOMAIN"]
      api_key = ENV["METERED_SECRET_KEY"]
      cache_key = "webrtc:metered:#{domain}:#{Digest::SHA256.hexdigest(api_key)}"

      Rails.cache.fetch(cache_key, expires_in: 5.minutes, race_condition_ttl: 10.seconds) do
        uri = URI::HTTPS.build(
          host: domain,
          path: "/api/v1/turn/credentials",
          query: URI.encode_www_form(apiKey: api_key)
        )

        http = Net::HTTP.new(uri.host, uri.port)
        http.use_ssl = true
        http.open_timeout = 2
        http.read_timeout = 2
        http.write_timeout = 2 if http.respond_to?(:write_timeout=)
        response = http.get(uri.request_uri)

        next [] unless response.is_a?(Net::HTTPSuccess)

        credentials = JSON.parse(response.body)
        next [] if credentials.empty?

        # Metered returns array of server configs (STUN + multiple TURN variants)
        # Map all of them to ice_server format
        credentials.map do |cred|
          {
            urls: cred["urls"] || cred["url"],
            username: cred["username"],
            credential: cred["credential"]
          }.compact
        end
      end
    rescue StandardError => e
      Rails.logger.error "[WebRTC] Failed to fetch metered.co credentials: #{e.message}"
      []
    end
  end
end
