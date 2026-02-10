# frozen_string_literal: true

module Hubs
  # Handles device authorization flow (RFC 8628).
  # CLI requests device code, polls for approval, receives token.
  class CodesController < ApplicationController
    skip_before_action :verify_authenticity_token

    # POST /hubs/codes
    # CLI requests a new device code to start auth flow
    def create
      auth = DeviceAuthorization.create!(device_name: params[:device_name])

      render json: {
        device_code: auth.device_code,
        user_code: auth.formatted_user_code,
        verification_uri: new_users_hub_url(code: auth.formatted_user_code),
        expires_in: auth.expires_in,
        interval: 5
      }
    end

    # GET /hubs/codes/:id
    # CLI polls for authorization status
    def show
      auth = DeviceAuthorization.find_by(device_code: params[:id])

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
        tokens = create_device_tokens(auth)
        render json: {
          access_token: tokens[:device_token].token,
          mcp_token: tokens[:mcp_token].token,
          token_type: "bearer"
        }
      when "denied"
        render json: { error: "access_denied" }, status: :bad_request
      else
        render json: { error: "invalid_grant" }, status: :bad_request
      end
    end

    private

    def create_device_tokens(auth)
      device = auth.user.devices.create!(
        name: auth.device_name,
        device_type: "cli",
        fingerprint: SecureRandom.hex(8).scan(/../).join(":")
      )

      {
        device_token: device.create_device_token!(name: auth.device_name),
        mcp_token: device.create_mcp_token!(name: "#{auth.device_name} MCP")
      }
    end
  end
end
