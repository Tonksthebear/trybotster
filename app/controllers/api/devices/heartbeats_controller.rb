# frozen_string_literal: true

module Api
  module Devices
    # Updates device last_seen_at timestamp (heartbeat/keepalive)
    class HeartbeatsController < ApplicationController
      include ApiKeyAuthenticatable

      skip_before_action :verify_authenticity_token
      before_action :authenticate_device_request!
      before_action :set_device

      # PATCH /api/devices/:device_id/heartbeat
      def update
        @device.touch_last_seen!

        render json: { success: true, last_seen_at: @device.last_seen_at }
      end

      private

      def authenticate_device_request!
        if request.headers["X-API-Key"].present?
          authenticate_with_api_key!
        else
          authenticate_user!
        end
      end

      def current_device_user
        current_api_user || current_user
      end

      def set_device
        @device = current_device_user.devices.find(params[:device_id])
      rescue ActiveRecord::RecordNotFound
        render json: { error: "Device not found" }, status: :not_found
      end
    end
  end
end
