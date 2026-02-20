# frozen_string_literal: true

module Hubs
  class DeviceController < ApplicationController
    before_action :authenticate_user!
    before_action :set_hub

    # GET /hubs/:hub_id/device
    def show
      @device = Current.hub.device
      redirect_to hub_path(Current.hub), alert: "No device associated with this hub" unless @device

      @active_hubs = @device.hubs.select(&:active?)
      @source_device = current_user.devices.cli_devices.with_notifications.where.not(id: @device.id).first
    end

    private

    def set_hub
      Current.hub = current_user.hubs.find_by(id: params[:hub_id])
      redirect_to hubs_path, alert: "Hub not found" unless Current.hub
    end
  end
end
