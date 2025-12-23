# frozen_string_literal: true

module Api
  # Manages hub registrations from CLI instances
  # Hubs are CLI instances that poll for messages and run agents
  class HubsController < ApplicationController
    include ApiKeyAuthenticatable

    skip_before_action :verify_authenticity_token
    before_action :authenticate_with_api_key!

    # PUT /api/hubs/:identifier
    # Upsert: create or update hub by identifier (heartbeat)
    def update
      hub = current_api_user.hubs.find_or_initialize_by(identifier: params[:identifier])
      hub.repo = params[:repo]
      hub.last_seen_at = Time.current

      if hub.save
        # Sync agents
        sync_hub_agents(hub, params[:agents] || [])

        # Broadcast update via Turbo Stream
        broadcast_hub_update(hub)

        render json: { success: true, hub_id: hub.id }
      else
        render json: { error: hub.errors.full_messages.join(", ") }, status: :unprocessable_entity
      end
    end

    # DELETE /api/hubs/:identifier
    # Called when CLI shuts down gracefully
    def destroy
      hub = current_api_user.hubs.find_by(identifier: params[:identifier])

      if hub
        hub.destroy
        broadcast_hub_removal(hub)
        render json: { success: true }
      else
        render json: { success: true } # Idempotent - already gone is fine
      end
    end

    private

    def sync_hub_agents(hub, agents_data)
      # Normalize agent data (handle both array of hashes and Rails strong params)
      agents_array = agents_data.is_a?(ActionController::Parameters) ? agents_data.values : agents_data
      session_keys = agents_array.map { |a| a[:session_key] || a["session_key"] }.compact

      # Remove agents no longer running
      hub.hub_agents.where.not(session_key: session_keys).destroy_all

      # Upsert current agents
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
        locals: { hubs: current_api_user.hubs.active.includes(:hub_agents) }
      )
    rescue => e
      Rails.logger.warn "Failed to broadcast hub removal: #{e.message}"
    end
  end
end
