# frozen_string_literal: true

module Api
  # Manages hub registrations from CLI instances and browser connections.
  # Hubs are CLI instances that run agents and relay terminal data.
  class HubsController < ApplicationController
    include ApiKeyAuthenticatable

    skip_before_action :verify_authenticity_token

    # CLI uses API key, browser uses session
    before_action :authenticate_hub_request!
    before_action :set_hub, only: [ :show ]
    before_action :set_hub_for_destroy, only: [ :destroy ]

    # GET /api/hubs
    # List active hubs for the current user (browser)
    def index
      hubs = current_hub_user.hubs.active.includes(:device, :hub_agents)

      render json: hubs.map { |hub| hub_json(hub) }
    end

    # GET /api/hubs/:identifier
    # Get hub details (browser)
    def show
      render json: hub_json(@hub, include_agents: true)
    end

    # PUT /api/hubs/:identifier
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
        sync_hub_agents(hub, params[:agents] || [])
        broadcast_hub_update(hub)

        render json: { success: true, hub_id: hub.id, e2e_enabled: hub.e2e_enabled? }
      else
        render json: { error: hub.errors.full_messages.join(", ") }, status: :unprocessable_entity
      end
    end

    # DELETE /api/hubs/:identifier
    # Called when CLI shuts down gracefully
    def destroy
      if @hub
        @hub.destroy
        broadcast_hub_removal(@hub)
        render json: { success: true }
      else
        render json: { success: true } # Idempotent - already gone is fine
      end
    end

    private

    def authenticate_hub_request!
      if request.headers["X-API-Key"].present?
        authenticate_with_api_key!
      elsif request.format.json? || request.content_type&.include?("json")
        # JSON requests without API key should get 401, not redirect
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
      render json: { error: "Hub not found" }, status: :not_found unless @hub
    end

    # For destroy - don't error if not found (idempotent delete)
    def set_hub_for_destroy
      @hub = current_hub_user.hubs.find_by(identifier: params[:identifier])
    end

    def hub_json(hub, include_agents: false)
      json = {
        id: hub.id,
        identifier: hub.identifier,
        repo: hub.repo,
        last_seen_at: hub.last_seen_at,
        e2e_enabled: hub.e2e_enabled?,
        agents_count: hub.hub_agents.count,
        device: hub.device ? {
          id: hub.device.id,
          name: hub.device.name,
          fingerprint: hub.device.fingerprint,
          active: hub.device.active?
        } : nil
      }

      if include_agents
        json[:agents] = hub.hub_agents.map do |agent|
          {
            id: agent.id,
            session_key: agent.session_key,
            tunnel_status: agent.tunnel_status,
            tunnel_port: agent.tunnel_port,
            last_invocation_url: agent.last_invocation_url
          }
        end
      end

      json
    end

    def sync_hub_agents(hub, agents_data)
      agents_array = agents_data.is_a?(ActionController::Parameters) ? agents_data.values : agents_data
      session_keys = agents_array.map { |a| a[:session_key] || a["session_key"] }.compact

      hub.hub_agents.where.not(session_key: session_keys).destroy_all

      agents_array.each do |agent_data|
        session_key = agent_data[:session_key] || agent_data["session_key"]
        next if session_key.blank?

        agent = hub.hub_agents.find_or_initialize_by(session_key: session_key)
        agent.last_invocation_url = agent_data[:last_invocation_url] || agent_data["last_invocation_url"]
        agent.save!
      end
    end

    def broadcast_hub_update(hub)
      Turbo::StreamsChannel.broadcast_update_to(
        "user_#{hub.user_id}_hubs",
        target: "hubs_list",
        partial: "hubs/list",
        locals: { hubs: hub.user.hubs.active.includes(:hub_agents) }
      )
    rescue => e
      Rails.logger.warn "Failed to broadcast hub update: #{e.message}"
    end

    def broadcast_hub_removal(hub)
      Turbo::StreamsChannel.broadcast_update_to(
        "user_#{hub.user_id}_hubs",
        target: "hubs_list",
        partial: "hubs/list",
        locals: { hubs: current_hub_user.hubs.active.includes(:hub_agents) }
      )
    rescue => e
      Rails.logger.warn "Failed to broadcast hub removal: #{e.message}"
    end
  end
end
