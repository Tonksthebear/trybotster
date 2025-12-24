# frozen_string_literal: true

class HubsController < ApplicationController
  before_action :authenticate_user!

  # GET /hubs
  # Dashboard showing active CLI hubs with live updates via Turbo Streams
  def index
    @hubs = current_user.hubs.active.includes(:hub_agents)
  end
end
