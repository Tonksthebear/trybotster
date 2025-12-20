# frozen_string_literal: true

module Api
  # Handles WebRTC signaling for P2P browser-to-CLI connections
  # Only SDP offers and answers pass through this endpoint - actual data
  # flows directly between browser and CLI via WebRTC data channels.
  class WebrtcSessionsController < ApplicationController
    include ApiKeyAuthenticatable

    skip_before_action :verify_authenticity_token

    # Browser endpoints use session authentication (Devise)
    before_action :authenticate_user!, only: [ :create, :show ]
    # CLI endpoint uses API key authentication
    before_action :authenticate_with_api_key!, only: [ :update ]

    # POST /api/webrtc/sessions
    # Browser creates a session with an SDP offer
    def create
      session = current_user.webrtc_sessions.build(
        offer: session_params[:offer],
        expires_at: 5.minutes.from_now
      )

      unless session.save
        render json: { error: session.errors.full_messages.join(", ") }, status: :unprocessable_entity
        return
      end

      # Create a message for the CLI to pick up via polling
      # This triggers the CLI to handle the WebRTC offer
      create_bot_message(session)

      render json: {
        session_id: session.id,
        status: session.status,
        expires_at: session.expires_at
      }, status: :created
    end

    # GET /api/webrtc/sessions/:id
    # Browser polls for the answer from CLI
    def show
      session = current_user.webrtc_sessions.find(params[:id])

      # Check if expired
      if session.expired?
        session.mark_expired! unless session.status == "expired"
        render json: {
          status: "expired",
          error: "Session expired"
        }, status: :gone
        return
      end

      render json: {
        status: session.status,
        answer: session.answer
      }
    rescue ActiveRecord::RecordNotFound
      render json: { error: "Session not found" }, status: :not_found
    end

    # PATCH /api/webrtc/sessions/:id
    # CLI posts the SDP answer
    def update
      session = WebrtcSession.find(params[:id])

      # Verify the CLI user has access (must be the same user who owns the session)
      unless session.user == current_api_user
        render json: { error: "Unauthorized" }, status: :unauthorized
        return
      end

      # Check if expired
      if session.expired?
        session.mark_expired! unless session.status == "expired"
        render json: { error: "Session expired" }, status: :gone
        return
      end

      # Set the answer
      session.set_answer!(answer_params[:answer])

      render json: {
        success: true,
        session_id: session.id,
        status: session.status
      }
    rescue ActiveRecord::RecordNotFound
      render json: { error: "Session not found" }, status: :not_found
    end

    private

    def session_params
      params.permit(offer: [ :type, :sdp ])
    end

    def answer_params
      params.permit(answer: [ :type, :sdp ])
    end

    def create_bot_message(session)
      # Create a Bot::Message that the CLI will pick up on its next poll
      # The CLI polls for messages and will receive this webrtc_offer event
      Bot::Message.create!(
        event_type: "webrtc_offer",
        payload: {
          session_id: session.id.to_s,  # CLI expects string for URL building
          offer: session.offer,
          user_id: current_user.id,
          ice_servers: build_ice_servers  # Include ICE servers so CLI doesn't need env vars
        }
      )
    end

    # Build ICE servers for WebRTC (shared with AgentsController)
    def build_ice_servers
      turn_api_key = ENV["METERED_TURN_API_KEY"]
      if turn_api_key.present?
        fetch_metered_ice_servers(turn_api_key)
      else
        Rails.logger.warn "No METERED_TURN_API_KEY - WebRTC may fail on cellular networks"
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
end
