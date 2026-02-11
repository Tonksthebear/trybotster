# frozen_string_literal: true

module ApiKeyAuthenticatable
  extend ActiveSupport::Concern

  included do
    # This concern is opt-in, so controllers must explicitly call authenticate_with_api_key!
    # or use it as a before_action
  end

  private

  def authenticate_with_api_key!
    api_key = extract_api_key

    if api_key.blank?
      render_unauthorized("API key required")
      return
    end

    @current_api_user = find_user_by_token(api_key)

    unless @current_api_user
      render_unauthorized("Invalid API key")
    end
  end

  def current_api_user
    @current_api_user
  end

  def extract_api_key
    # Support Authorization: Bearer header (preferred, Fizzy-style)
    # Falls back to X-API-Key header and query param for backwards compatibility
    if (auth_header = request.headers["Authorization"])
      auth_header.delete_prefix("Bearer ")
    else
      request.headers["X-API-Key"] || params[:api_key]
    end
  end

  def api_key_present?
    extract_api_key.present?
  end

  def render_unauthorized(message)
    render json: { error: message }, status: :unauthorized
  end

  def find_user_by_token(token)
    device_token = DeviceToken.find_by(token: token)
    return nil unless device_token

    device_token.touch_usage!(ip: request.remote_ip)
    device_token.device&.user
  end
end
