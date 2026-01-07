# frozen_string_literal: true

class HubConnectionsController < ApplicationController
  before_action :authenticate_user!

  # GET /hub_connection
  # Secure E2E connection page - CLI public key is passed via URL fragment (#key=...)
  # The fragment is NEVER sent to the server, preventing MITM attacks.
  # JavaScript reads the key from window.location.hash and uses it directly.
  def show
    # Browser device for this session (for E2E encryption)
    @browser_device = current_user.devices.browser_devices.order(last_seen_at: :desc).first

    # No hub data fetched here - the hub identifier comes from the URL fragment
    # and is only visible to the browser JavaScript, not to the server.
  end

  # GET /hub_connection/new
  # Form to enter a connection code from the CLI
  def new
    # Just render the form
  end

  # POST /hub_connection
  # Look up a hub by connection code and redirect to it
  def create
    identifier = params[:code]&.strip

    if identifier.blank?
      flash.now[:alert] = "Please enter a connection code"
      render :new, status: :unprocessable_entity
      return
    end

    # Find the hub by identifier (must belong to current user)
    hub = current_user.hubs.find_by(identifier: identifier)

    if hub
      # Redirect to agents page where terminal access is available
      redirect_to agents_path(hub: hub.identifier), notice: "Hub found! Select it from the list to connect."
    else
      flash.now[:alert] = "Hub not found. Make sure you entered the code correctly and the CLI is running."
      render :new, status: :unprocessable_entity
    end
  end
end
