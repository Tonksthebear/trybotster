# frozen_string_literal: true

module Hubs
  # Displays terminal view for a specific agent by index.
  #
  # URL: /hubs/:hub_id/agents/:index
  #
  # The agent index corresponds to the position in the CLI's agent list.
  # Note: indices can shift if agents are removed mid-session.
  class AgentsController < ApplicationController
    before_action :authenticate_user!
    before_action :set_hub
    before_action :set_agent_index

    # GET /hubs/:hub_id/agents/:index
    # Terminal view for a specific agent
    def show
      @browser_device = current_user.devices.browser_devices.order(last_seen_at: :desc).first
    end

    private

    def set_hub
      @hub = current_user.hubs.find_by(id: params[:hub_id])

      unless @hub
        redirect_to hubs_path, alert: "Hub not found"
      end
    end

    def set_agent_index
      @agent_index = params[:index].to_i

      # Validate agent index against known agents (if hub has agent info)
      # Note: Agent list is dynamic; this is a soft validation
      if @hub.hub_agents.any? && @agent_index >= @hub.hub_agents.count
        redirect_to hub_path(@hub), alert: "Agent not found"
        return
      end
    end
  end
end
