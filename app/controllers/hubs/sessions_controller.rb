# frozen_string_literal: true

module Hubs
  # Displays terminal view for a specific session (identified by session_uuid).
  #
  # URL: /hubs/:hub_id/sessions/:session_uuid
  #
  # Replaces the old AgentsController + Agents::PtysController pair.
  # Session UUID is the primary key — no more agent_index/pty_index.
  class SessionsController < ApplicationController
    before_action :authenticate_user!
    before_action :set_hub
    before_action :set_session_uuid

    # GET /hubs/:hub_id/sessions/:session_uuid
    def show
    end

    private

    def set_hub
      Current.hub = current_user.hubs.find_by(id: params[:hub_id])

      unless Current.hub
        redirect_to hubs_path, alert: "Hub not found"
      end
    end

    def set_session_uuid
      Current.session_uuid = params[:uuid]
    end
  end
end
