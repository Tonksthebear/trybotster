class Users::SessionsController < Devise::SessionsController
  include JwtAuthenticatable::InstanceMethods  # For get_token_from_header and decode_jwt helpers

  def destroy
    # Blacklist current token via JTI (if provided in header)
    token = get_token_from_header
    if token
      begin
        payload = JWT.decode(token, Rails.application.credentials.jwt.secret, true, algorithm: "HS256")
        jti = payload[0]["jti"]
        exp = payload[0]["exp"]
        ttl = exp - Time.now.to_i
        Rails.cache.write(jti, true, expires_in: ttl.seconds) if ttl > 0
        Rails.logger.info "JWT token #{jti} blacklisted on logout"
      rescue JWT::DecodeError, JWT::VerificationError
        Rails.logger.warn "Failed to decode token for blacklisting: #{$!.message}"
      end
    end

    # Manually sign out (Devise helper)
    signed_out = sign_out(current_user)

    # Set flash message for web (Devise helper)
    set_flash_message! :notice, :signed_out if signed_out

    # Respond based on format (single response per action)
    respond_to do |format|
      format.json do
        render json: { message: "Logged out successfully and token revoked" }, status: :ok
      end
      format.any { redirect_to after_sign_out_path_for(resource_name) }  # Defaults to root_path
    end
  end
end
