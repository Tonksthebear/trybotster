# frozen_string_literal: true

module Bots
  class MessagesController < ApplicationController
    include ApiKeyAuthenticatable

    skip_before_action :verify_authenticity_token
    before_action :authenticate_with_api_key!

    # GET /bots/messages
    # Returns pending messages for the authenticated user
    def index
      messages = current_api_user.bot_messages.for_delivery.limit(50)

      render json: {
        messages: messages.map do |msg|
          {
            id: msg.id,
            event_type: msg.event_type,
            payload: msg.payload,
            created_at: msg.created_at,
            sent_at: msg.sent_at
          }
        end,
        count: messages.count
      }
    end

    # PATCH/PUT /bots/messages/:id
    # Updates a message (typically to acknowledge it)
    def update
      message = current_api_user.bot_messages.find(params[:id])

      # Acknowledge the message (RESTful update)
      message.acknowledge!

      render json: {
        success: true,
        message_id: message.id,
        acknowledged_at: message.acknowledged_at
      }
    rescue ActiveRecord::RecordNotFound
      render json: { error: "Message not found" }, status: :not_found
    end
  end
end
