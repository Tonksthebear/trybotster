# frozen_string_literal: true

module Settings
  class DevicesController < ApplicationController
    before_action :authenticate_user!

    # GET /settings/devices
    def index
      @devices = current_user.devices.cli_devices.includes(:hubs).by_last_seen
    end

    # GET /settings/devices/:id
    def show
      @device = current_user.devices.includes(:hubs).find(params[:id])
      @source_device = current_user.devices.cli_devices.with_notifications.where.not(id: @device.id).first
    end
  end
end
