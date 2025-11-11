module JwtAuthenticatable
  extend ActiveSupport::Concern

  # Define class methods first
  module ClassMethods
    # Skip JWT auth for specific actions
    def allow_unauthenticated_jwt_access(**options)
      skip_before_action :authenticate_jwt_user!, options
    end
  end

  # Define instance methods first
  module InstanceMethods
    # Encode user data into JWT (called after SSO)
    def encode_jwt(user)
      now = Time.now.to_i
      payload = {
        data: {
          id: user.id,
          email: user.email,
          username: user.username  # From GitHub
        },
        exp: now + 1.hour.to_i,  # Adjust expiration
        iat: now,
        iss: "trybotster",   # Issuer
        aud: "mcp-client",       # Audience (your client)
        sub: "user:#{user.id}",
        jti: SecureRandom.uuid   # Unique ID for blacklisting
      }

      JWT.encode(payload, Rails.application.credentials.jwt.secret, "HS256")
    end

    # Decode and verify token, check blacklist
    def decode_jwt
      token = get_token_from_header
      return nil unless token

      begin
        decoded = JWT.decode(token, Rails.application.credentials.jwt.secret, true, algorithm: "HS256")

        # Extract JTI and check if blacklisted in solid_cache
        jti = decoded[0]["jti"]
        if Rails.cache.exist?(jti)
          Rails.logger.warn "Token #{jti} is blacklisted"
          return nil
        end

        decoded[0]["data"].with_indifferent_access  # Return user data
      rescue JWT::DecodeError, JWT::VerificationError
        Rails.logger.warn "JWT decode error: #{$!.message}"
        nil
      end
    end

    # Set current_user from decoded token
    def current_user_from_jwt
      @current_user_from_jwt ||= begin
        user_data = decode_jwt
        return nil unless user_data

        User.find_by(id: user_data[:id])  # Fetch full user (or cache if needed)
      end
    end

    # Authenticate: Halt if no valid token
    def authenticate_jwt_user!
      unless current_user_from_jwt
        render json: { error: "Unauthorized: Invalid or expired token" }, status: :unauthorized
      end
    end

  private

    def get_token_from_header
      request.headers["Authorization"]&.match(/\ABearer (.*)\z/)&.captures&.first
    end
  end

  # Hook to include/extend when the concern is mixed in
  included do
    extend ClassMethods  # Adds class methods (e.g., allow_unauthenticated_jwt_access)
    include InstanceMethods  # Adds instance methods (e.g., encode_jwt)
    before_action :authenticate_jwt_user!  # Default: Require auth on all actions
  end
end
