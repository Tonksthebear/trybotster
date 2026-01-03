# frozen_string_literal: true

# Custom ActionCable configuration to allow API-key authenticated connections
# to bypass origin checking (for CLI WebSocket connections)

Rails.application.config.after_initialize do
  ActionCable::Connection::Base.class_eval do
    # Override the default origin checking to allow API key authenticated requests
    def allow_request_origin?
      # If request has a valid API key, skip origin check
      if request.params[:api_key].present?
        user = User.find_by_api_key(request.params[:api_key])
        return true if user.present?
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
