# frozen_string_literal: true

class AgentsController < ApplicationController
  before_action :authenticate_user!

  # GET /agents
  # Dashboard showing running agents with WebRTC P2P connection
  def index
    # Any active WebRTC sessions for this user (for reconnection)
    @active_sessions = current_user.webrtc_sessions.active.order(created_at: :desc)
  end
end
