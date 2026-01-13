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
  def show
    unless @hub
      redirect_to hubs_path, alert: "Hub not found"
      return
    end

    @browser_device = current_user.devices.browser_devices.order(last_seen_at: :desc).first
  end

  # POST /hubs
  # CLI registration: creates or finds hub and returns Rails ID
  # Called once at CLI startup before QR code generation
  def create
    hub = current_hub_user.hubs.find_or_initialize_by(identifier: params[:identifier])
    is_new = hub.new_record?
    hub.repo = params[:repo]
    hub.last_seen_at = Time.current
    hub.alive = true

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
    unless @hub
      render json: { error: "Hub not found" }, status: :not_found
      return
    end

    @hub.repo = params[:repo] if params[:repo].present?
    @hub.last_seen_at = Time.current
    @hub.alive = true

    if params[:device_id].present?
      device = current_hub_user.devices.find_by(id: params[:device_id])
      @hub.device = device if device
    end

    if @hub.save
      @hub.sync_agents(params[:agents] || [])
      @hub.broadcast_update!

      render json: { success: true, hub_id: @hub.id, e2e_enabled: @hub.e2e_enabled? }
    else
      render json: { error: @hub.errors.full_messages.join(", ") }, status: :unprocessable_entity
    end
  end

  # DELETE /hubs/:id
  # Called when CLI shuts down gracefully
  # Sets alive=false to mark offline while preserving hub ID for reconnection.
  # Browser sessions are tied to hub ID, so destroying breaks reconnection.
  def destroy
    if @hub
      @hub.broadcast_removal!
      @hub.update!(alive: false)
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
    @hub = current_hub_user.hubs.find_by(id: params[:id])
  end
end
