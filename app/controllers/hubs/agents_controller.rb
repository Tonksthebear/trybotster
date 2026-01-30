# frozen_string_literal: true

module Hubs
  # Agent resource - redirects to default PTY view.
  #
  # URL: /hubs/:hub_id/agents/:index
  # Redirects to: /hubs/:hub_id/agents/:index/ptys/0
  #
  # The actual terminal view is handled by Agents::PtysController.
  class AgentsController < ApplicationController
    before_action :authenticate_user!
    before_action :set_hub

    # GET /hubs/:hub_id/agents/:index
    # Redirects to PTY 0 (CLI terminal)
    def show
      redirect_to hub_agent_pty_path(Current.hub, params[:index], 0)
    end

    private

    def set_hub
      Current.hub = current_user.hubs.find_by(id: params[:hub_id])

      unless Current.hub
        redirect_to hubs_path, alert: "Hub not found"
      end
    end
  end
end
