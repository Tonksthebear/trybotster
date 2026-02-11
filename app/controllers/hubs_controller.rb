# frozen_string_literal: true

class HubsController < ApplicationController
  include ApiKeyAuthenticatable

  before_action :authenticate_user!, only: [ :index, :show ]
  before_action :authenticate_hub_request!, only: [ :create, :update, :destroy ]
  before_action :set_hub, only: [ :show, :update, :destroy ]

  # GET /hubs
  # Dashboard showing list of hubs with health status
  def index
    @hubs = current_user.hubs.active.includes(:device, :hub_agents)
  end

  # GET /hubs/:id
  # Terminal view for a specific hub
  # URL fragment contains E2E key: /hubs/:id#<bundle>
  #
  # JSON format returns status info for browser-side error handling
  def show
    unless Current.hub
      respond_to do |format|
        format.html { redirect_to hubs_path, alert: "Hub not found" }
        format.json { render json: { error: "Hub not found" }, status: :not_found }
      end
      return
    end

    respond_to do |format|
      format.html do
        @browser_device = current_user.devices.browser_devices.order(last_seen_at: :desc).first
      end
      format.json do
        # Return hub status for browser-side error handling
        # Used by hub_connection_controller.js to show appropriate error messages
        render json: {
          id: Current.hub.id,
          identifier: Current.hub.identifier,
          active: Current.hub.active?,
          alive: Current.hub.alive?,
          last_seen_at: Current.hub.last_seen_at&.iso8601,
          seconds_since_heartbeat: Current.hub.last_seen_at ? (Time.current - Current.hub.last_seen_at).to_i : nil
        }
      end
    end
  end

  # POST /hubs
  # CLI registration: creates or finds hub and returns Rails ID
  # Called once at CLI startup before QR code generation
  def create
    hub = current_hub_user.hubs.find_or_initialize_by(identifier: params[:identifier])
    is_new = hub.new_record?
    hub.last_seen_at = Time.current
    hub.alive = true
    if params[:name].present?
      hub.name = params[:name]
    elsif params[:repo].present? && hub.read_attribute(:name).blank?
      hub.name = params[:repo]
    end

    if params[:device_id].present?
      device = current_hub_user.devices.find_by(id: params[:device_id])
      hub.device = device if device
    end

    if hub.save
      status = is_new ? :created : :ok
      render json: { id: hub.id, identifier: hub.identifier }, status: status
    else
      render json: { error: hub.errors.full_messages.join(", ") }, status: :unprocessable_entity
    end
  end

  # PUT /hubs/:id
  # CLI heartbeat: updates existing hub by Rails ID
  def update
    unless Current.hub
      render json: { error: "Hub not found" }, status: :not_found
      return
    end

    Current.hub.last_seen_at = Time.current
    Current.hub.alive = params.key?(:alive) ? ActiveModel::Type::Boolean.new.cast(params[:alive]) : true

    if params[:device_id].present?
      device = current_hub_user.devices.find_by(id: params[:device_id])
      Current.hub.device = device if device
    end

    if Current.hub.save
      Current.hub.sync_agents(params[:agents] || [])

      render json: { success: true, hub_id: Current.hub.id, e2e_enabled: Current.hub.e2e_enabled? }
    else
      render json: { error: Current.hub.errors.full_messages.join(", ") }, status: :unprocessable_entity
    end
  end

  # DELETE /hubs/:id
  # Destroys the hub record. Only called on CLI reset.
  # Normal shutdown uses PUT with alive: false instead.
  def destroy
    if Current.hub
      Current.hub.destroy!
      render json: { success: true }
    else
      render json: { success: true } # Idempotent - already gone is fine
    end
  end

  private

  def authenticate_hub_request!
    if api_key_present?
      authenticate_with_api_key!
    elsif request.format.json? || request.content_type&.include?("json")
      render json: { error: "API key required" }, status: :unauthorized
    else
      authenticate_user!
    end
  end

  def current_hub_user
    current_api_user || current_user
  end

  def set_hub
    Current.hub = current_hub_user.hubs.find_by(id: params[:id])
  end
end
