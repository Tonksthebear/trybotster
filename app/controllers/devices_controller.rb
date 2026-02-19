# frozen_string_literal: true

# Manages device registration for E2E encrypted terminal access.
class DevicesController < ApplicationController
  include ApiKeyAuthenticatable

  # CLI uses Bearer token auth, browser uses session auth
  # CSRF is skipped for Bearer requests via ApplicationController
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
        created: device.previously_new_record?
      }, status: device.previously_new_record? ? :created : :ok
    else
      render json: { errors: device.errors.full_messages }, status: :unprocessable_entity
    end
  end

  # PATCH /devices/:id
  # Update device attributes (currently: notifications_enabled)
  def update
    device = current_device_user.devices.find(params[:id])
    device.update!(params.permit(:notifications_enabled))
    render json: device_json(device)
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
    if api_key_present?
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
      hubs_count: device.hubs.active.count,
      notifications_enabled: device.notifications_enabled
    }
  end
end
