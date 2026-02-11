# frozen_string_literal: true

module Hubs
  # Updates hub last_seen_at timestamp (heartbeat/keepalive)
  class HeartbeatsController < ApplicationController
    include ApiKeyAuthenticatable

    before_action :authenticate_hub_request!
    before_action :set_hub

    # PATCH /hubs/:id/heartbeat
    def update
      Current.hub.touch(:last_seen_at)

      render json: { success: true, last_seen_at: Current.hub.last_seen_at }
    end

    private

    def authenticate_hub_request!
      if api_key_present?
        authenticate_with_api_key!
      else
        authenticate_user!
      end
    end

    def current_hub_user
      current_api_user || current_user
    end

    def set_hub
      Current.hub = current_hub_user.hubs.find_by(id: params[:hub_id])
      render json: { error: "Hub not found" }, status: :not_found unless Current.hub
    end
  end
end
