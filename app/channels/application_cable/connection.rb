# frozen_string_literal: true

module ApplicationCable
  class Connection < ActionCable::Connection::Base
    identified_by :current_user

    def connect
      self.current_user = find_verified_user
    end

    private

    def find_verified_user
      # Try session auth (for browser - Fizzy pattern)
      if env["warden"] && (user = env["warden"].user)
        Rails.logger.info "[ActionCable] Auth via session: user=#{user.id}"
        return user
      end

      # Try DeviceToken auth (for CLI via Authorization header)
      token = extract_device_token
      Rails.logger.debug "[ActionCable] Authorization header present: #{token.present?}"

      if token.present?
        device_token = DeviceToken.find_by(token: token)
        if device_token
          device_token.touch_usage!(ip: request.remote_ip)
          Rails.logger.info "[ActionCable] Auth via DeviceToken: user=#{device_token.user&.id}"
          return device_token.user
        else
          Rails.logger.warn "[ActionCable] DeviceToken not found for provided token"
        end
      end

      Rails.logger.warn "[ActionCable] No valid auth - rejecting connection"
      reject_unauthorized_connection
    end

    def extract_device_token
      # Bearer token in Authorization header only (no query param for security)
      auth_header = request.headers["HTTP_AUTHORIZATION"] || request.headers["Authorization"]
      return nil unless auth_header.present?

      auth_header.delete_prefix("Bearer ")
    end
  end
end
