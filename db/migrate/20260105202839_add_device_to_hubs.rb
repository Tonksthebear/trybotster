class AddDeviceToHubs < ActiveRecord::Migration[8.1]
  def change
    # Allow null for existing hubs during migration period
    add_reference :hubs, :device, null: true, foreign_key: true
  end
end
