# frozen_string_literal: true

module Hubs
  module Agents
    # Displays terminal view for a specific PTY.
    #
    # URL: /hubs/:hub_id/agents/:agent_index/ptys/:index
    #
    # PTY indices:
    # - 0: CLI PTY (Claude agent terminal)
    # - 1: Server PTY (development server)
    class PtysController < ApplicationController
      before_action :authenticate_user!
      before_action :set_hub
      before_action :set_agent_index
      before_action :set_pty_index

      # GET /hubs/:hub_id/agents/:agent_index/ptys/:index
      def show
        @browser_device = current_user.devices.browser_devices.order(last_seen_at: :desc).first
        render "hubs/agents/show"
      end

      private

      def set_hub
        Current.hub = current_user.hubs.find_by(id: params[:hub_id])

        unless Current.hub
          redirect_to hubs_path, alert: "Hub not found"
        end
      end

      def set_agent_index
        Current.agent_index = params[:agent_index].to_i
      end

      def set_pty_index
        Current.pty_index = params[:index].to_i
      end
    end
  end
end
