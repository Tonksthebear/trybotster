# frozen_string_literal: true

# Manages device registration for E2E encrypted terminal access.
#
# Two modes:
# 1. Secure mode (default): public_key is NOT stored on server.
#    Key exchange happens via QR code URL fragment (MITM-proof).
#
# 2. Convenience mode: public_key IS stored on server.
#    Browser can fetch key from API for easier pairing (potential MITM).
class DevicesController < ApplicationController
  include ApiKeyAuthenticatable

  skip_before_action :verify_authenticity_token

  # CLI uses API key auth, browser uses session auth
  before_action :authenticate_device_request!

  # GET /devices
  # List all devices for the current user (for key exchange)
  def index
    devices = current_device_user.devices.by_last_seen

    render json: devices.map { |d| device_json(d) }
  end

  # POST /devices
  # Register a new device (CLI or browser)
  def create
    fingerprint = params[:fingerprint]
    public_key = params[:public_key]

    # Browser devices always send public_key (they need it for key exchange)
    # CLI devices only send it if server_assisted_pairing is enabled
    if params[:device_type] == "browser"
      device = current_device_user.devices.find_or_initialize_by(public_key: public_key)
    elsif fingerprint.present?
      device = current_device_user.devices.find_or_initialize_by(fingerprint: fingerprint)
    elsif public_key.present?
      device = current_device_user.devices.find_or_initialize_by(public_key: public_key)
    else
      render json: { error: "Either fingerprint or public_key is required" }, status: :bad_request
      return
    end

    device.assign_attributes(
      device_type: params.require(:device_type),
      name: params.require(:name),
      public_key: public_key,
      fingerprint: fingerprint.presence || device.fingerprint,
      last_seen_at: Time.current
    )

    if device.save
      render json: {
        device_id: device.id,
        fingerprint: device.fingerprint,
        created: device.previously_new_record?,
        server_assisted_pairing: public_key.present?
      }, status: device.previously_new_record? ? :created : :ok
    else
      render json: { errors: device.errors.full_messages }, status: :unprocessable_entity
    end
  end

  # DELETE /devices/:id
  # Remove a device (and revoke its access)
  def destroy
    device = current_device_user.devices.find(params[:id])
    device.destroy!

    head :no_content
  end

  private

  def authenticate_device_request!
    if request.headers["X-API-Key"].present?
      authenticate_with_api_key!
    else
      authenticate_user!
    end
  end

  def current_device_user
    current_api_user || current_user
  end

  def device_json(device)
    {
      id: device.id,
      name: device.name,
      device_type: device.device_type,
      public_key: device.public_key,
      fingerprint: device.fingerprint,
      last_seen_at: device.last_seen_at,
      active: device.active?,
      hubs_count: device.hubs.active.count
    }
  end
end
