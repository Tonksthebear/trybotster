# frozen_string_literal: true

class AgentsController < ApplicationController
  before_action :authenticate_user!

  # GET /agents
  # Dashboard showing running agents with WebRTC P2P connection
  def index
    # Any active WebRTC sessions for this user (for reconnection)
    @active_sessions = current_user.webrtc_sessions.active.order(created_at: :desc)

    # ICE server configuration for WebRTC (STUN + optional TURN)
    @ice_servers = build_ice_servers
  end

  private

  def build_ice_servers
    # Fetch TURN credentials from metered.ca API if configured
    turn_api_key = ENV["METERED_TURN_API_KEY"]
    if turn_api_key.present?
      fetch_metered_ice_servers(turn_api_key)
    else
      # Fallback to STUN-only (may fail on cellular/symmetric NAT)
      Rails.logger.warn "No METERED_TURN_API_KEY configured - WebRTC may fail on cellular networks"
      [
        { urls: "stun:stun.l.google.com:19302" },
        { urls: "stun:stun1.l.google.com:19302" }
      ]
    end
  end

  def fetch_metered_ice_servers(api_key)
    Rails.cache.fetch("metered_ice_servers", expires_in: 1.hour) do
      response = Faraday.get("https://trybotster.metered.live/api/v1/turn/credentials?apiKey=#{api_key}")

      if response.success?
        JSON.parse(response.body)
      else
        Rails.logger.error "Failed to fetch TURN credentials: #{response.status}"
        # Fallback to STUN only
        [
          { urls: "stun:stun.l.google.com:19302" },
          { urls: "stun:stun1.l.google.com:19302" }
        ]
      end
    end
  rescue Faraday::Error => e
    Rails.logger.error "Failed to fetch TURN credentials: #{e.message}"
    [
      { urls: "stun:stun.l.google.com:19302" },
      { urls: "stun:stun1.l.google.com:19302" }
    ]
  end
end
