# frozen_string_literal: true

module Hubs
  class MessagesController < ApplicationController
    include ApiKeyAuthenticatable

    before_action :authenticate_with_api_key!
    before_action :set_hub

    # GET /hubs/:id/messages
    # Returns pending, unclaimed messages for this hub's repo
    def index
      # Get all pending, unclaimed messages for this hub's repo
      # Also include webrtc_offer messages for this user (they don't have a repo)
      messages = Bot::Message.for_delivery
        .where(
          "(payload->>'repo' = ?) OR (event_type = 'webrtc_offer' AND payload->>'user_id' = ?)",
          @hub.repo,
          current_api_user.id.to_s
        )
        .limit(50)

      # Filter messages by repo access authorization
      authorized_messages = []

      messages.each do |msg|
        repo = msg.repo

        if repo.blank?
          # No repo in payload, allow it through
          authorized_messages << msg
        elsif current_api_user.has_github_repo_access?(repo)
          # User has access to this repo
          authorized_messages << msg
        end
        # Skip messages the user doesn't have access to
      end

      # Claim authorized messages for this user's daemon
      authorized_messages.each do |msg|
        msg.claim!(current_api_user.id)
      end

      render json: {
        messages: authorized_messages.map do |msg|
          {
            id: msg.id,
            event_type: msg.event_type,
            payload: msg.payload,
            created_at: msg.created_at,
            sent_at: msg.sent_at,
            claimed_at: msg.claimed_at
          }
        end,
        count: authorized_messages.count
      }
    end

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
      @hub = current_api_user.hubs.find_by!(id: params[:hub_id])
    rescue ActiveRecord::RecordNotFound
      render json: { error: "Hub not found" }, status: :not_found
    end
  end
end
