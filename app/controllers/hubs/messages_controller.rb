# frozen_string_literal: true

module Hubs
  class MessagesController < ApplicationController
    include ApiKeyAuthenticatable

    before_action :authenticate_with_api_key!
    before_action :set_hub

    # PATCH/PUT /hubs/:id/messages/:id
    # Acknowledges a message
    def update
      message = Bot::Message.find_by!(id: params[:id], claimed_by_user_id: current_api_user.id)
      message.acknowledge!

      render json: {
        success: true,
        message_id: message.id,
        acknowledged_at: message.acknowledged_at
      }
    rescue ActiveRecord::RecordNotFound
      render json: { error: "Message not found or not claimed by this user" }, status: :not_found
    end

    private

    def set_hub
      Current.hub = current_api_user.hubs.find_by!(id: params[:hub_id])
    rescue ActiveRecord::RecordNotFound
      render json: { error: "Hub not found" }, status: :not_found
    end
  end
end
