# frozen_string_literal: true

# Custom ActionCable configuration to allow DeviceToken authenticated connections
# to bypass origin checking (for CLI WebSocket connections)

Rails.application.config.after_initialize do
  ActionCable::Connection::Base.class_eval do
    # Override the default origin checking to allow DeviceToken authenticated requests
    def allow_request_origin?
      # If request has a valid DeviceToken, skip origin check (CLI connections)
      # Check Authorization header (Bearer token) - same pattern as ApplicationCable::Connection
      auth_header = request.headers["Authorization"] || env["HTTP_AUTHORIZATION"]
      if auth_header.present? && auth_header.start_with?("Bearer ")
        token = auth_header.sub("Bearer ", "")
        device_token = DeviceToken.find_by(token: token)
        return true if device_token.present?
      end

      # Otherwise, fall back to default origin checking
      return true if server.config.disable_request_forgery_protection

      proto = Rack::Request.new(env).ssl? ? "https" : "http"
      if server.config.allow_same_origin_as_host && env["HTTP_ORIGIN"] == "#{proto}://#{env['HTTP_HOST']}"
        true
      elsif Array(server.config.allowed_request_origins).any? { |allowed_origin| allowed_origin === env["HTTP_ORIGIN"] }
        true
      else
        logger.error("Request origin not allowed: #{env['HTTP_ORIGIN']}")
        false
      end
    end
  end
end
