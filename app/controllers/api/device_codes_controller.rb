# frozen_string_literal: true

module Api
  # Handles device authorization flow (RFC 8628).
  # CLI requests device code, polls for approval, receives token.
  class DeviceCodesController < ApplicationController
    skip_before_action :verify_authenticity_token

    # POST /api/device_codes
    # CLI requests a new device code to start auth flow
    def create
      auth = DeviceAuthorization.create!(device_name: params[:device_name])

      render json: {
        device_code: auth.device_code,
        user_code: auth.formatted_user_code,
        verification_uri: device_url,
        expires_in: auth.expires_in,
        interval: 5
      }
    end

    # GET /api/device_codes/:device_code
    # CLI polls for authorization status
    def show
      auth = DeviceAuthorization.find_by(device_code: params[:device_code])

      if auth.nil?
        render json: { error: "invalid_grant" }, status: :bad_request
        return
      end

      if auth.expired?
        auth.expire! if auth.pending?
        render json: { error: "expired_token" }, status: :bad_request
        return
      end

      case auth.status
      when "pending"
        render json: { error: "authorization_pending" }, status: :accepted
      when "approved"
        token = create_device_token(auth)
        render json: { access_token: token.token, token_type: "bearer" }
      when "denied"
        render json: { error: "access_denied" }, status: :bad_request
      else
        render json: { error: "invalid_grant" }, status: :bad_request
      end
    end

    private

    def create_device_token(auth)
      auth.user.device_tokens.create!(name: auth.device_name)
    end
  end
end
