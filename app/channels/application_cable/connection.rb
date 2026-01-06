# frozen_string_literal: true

module ApplicationCable
  class Connection < ActionCable::Connection::Base
    identified_by :current_user

    def connect
      self.current_user = find_verified_user
    end

    private

    def find_verified_user
      # Try DeviceToken auth (for CLI)
      if (token = request.params[:api_key])
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
  end
end
