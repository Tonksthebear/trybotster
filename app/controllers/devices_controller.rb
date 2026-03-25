# frozen_string_literal: true

# Manages browser key registration for E2E encrypted terminal access.
#
# POST /devices (device_type: "browser") → creates BrowserKey
# GET /devices → returns browser keys for key exchange
class DevicesController < ApplicationController
  include ApiKeyAuthenticatable

  # CLI uses Bearer token auth, browser uses session auth
  # CSRF is skipped for Bearer requests via ApplicationController
  before_action :authenticate_device_request!

  # GET /devices
  # List all browser keys for the current user (for key exchange)
  def index
    browser_keys = current_device_user.browser_keys.by_last_seen

    render json: browser_keys.map { |bk| browser_key_json(bk) }
  end

  # POST /devices
  # Register a browser key (browser E2E key exchange)
  def create
    unless params[:device_type] == "browser"
      render json: { error: "Only browser device_type is supported" }, status: :bad_request
      return
    end

    public_key = params[:public_key]
    unless public_key.present?
      render json: { error: "public_key is required" }, status: :bad_request
      return
    end

    browser_key = current_device_user.browser_keys.find_or_initialize_by(public_key: public_key)
    browser_key.assign_attributes(
      name: params.require(:name),
      last_seen_at: Time.current
    )

    if browser_key.save
      render json: {
        device_id: browser_key.id,
        fingerprint: browser_key.fingerprint,
        created: browser_key.previously_new_record?
      }, status: browser_key.previously_new_record? ? :created : :ok
    else
      render json: { errors: browser_key.errors.full_messages }, status: :unprocessable_entity
    end
  end

  # DELETE /devices/:id
  # Remove a browser key
  def destroy
    browser_key = current_device_user.browser_keys.find(params[:id])
    browser_key.destroy!

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

  def browser_key_json(bk)
    {
      id: bk.id,
      name: bk.name,
      device_type: "browser",
      public_key: bk.public_key,
      fingerprint: bk.fingerprint,
      last_seen_at: bk.last_seen_at,
      active: bk.active?
    }
  end
end
