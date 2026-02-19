class AddNotificationsEnabledToDevices < ActiveRecord::Migration[8.1]
  def change
    add_column :devices, :notifications_enabled, :boolean, default: false, null: false
  end
end
