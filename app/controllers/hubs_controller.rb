# frozen_string_literal: true

class HubsController < ApplicationController
  before_action :authenticate_user!

  # GET /hubs
  # Dashboard showing active CLI hubs with live updates via Turbo Streams
  def index
    @hubs = current_user.hubs.active.includes(:hub_agents)
  end

  # GET /connect
  # Form to enter a connection code from the CLI
  def connect
    # Just render the form
  end

  # POST /connect
  # Look up a hub by connection code and redirect to it
  def lookup
    identifier = params[:code]&.strip

    if identifier.blank?
      flash.now[:alert] = "Please enter a connection code"
      render :connect, status: :unprocessable_entity
      return
    end

    # Find the hub by identifier (must belong to current user)
    hub = current_user.hubs.find_by(identifier: identifier)

    if hub
      # Redirect to agents page where terminal access is available
      redirect_to agents_path(hub: hub.identifier), notice: "Hub found! Select it from the list to connect."
    else
      flash.now[:alert] = "Hub not found. Make sure you entered the code correctly and the CLI is running."
      render :connect, status: :unprocessable_entity
    end
  end
end
