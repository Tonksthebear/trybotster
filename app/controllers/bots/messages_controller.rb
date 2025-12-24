# frozen_string_literal: true

module Bots
  class MessagesController < ApplicationController
    include ApiKeyAuthenticatable

    skip_before_action :verify_authenticity_token
    before_action :authenticate_with_api_key!

    # GET /bots/messages
    # Returns pending, unclaimed messages for repos the authenticated user has access to
    # First-come-first-served: daemon claims messages by polling
    # Required query param: repo (e.g., ?repo=owner/repo) to filter by repository
    def index
      # Require repo parameter
      if params[:repo].blank?
        render json: {
          error: "Missing required parameter: repo",
          message: "Please provide the repository name in format: owner/repo"
        }, status: :bad_request
        return
      end

      # Get all pending, unclaimed messages for this specific repo
      # Also include webrtc_offer messages for this user (they don't have a repo)
      messages = Bot::Message.for_delivery
        .where(
          "(payload->>'repo' = ?) OR (event_type = 'webrtc_offer' AND payload->>'user_id' = ?)",
          params[:repo],
          current_api_user.id.to_s
        )
        .limit(50)

      # Filter messages by repo access authorization
      authorized_messages = []
      unauthorized_messages = []

      messages.each do |msg|
        repo = msg.repo

        if repo.blank?
          # No repo in payload, allow it through
          authorized_messages << msg
        elsif current_api_user.has_github_repo_access?(repo)
          # User has access to this repo
          authorized_messages << msg
        else
          # User does not have access to this repo (skip, don't mark as failed yet)
          # Another daemon with access might claim it
          unauthorized_messages << msg
        end
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

    # PATCH/PUT /bots/messages/:id
    # Updates a message (typically to acknowledge it)
    def update
      # Find message claimed by this user
      message = Bot::Message.find_by!(id: params[:id], claimed_by_user_id: current_api_user.id)

      # Acknowledge the message (RESTful update)
      message.acknowledge!

      render json: {
        success: true,
        message_id: message.id,
        acknowledged_at: message.acknowledged_at
      }
    rescue ActiveRecord::RecordNotFound
      render json: { error: "Message not found or not claimed by this user" }, status: :not_found
    end
  end
end
