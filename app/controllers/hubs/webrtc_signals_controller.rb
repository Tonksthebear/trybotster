# frozen_string_literal: true

module Hubs
  # Ephemeral WebRTC signaling with Redis-backed storage
  #
  # Relays SDP offers/answers and ICE candidates between browser and CLI.
  # Signals are stored in Redis with short TTL for polling retrieval.
  #
  # Signal types:
  # - offer: Browser -> CLI (SDP offer to initiate connection)
  # - answer: CLI -> Browser (SDP answer to accept connection)
  # - ice: Either -> Other (ICE candidate for NAT traversal)
  #
  # Auth:
  # - Browser: session auth (current_user)
  # - CLI: DeviceToken Bearer auth
  class WebrtcSignalsController < ApplicationController
    skip_before_action :verify_authenticity_token
    before_action :authenticate_user_or_device!
    before_action :set_hub

    SIGNAL_TTL = 60 # seconds

    # GET /hubs/:hub_id/webrtc_signals
    # Fetch pending signals for this browser/CLI
    def index
      browser_identity = params[:browser_identity]
      return render json: { error: "browser_identity required" }, status: :bad_request unless browser_identity

      signals = []

      if current_user
        # Browser fetching: get answer and CLI's ICE candidates
        answer_key = signal_key("answer", browser_identity)
        answer = Rails.cache.read(answer_key)
        if answer
          signals << { type: "answer", sdp: answer }
          Rails.cache.delete(answer_key)
        end

        # Get ICE candidates from CLI
        ice_key = signal_key("ice_from_cli", browser_identity)
        ice_candidates = Rails.cache.read(ice_key) || []
        ice_candidates.each { |c| signals << { type: "ice", candidate: c } }
        Rails.cache.delete(ice_key) if ice_candidates.any?
      elsif current_device
        # CLI fetching: get offer and browser's ICE candidates
        offer_key = signal_key("offer", browser_identity)
        offer = Rails.cache.read(offer_key)
        if offer
          signals << { type: "offer", sdp: offer }
          Rails.cache.delete(offer_key)
        end

        # Get ICE candidates from browser
        ice_key = signal_key("ice_from_browser", browser_identity)
        ice_candidates = Rails.cache.read(ice_key) || []
        ice_candidates.each { |c| signals << { type: "ice", candidate: c } }
        Rails.cache.delete(ice_key) if ice_candidates.any?
      end

      render json: { signals: signals }
    end

    # POST /hubs/:hub_id/webrtc_signals
    # Create and relay a signal (offer, answer, or ice candidate)
    def create
      case params[:signal_type]
      when "offer"
        relay_offer
      when "answer"
        relay_answer
      when "ice"
        relay_ice
      else
        render json: { error: "Invalid signal_type" }, status: :unprocessable_entity
      end
    end

    private

    def relay_offer
      unless current_user
        render json: { error: "Only browsers can send offers" }, status: :forbidden
        return
      end

      unless @hub.alive?
        render json: { error: "CLI is offline" }, status: :service_unavailable
        return
      end

      browser_identity = params[:browser_identity]

      # Store offer in cache for CLI to poll
      offer_key = signal_key("offer", browser_identity)
      Rails.cache.write(offer_key, params[:sdp], expires_in: SIGNAL_TTL.seconds)

      # Also broadcast via ActionCable for CLI that's already listening
      ActionCable.server.broadcast(
        "hub_command:#{@hub.id}",
        {
          type: "webrtc_offer",
          browser_identity: browser_identity,
          agent_index: params[:agent_index],
          pty_index: params[:pty_index],
          sdp: params[:sdp]
        }
      )

      render json: { status: "relayed" }, status: :created
    end

    def relay_answer
      unless current_device
        render json: { error: "Only CLI can send answers" }, status: :forbidden
        return
      end

      browser_identity = params[:browser_identity]

      # Store answer in cache for browser to poll
      answer_key = signal_key("answer", browser_identity)
      Rails.cache.write(answer_key, params[:sdp], expires_in: SIGNAL_TTL.seconds)

      render json: { status: "relayed" }, status: :created
    end

    def relay_ice
      browser_identity = params[:browser_identity]
      # Convert to plain hash for cache serialization
      candidate = params[:candidate].to_unsafe_h

      if current_user
        # Browser -> CLI: append to ICE candidates list
        ice_key = signal_key("ice_from_browser", browser_identity)
        candidates = Rails.cache.read(ice_key) || []
        candidates << candidate
        Rails.cache.write(ice_key, candidates, expires_in: SIGNAL_TTL.seconds)
      elsif current_device
        # CLI -> Browser: append to ICE candidates list
        ice_key = signal_key("ice_from_cli", browser_identity)
        candidates = Rails.cache.read(ice_key) || []
        candidates << candidate
        Rails.cache.write(ice_key, candidates, expires_in: SIGNAL_TTL.seconds)
      end

      render json: { status: "relayed" }, status: :created
    end

    def signal_key(type, browser_identity)
      "webrtc:#{@hub.id}:#{browser_identity}:#{type}"
    end

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
  end
end
