# frozen_string_literal: true

module ApplicationCable
  class Connection < ActionCable::Connection::Base
    identified_by :current_user
    identified_by :preview_identity

    def connect
      # Try authenticated paths first (session, HubToken)
      if (user = find_authenticated_user)
        self.current_user = user
      elsif (identity = request.params[:preview_identity])&.start_with?("preview:")
        # Anonymous public preview connection — identified by preview string only
        self.preview_identity = identity
        Rails.logger.info "[ActionCable] Preview connection: #{identity[..30]}"
      else
        Rails.logger.warn "[ActionCable] No valid auth - rejecting connection"
        reject_unauthorized_connection
      end
    end

    private

    def find_authenticated_user
      # Try session auth (for browser - Fizzy pattern)
      if env["warden"] && (user = env["warden"].user)
        Rails.logger.info "[ActionCable] Auth via session: user=#{user.id}"
        return user
      end

      # Try HubToken auth (for CLI via Authorization header)
      token = extract_hub_token
      Rails.logger.debug "[ActionCable] Authorization header present: #{token.present?}"

      if token.present?
        hub_token = HubToken.find_by(token: token)
        if hub_token
          hub_token.touch_usage!(ip: request.remote_ip)
          Rails.logger.info "[ActionCable] Auth via HubToken: user=#{hub_token.user&.id}"
          return hub_token.user
        else
          Rails.logger.warn "[ActionCable] HubToken not found for provided token"
        end
      end

      nil
    end

    def extract_hub_token
      # Bearer token in Authorization header only (no query param for security)
      auth_header = request.headers["HTTP_AUTHORIZATION"] || request.headers["Authorization"]
      return nil unless auth_header.present?

      auth_header.delete_prefix("Bearer ")
    end
  end
end
