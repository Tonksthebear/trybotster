# frozen_string_literal: true

module Hubs
  # Handles hub authorization flow (RFC 8628).
  # CLI requests device code, polls for approval, receives token.
  class CodesController < ApplicationController
    skip_before_action :verify_authenticity_token

    # POST /hubs/codes
    # CLI requests a new device code to start auth flow
    def create
      auth = HubAuthorization.create!(
        device_name: params[:device_name],
        fingerprint: params[:fingerprint]
      )

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
      auth = HubAuthorization.find_by(device_code: params[:id])

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
        tokens = create_hub_tokens(auth)
        render json: {
          access_token: tokens[:hub_token].token,
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

    def create_hub_tokens(auth)
      fingerprint = auth.fingerprint.presence || SecureRandom.hex(8).scan(/../).join(":")

      # Find or create hub by fingerprint
      hub = auth.user.hubs.find_or_initialize_by(fingerprint: fingerprint)
      hub.update!(
        name: auth.device_name,
        identifier: hub.identifier.presence || SecureRandom.hex(16),
        last_seen_at: Time.current,
        alive: false
      )

      hub_token = hub.hub_token || hub.create_hub_token!
      mcp_token = hub.mcp_token || hub.create_mcp_token!(name: "#{auth.device_name} MCP")

      { hub_token: hub_token, mcp_token: mcp_token }
    end
  end
end
