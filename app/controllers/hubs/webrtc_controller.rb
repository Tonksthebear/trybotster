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
      return if public_preview_request?

      render json: { error: "Unauthorized" }, status: :unauthorized
    end

    # Allow unauthenticated ICE config requests for sessions with public preview enabled
    def public_preview_request?
      session_uuid = request.params[:session_uuid]
      return false unless session_uuid.present?

      @_public_preview_hub = Hub.find_by(id: params[:hub_id])
      @_public_preview_hub&.alive? && @_public_preview_hub&.public_preview_enabled?(session_uuid)
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
      elsif @_public_preview_hub
               @_public_preview_hub
      end

      render json: { error: "Hub not found" }, status: :not_found unless @hub
    end

    def ice_servers
      turn = turn_credentials
      turn = filter_tcp_turn(Array(turn))

      # When TURN is configured, avoid external Google STUN. For WAN clients,
      # derive a matching stun: URL from the same TURN host so the browser/CLI
      # can gather srflx candidates quickly without depending on extra public
      # endpoints. LAN clients can rely on host candidates alone.
      if turn.any?
        lan_request? ? turn : augment_with_matching_stun(turn)
      elsif lan_request?
        # LAN clients only need host candidates — skip external STUN to avoid
        # 5s timeout probes to Google servers that add no value on local networks.
        []
      else
        [
          { urls: "stun:stun.l.google.com:19302" },
          { urls: "stun:stun1.l.google.com:19302" }
        ]
      end
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

    # Detect LAN clients by request IP (private RFC 1918 / link-local ranges).
    # Used to skip external STUN servers that add 5s timeout on local networks.
    def lan_request?
      ip = request.remote_ip
      return false if ip.blank?

      addr = IPAddr.new(ip)
      LAN_RANGES.any? { |range| range.include?(addr) }
    rescue IPAddr::InvalidAddressError
      false
    end

    LAN_RANGES = [
      IPAddr.new("10.0.0.0/8"),
      IPAddr.new("172.16.0.0/12"),
      IPAddr.new("192.168.0.0/16"),
      IPAddr.new("127.0.0.0/8"),
      IPAddr.new("::1/128"),
      IPAddr.new("fc00::/7")
    ].freeze

    # Remove TCP TURN URLs — TCP TURN in rustrtc has no connect timeout,
    # so a firewalled TCP port hangs for 30-75s (OS TCP timeout).
    # UDP TURN is preferred and sufficient for all use cases.
    #
    # Handles both string URLs ("turn:host:3478?transport=tcp") and array
    # URLs (["turn:host:3478?transport=tcp", "turn:host:443?transport=tcp"])
    # as returned by metered.co.
    def filter_tcp_turn(servers)
      servers.filter_map do |server|
        urls = server[:urls] || server["urls"]

        if urls.is_a?(Array)
          filtered = urls.reject { |u| u.is_a?(String) && u.match?(/\?transport=tcp\z/i) }
          next if filtered.empty?
          next server if filtered.length == urls.length # nothing removed

          server.merge(urls: filtered)
        elsif urls.is_a?(String) && urls.match?(/\?transport=tcp\z/i)
          next # reject entirely
        else
          server
        end
      end
    end

    def augment_with_matching_stun(servers)
      existing_stun_urls = servers.flat_map do |server|
        Array(server[:urls] || server["urls"]).filter_map do |url|
          url if url.is_a?(String) && url.match?(/\Astuns?:/i)
        end
      end

      derived_stun_urls = servers.flat_map do |server|
        Array(server[:urls] || server["urls"]).filter_map do |url|
          derive_stun_url(url)
        end
      end.uniq - existing_stun_urls

      return servers if derived_stun_urls.empty?

      derived_stun_urls.map { |url| { urls: url } } + servers
    end

    def derive_stun_url(url)
      return unless url.is_a?(String)

      clean = url.sub(/\?.*\z/, "")
      return clean.sub(/\Aturn:/i, "stun:") if clean.match?(/\Aturn:/i)
      return clean.sub(/\Aturns:/i, "stuns:") if clean.match?(/\Aturns:/i)

      nil
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
