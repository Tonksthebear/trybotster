# frozen_string_literal: true

class NotificationsController < ApplicationController
  # GET /notifications
  # Serves the page shell. Notification data is rendered client-side from IndexedDB.
  # Device list is server-rendered for push notification management.
  def index
    @cli_devices = current_user.devices.cli_devices.includes(:hubs).by_last_seen
    @source_device = @cli_devices.find(&:notifications_enabled?)
  end
end
