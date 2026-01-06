# frozen_string_literal: true

class AgentsController < ApplicationController
  before_action :authenticate_user!

  # GET /agents
  # Dashboard showing running agents with E2E encrypted terminal access via Action Cable
  def index
    # Active hubs (CLI instances) for this user
    @hubs = current_user.hubs.active.includes(:device, :hub_agents)

    # Browser device for this session (for E2E encryption)
    @browser_device = current_user.devices.browser_devices.order(last_seen_at: :desc).first

    # If a hub identifier is provided via query param, pass it to the view for auto-connect
    @auto_connect_hub = params[:hub]
  end

  # GET /agents/connect
  # Secure E2E connection page - CLI public key is passed via URL fragment (#key=...)
  # The fragment is NEVER sent to the server, preventing MITM attacks.
  # JavaScript reads the key from window.location.hash and uses it directly.
  def connect
    # Browser device for this session (for E2E encryption)
    @browser_device = current_user.devices.browser_devices.order(last_seen_at: :desc).first

    # No hub data fetched here - the hub identifier comes from the URL fragment
    # and is only visible to the browser JavaScript, not to the server.
  end
end
