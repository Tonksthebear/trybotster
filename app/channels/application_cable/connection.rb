# frozen_string_literal: true

module ApplicationCable
  class Connection < ActionCable::Connection::Base
    identified_by :current_user

    def connect
      self.current_user = find_verified_user
    end

    private

    def find_verified_user
      # Try DeviceToken auth (for CLI) - query param for WebSocket compatibility
      token = extract_api_token
      if token.present?
        device_token = DeviceToken.find_by(token: token)
        if device_token
          device_token.touch_usage!(ip: request.remote_ip)
          return device_token.user
        end
      end

      # Try session auth (for browser)
      if env["warden"] && (user = env["warden"].user)
        return user
      end

      reject_unauthorized_connection
    end

    def extract_api_token
      # Support query param (WebSocket standard) or Authorization header
      request.params[:api_key] || extract_bearer_token
    end

    def extract_bearer_token
      auth_header = request.headers["Authorization"]
      auth_header&.delete_prefix("Bearer ")
    end
  end
end
