# frozen_string_literal: true

class HubsController < ApplicationController
  include ApiKeyAuthenticatable

  before_action :authenticate_user!, only: [ :index, :show ]
  before_action :authenticate_hub_request!, only: [ :update, :destroy ]
  before_action :set_hub, only: [ :show, :destroy ]

  # GET /hubs
  # Dashboard showing list of hubs with health status
  def index
    @hubs = current_user.hubs.active.includes(:device, :hub_agents)
  end

  # GET /hubs/:identifier
  # Terminal view for a specific hub
  # URL fragment contains E2E key: /hubs/:identifier#key=...
  def show
    unless @hub
      redirect_to hubs_path, alert: "Hub not found"
      return
    end

    @browser_device = current_user.devices.browser_devices.order(last_seen_at: :desc).first
  end

  # PUT /hubs/:identifier
  # Upsert: create or update hub by identifier (CLI heartbeat)
  def update
    hub = current_hub_user.hubs.find_or_initialize_by(identifier: params[:identifier])
    hub.repo = params[:repo]
    hub.last_seen_at = Time.current

    # Associate with device if device_id provided
    if params[:device_id].present?
      device = current_hub_user.devices.find_by(id: params[:device_id])
      hub.device = device if device
    end

    if hub.save
      hub.sync_agents(params[:agents] || [])
      hub.broadcast_update!

      render json: { success: true, hub_id: hub.id, e2e_enabled: hub.e2e_enabled? }
    else
      render json: { error: hub.errors.full_messages.join(", ") }, status: :unprocessable_entity
    end
  end

  # DELETE /hubs/:identifier
  # Called when CLI shuts down gracefully
  def destroy
    if @hub
      @hub.broadcast_removal!
      @hub.destroy
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
    @hub = current_hub_user.hubs.find_by(identifier: params[:identifier])
  end
end
